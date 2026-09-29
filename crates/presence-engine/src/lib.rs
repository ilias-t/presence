//! Audio engine for presence.
//!
//! Captures a microphone, runs [`presence_dsp::Processor`] on it, and plays the
//! result into an output device — normally a virtual loopback device such as
//! VB-Cable or BlackHole, which call apps then select as their microphone.
//!
//! Threading: the [`Engine`] handle lives on the UI thread; audio runs on the
//! CoreAudio callback threads. Settings cross over lock-free; meters and
//! analysis samples come back lock-free.
//!
//! Signal path: the input callback takes channel 0 of the mic, runs the DSP and
//! pushes mono samples into an SPSC ring. The output callback drains the ring
//! through an adaptive-ratio cubic resampler that steers the ring fill toward a
//! small target, absorbing both nominal rate differences and the drift between
//! the two devices' independent clocks.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use cpal::traits::{DeviceTrait, StreamTrait};
use presence_dsp::{EqSettings, Processor};

mod device;
#[cfg(target_os = "macos")]
mod hal;
mod link;
mod meter;
mod resampler;

use device::{DeviceCaps, Direction};
use link::Drain;
use meter::{Ballistics, MeterTap};

/// Name fragments of virtual loopback devices we can output to, in preference order.
pub const KNOWN_VIRTUAL_DEVICES: &[&str] = &["presence", "BlackHole", "VB-Cable", "Loopback"];

/// Fallback sample rate: tried on the input after its current nominal rate
/// (which is kept when possible, since changing it affects every app using the
/// mic), and on the output after the input's rate.
pub const PREFERRED_SAMPLE_RATE: u32 = 48_000;

/// IO buffer size requested from each device (clamped into the device's
/// supported range; the device default is used if a fixed size is refused).
/// 256 frames is 5.3 ms at 48 kHz.
pub const PREFERRED_BUFFER_FRAMES: u32 = 256;

/// Capacity of the analysis ring drained by [`Engine::drain_analysis`]
/// (≈ 340 ms at 48 kHz).
pub const ANALYSIS_CAPACITY: usize = 1 << 14;

/// Input → output ring capacity. Far above the steering target (a few hundred
/// samples) so the producer essentially never finds it full; excess latency is
/// shed by the consumer's resync instead.
const AUDIO_RING_CAPACITY: usize = 1 << 16;

/// Largest block the input callback processes in one go; bigger callbacks are
/// split. Sized so the scratch buffer is allocated once, up front.
const MAX_CHUNK: usize = 4096;

/// Length of the dry/wet crossfade when bypass toggles.
const BYPASS_FADE_SECS: f32 = 0.02;

/// Data callbacks run by every engine in this process, both directions.
static CALLBACKS: AtomicU64 = AtomicU64::new(0);

/// Total audio data callbacks run by all engines in this process so far.
///
/// Diagnostics: a value that keeps rising after the last [`Engine`] was
/// dropped means a stream is still running.
pub fn audio_callback_count() -> u64 {
    CALLBACKS.load(Ordering::Relaxed)
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("audio device not found: {0}")]
    DeviceNotFound(String),
    #[error("no input devices found")]
    NoDefaultInput,
    /// No input was named and every input is a virtual loopback device (see
    /// [`auto_input_device`]).
    #[error(
        "no microphone found: every input is a virtual loopback device, and capturing one \
         could feed presence its own output (name one explicitly to use it anyway)"
    )]
    NoMicrophone,
    #[error(
        "no output device given and no known virtual device found (install BlackHole or VB-Cable)"
    )]
    NoVirtualOutput,
    #[error("unsupported device configuration: {0}")]
    UnsupportedConfig(String),
    #[error("audio backend error: {0}")]
    Backend(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub default_sample_rate: u32,
}

pub fn list_input_devices() -> Result<Vec<DeviceInfo>, EngineError> {
    device::list(Direction::Input)
}

pub fn list_output_devices() -> Result<Vec<DeviceInfo>, EngineError> {
    device::list(Direction::Output)
}

/// Whether `name` looks like a virtual loopback device: it contains a
/// [`KNOWN_VIRTUAL_DEVICES`] entry, ignoring case.
pub fn is_virtual_device(name: &str) -> bool {
    let name = name.to_lowercase();
    KNOWN_VIRTUAL_DEVICES
        .iter()
        .any(|fragment| name.contains(&fragment.to_lowercase()))
}

/// First output device whose name contains a [`KNOWN_VIRTUAL_DEVICES`] entry.
///
/// Matching is case-insensitive (see [`is_virtual_device`]), and the list
/// order is a preference: a BlackHole device is chosen over VB-Cable
/// regardless of enumeration order.
pub fn find_virtual_output() -> Result<Option<DeviceInfo>, EngineError> {
    let mut outputs = list_output_devices()?;
    outputs.retain(|d| is_virtual_device(&d.name));
    let names: Vec<&str> = outputs.iter().map(|d| d.name.as_str()).collect();
    Ok(device::pick_virtual(&names).map(|i| outputs.swap_remove(i)))
}

/// The input [`Engine::start`] captures when [`EngineConfig::input_device`] is
/// `None`: the system default input unless it's a virtual loopback device
/// ([`is_virtual_device`]), in which case the built-in microphone, else the
/// first other non-virtual input. `None` if every input is virtual.
///
/// Virtual devices are skipped because the usual setup makes VB-Cable or
/// BlackHole the system default input (so call apps follow it), and capturing
/// the device presence plays into would loop its output back into itself.
pub fn auto_input_device() -> Result<Option<DeviceInfo>, EngineError> {
    let inputs = device::caps(Direction::Input)?;
    Ok(device::pick_auto_input(&inputs).map(|i| inputs[i].info()))
}

/// Microphone (TCC) authorization for this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicPermission {
    /// Never asked. [`Engine::start`] will trigger the system prompt and block
    /// until the user answers it — show a hint in the UI before calling it.
    NotDetermined,
    /// Blocked by policy (MDM / parental controls).
    Restricted,
    /// The user declined; capture would only deliver silence.
    Denied,
    Authorized,
}

/// Current microphone permission, without prompting or blocking. On macOS the
/// grant belongs to the "responsible" app — the terminal, for a CLI.
pub fn microphone_permission() -> MicPermission {
    #[cfg(target_os = "macos")]
    {
        hal::mic_permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicPermission::Authorized
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Device name (exact, else case-insensitive, else a unique
    /// case-insensitive substring); `None` = [`auto_input_device`], which
    /// never picks a virtual loopback device.
    pub input_device: Option<String>,
    /// Device name, matched like `input_device`; `None` = [`find_virtual_output`].
    pub output_device: Option<String>,
    pub settings: EqSettings,
    pub bypass: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meters {
    pub input_peak_db: f32,
    pub input_rms_db: f32,
    pub output_peak_db: f32,
    pub output_rms_db: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineStatus {
    Running,
    /// A stream reported an error (e.g. device unplugged). The engine must be restarted.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineStats {
    pub input_device: String,
    pub output_device: String,
    pub input_sample_rate: u32,
    pub output_sample_rate: u32,
    /// Estimated added latency (capture buffer + ring fill + output buffer).
    pub latency_ms: f32,
    pub underruns: u64,
    pub overruns: u64,
}

/// State shared between the UI thread and both audio callbacks. Everything the
/// callbacks touch is an atomic; the one mutex is only used from the error
/// callback (never the data path) and the UI.
struct Shared {
    bypass: AtomicBool,
    failed: AtomicBool,
    failure: Mutex<Option<String>>,
    input_meter: MeterTap,
    output_meter: MeterTap,
    underruns: AtomicU64,
    overruns: AtomicU64,
    /// Largest callback sizes seen, in frames.
    in_block: AtomicU32,
    out_block: AtomicU32,
    /// Smoothed ring fill in input samples, f32 bits.
    fill: AtomicU32,
    /// Time of the input side's last push, ns since `epoch`; 0 = never.
    last_push_ns: AtomicU64,
    /// Input data callbacks run so far.
    input_callbacks: AtomicU64,
    epoch: Instant,
}

impl Shared {
    fn new(bypass: bool) -> Self {
        Self {
            bypass: AtomicBool::new(bypass),
            failed: AtomicBool::new(false),
            failure: Mutex::new(None),
            input_meter: MeterTap::default(),
            output_meter: MeterTap::default(),
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            in_block: AtomicU32::new(0),
            out_block: AtomicU32::new(0),
            fill: AtomicU32::new(0),
            last_push_ns: AtomicU64::new(0),
            input_callbacks: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    fn now_ns(&self) -> u64 {
        // Instant::now is a mach_absolute_time read on macOS: no syscall, RT-safe.
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Called from cpal's error callback, which can run on a render thread.
    /// `try_lock` so it never blocks there; the first message wins.
    fn fail(&self, stream: &str, err: &cpal::StreamError) {
        if let Ok(mut failure) = self.failure.try_lock()
            && failure.is_none()
        {
            *failure = Some(format!("{stream} stream: {err}"));
        }
        self.failed.store(true, Ordering::Release);
    }
}

/// Moves callback state into a cpal closure. cpal consumes the closure even
/// when building the stream fails, so the state waits in this slot and the
/// callback claims it on its first run; a failed attempt leaves it in place for
/// the next config we try. The callback only `try_lock`s until it succeeds.
struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T: Send + 'static> Handoff<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    /// Mutate the waiting state between attempts. `None` if a callback already
    /// claimed it (a failed attempt that nevertheless ran).
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .map(f)
    }

    fn claimer(&self) -> impl FnMut() -> Option<T> + Send + 'static {
        let slot = Arc::clone(&self.0);
        move || slot.try_lock().ok().and_then(|mut s| s.take())
    }
}

/// Everything the input callback owns.
struct InputSide {
    processor: Processor,
    sample_rate: u32,
    settings: triple_buffer::Output<EqSettings>,
    /// Current wet (processed) share of the output, 0..=1; ramps toward the
    /// bypass target so toggling bypass crossfades instead of clicking.
    wet: f32,
    channels: usize,
    scratch: Vec<f32>,
    /// Copy of the unprocessed block for the crossfade.
    dry: Vec<f32>,
    audio_tx: rtrb::Producer<f32>,
    analysis_tx: rtrb::Producer<f32>,
    shared: Arc<Shared>,
}

impl InputSide {
    fn process(&mut self, data: &[f32]) {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        let channels = self.channels.max(1);
        let shared = &*self.shared;
        shared.input_callbacks.fetch_add(1, Ordering::Relaxed);
        let frames = data.len() / channels;
        shared.in_block.fetch_max(frames as u32, Ordering::Relaxed);

        // Apply settings even while bypassed so they're current on re-enable.
        if self.settings.update() {
            self.processor.set_settings(self.settings.output_buffer());
        }
        let bypass = shared.bypass.load(Ordering::Relaxed);
        let target = if bypass { 0.0 } else { 1.0 };
        let fade_step = 1.0 / (BYPASS_FADE_SECS * self.sample_rate.max(1) as f32);

        let mut overrun = false;
        for chunk in data.chunks(channels * MAX_CHUNK) {
            let n = chunk.len() / channels;
            let (Some(buf), Some(dry)) = (self.scratch.get_mut(..n), self.dry.get_mut(..n)) else {
                continue;
            };
            // Mono = channel 0. Averaging would comb-filter spaced mic arrays
            // and drop 3 dB of level on mono-in-stereo interfaces; channel 0 is
            // the primary capsule on every Mac and single-mic USB device.
            for (dst, frame) in buf.iter_mut().zip(chunk.chunks_exact(channels)) {
                *dst = frame.first().copied().unwrap_or(0.0);
            }
            shared.input_meter.record(buf);
            // Fully bypassed (wet share 0 and staying there) skips the DSP.
            if self.wet > 0.0 || !bypass {
                if self.wet != target {
                    dry.copy_from_slice(buf);
                }
                self.processor.process(buf);
                if self.wet != target {
                    for (y, &d) in buf.iter_mut().zip(dry.iter()) {
                        self.wet = if target > self.wet {
                            (self.wet + fade_step).min(target)
                        } else {
                            (self.wet - fade_step).max(target)
                        };
                        *y = d + self.wet * (*y - d);
                    }
                }
                if self.wet == 0.0 {
                    // The fade-out just finished, so the wet path is silent:
                    // clearing its state now is inaudible, and re-enabling
                    // later fades in from a clean filter instead of one frozen
                    // mid-signal.
                    self.processor.reset();
                }
            }
            shared.output_meter.record(buf);
            // Analysis is best-effort: whatever doesn't fit is dropped.
            link::push_block(&mut self.analysis_tx, buf);
            overrun |= link::push_block(&mut self.audio_tx, buf);
        }
        shared
            .last_push_ns
            .store(shared.now_ns().max(1), Ordering::Release);
        if overrun {
            shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Everything the output callback owns.
struct OutputSide {
    drain: Drain,
    in_rate: f64,
    channels: usize,
    shared: Arc<Shared>,
}

impl OutputSide {
    fn render(&mut self, data: &mut [f32]) {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        let shared = &*self.shared;
        let channels = self.channels.max(1);
        shared
            .out_block
            .fetch_max((data.len() / channels) as u32, Ordering::Relaxed);
        let in_block = shared.in_block.load(Ordering::Relaxed) as usize;
        let last_push = shared.last_push_ns.load(Ordering::Acquire);
        let in_phase = if last_push == 0 {
            0.0
        } else {
            shared.now_ns().saturating_sub(last_push) as f64 * 1e-9 * self.in_rate
        };
        let report = self.drain.render(data, channels, in_block, in_phase);
        if report.underrun {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        if report.overrun {
            shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
        shared
            .fill
            .store((report.fill as f32).to_bits(), Ordering::Relaxed);
    }
}

struct MeterReadout {
    input: Ballistics,
    output: Ballistics,
    last: Instant,
}

struct StreamInfo {
    input_device: String,
    output_device: String,
    input_rate: u32,
    output_rate: u32,
}

pub struct Engine {
    // `Drop` pauses both streams first; dropping them (fields drop in
    // declaration order, streams first) then disposes the audio units before
    // anything the callbacks share is torn down.
    input: cpal::Stream,
    output: cpal::Stream,
    shared: Arc<Shared>,
    settings: Mutex<triple_buffer::Input<EqSettings>>,
    analysis: Mutex<rtrb::Consumer<f32>>,
    readout: Mutex<MeterReadout>,
    info: StreamInfo,
}

impl Engine {
    /// Open both devices and start processing.
    ///
    /// Fails fast if microphone access is denied (CoreAudio would otherwise
    /// deliver silence without any error). If permission was never asked, this
    /// blocks while macOS shows its prompt; see [`microphone_permission`].
    pub fn start(config: EngineConfig) -> Result<Engine, EngineError> {
        match microphone_permission() {
            MicPermission::Denied | MicPermission::Restricted => {
                return Err(EngineError::Backend(
                    "microphone access denied; allow this app (your terminal, for the CLI) in \
                     System Settings > Privacy & Security > Microphone"
                        .into(),
                ));
            }
            MicPermission::NotDetermined | MicPermission::Authorized => {}
        }
        let (in_dev, in_caps) = device::open(Direction::Input, config.input_device.as_deref())?;
        let (out_dev, out_caps) = match config.output_device.as_deref() {
            Some(name) => device::open(Direction::Output, Some(name))?,
            None => {
                let found = find_virtual_output()?.ok_or(EngineError::NoVirtualOutput)?;
                device::open(Direction::Output, Some(&found.name))?
            }
        };
        let (in_name, out_name) = (in_caps.name.clone(), out_caps.name.clone());
        // A loopback device captured and played into at once feeds itself;
        // the transport type also catches loopback drivers we don't know by name.
        if in_name == out_name
            && (is_virtual_device(&in_name)
                || in_caps.virtual_transport
                || out_caps.virtual_transport)
        {
            return Err(EngineError::UnsupportedConfig(format!(
                "input and output are both the loopback device {in_name:?}; that would feed back"
            )));
        }

        let shared = Arc::new(Shared::new(config.bypass));
        let (settings_tx, settings_rx) = triple_buffer::triple_buffer(&config.settings);
        let (audio_tx, audio_rx) = rtrb::RingBuffer::new(AUDIO_RING_CAPACITY);
        let (analysis_tx, analysis_rx) = rtrb::RingBuffer::new(ANALYSIS_CAPACITY);

        let (input, in_cfg) = open_input(
            &in_dev,
            &in_caps,
            &config,
            &shared,
            settings_rx,
            audio_tx,
            analysis_tx,
        )?;
        // cpal starts a stream as soon as it's built, so from here on every
        // error path must stop what's already running rather than rely on drop.
        let in_rate = in_cfg.sample_rate.0;
        let (output, out_cfg) = match open_output(&out_dev, &out_caps, in_rate, &shared, audio_rx) {
            Ok(opened) => opened,
            Err(e) => {
                pause(&input);
                return Err(e);
            }
        };
        for stream in [&input, &output] {
            if let Err(e) = stream.play() {
                pause(&input);
                pause(&output);
                return Err(EngineError::Backend(e.to_string()));
            }
        }
        // The input may have lapped the ring while the output was being opened;
        // that's startup, not a glitch worth reporting.
        shared.overruns.store(0, Ordering::Relaxed);
        shared.underruns.store(0, Ordering::Relaxed);

        Ok(Engine {
            input,
            output,
            shared,
            settings: Mutex::new(settings_tx),
            analysis: Mutex::new(analysis_rx),
            readout: Mutex::new(MeterReadout {
                input: Ballistics::default(),
                output: Ballistics::default(),
                last: Instant::now(),
            }),
            info: StreamInfo {
                input_device: in_name,
                output_device: out_name,
                input_rate: in_rate,
                output_rate: out_cfg.sample_rate.0,
            },
        })
    }

    /// Send new settings to the audio thread. Lock-free on the audio side.
    pub fn set_settings(&self, settings: EqSettings) {
        lock(&self.settings).write(settings);
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.shared.bypass.store(bypass, Ordering::Relaxed);
    }

    pub fn bypass(&self) -> bool {
        self.shared.bypass.load(Ordering::Relaxed)
    }

    /// Meters decay over time; call ~30 times per second.
    ///
    /// Input is pre-DSP; output is post-DSP (measured in the input callback on
    /// exactly the samples sent to the output ring and to [`Engine::drain_analysis`]).
    pub fn meters(&self) -> Meters {
        let mut readout = lock(&self.readout);
        let now = Instant::now();
        let dt = now.duration_since(readout.last).as_secs_f32();
        readout.last = now;
        let (ip, ims) = self.shared.input_meter.take();
        let (op, oms) = self.shared.output_meter.take();
        let (input_peak_db, input_rms_db) = readout.input.update(ip, ims, dt);
        let (output_peak_db, output_rms_db) = readout.output.update(op, oms, dt);
        Meters {
            input_peak_db,
            input_rms_db,
            output_peak_db,
            output_rms_db,
        }
    }

    pub fn status(&self) -> EngineStatus {
        if self.shared.failed.load(Ordering::Acquire) {
            let msg = lock(&self.shared.failure)
                .clone()
                .unwrap_or_else(|| "audio stream error".to_string());
            EngineStatus::Failed(msg)
        } else {
            EngineStatus::Running
        }
    }

    /// Input data callbacks this engine has run. A count that stays put while
    /// the engine is running means the microphone delivers nothing at all —
    /// no error, no samples (e.g. macOS is holding capture back until the
    /// terminal is granted microphone access).
    pub fn callback_count(&self) -> u64 {
        self.shared.input_callbacks.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> EngineStats {
        let s = &*self.shared;
        let in_rate = self.info.input_rate.max(1) as f32;
        let out_rate = self.info.output_rate.max(1) as f32;
        let in_block = s.in_block.load(Ordering::Relaxed) as f32;
        let out_block = s.out_block.load(Ordering::Relaxed) as f32;
        let fill = f32::from_bits(s.fill.load(Ordering::Relaxed));
        // Excludes the devices' own hardware/safety-offset latency, which cpal
        // doesn't expose.
        let latency_s = (in_block + fill) / in_rate + out_block / out_rate;
        EngineStats {
            input_device: self.info.input_device.clone(),
            output_device: self.info.output_device.clone(),
            input_sample_rate: self.info.input_rate,
            output_sample_rate: self.info.output_rate,
            latency_ms: latency_s * 1000.0,
            underruns: s.underruns.load(Ordering::Relaxed),
            overruns: s.overruns.load(Ordering::Relaxed),
        }
    }

    /// Append processed (post-EQ) mono samples captured since the last call,
    /// at `stats().input_sample_rate`, for a spectrum analyzer. Drops samples
    /// rather than blocking if the caller falls behind.
    pub fn drain_analysis(&self, out: &mut Vec<f32>) {
        let mut rx = lock(&self.analysis);
        let n = rx.slots();
        if let Ok(chunk) = rx.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out.reserve(a.len() + b.len());
            out.extend_from_slice(a);
            out.extend_from_slice(b);
            chunk.commit_all();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Stop the callbacks explicitly instead of trusting the backend's
        // stream destructor alone: cpal 0.15 leaked every non-default-device
        // stream through a reference cycle, leaving the audio running.
        pause(&self.input);
        pause(&self.output);
    }
}

/// Stop a stream's callbacks, best effort (there's nothing useful to do if the
/// backend refuses while we're tearing down).
fn pause(stream: &cpal::Stream) {
    let _ = stream.pause();
}

/// UI-side lock; a poisoned mutex only means a UI-thread panic mid-update of
/// plain data, so keep going with what's there.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn build_error(what: &str, errors: &[String]) -> EngineError {
    EngineError::UnsupportedConfig(format!("could not open {what}: {}", errors.join("; ")))
}

fn open_input(
    device: &cpal::Device,
    caps: &DeviceCaps,
    config: &EngineConfig,
    shared: &Arc<Shared>,
    settings_rx: triple_buffer::Output<EqSettings>,
    audio_tx: rtrb::Producer<f32>,
    analysis_tx: rtrb::Producer<f32>,
) -> Result<(cpal::Stream, cpal::StreamConfig), EngineError> {
    // cpal sets an input device's nominal rate to the stream's, which changes
    // it for every app using the mic; keep whatever it runs at now when we can.
    let rates = device::input_rate_candidates(&caps.rate_ranges, caps.default_rate);
    let buffers = device::buffer_candidates(caps.buffer_range, PREFERRED_BUFFER_FRAMES);
    let channels = caps.channels;
    let first_rate = rates.first().copied().unwrap_or(PREFERRED_SAMPLE_RATE);

    // Everything the callback needs is allocated here, before any stream runs.
    let side = Handoff::new(InputSide {
        processor: Processor::new(first_rate as f32, &config.settings),
        sample_rate: first_rate,
        settings: settings_rx,
        wet: if config.bypass { 0.0 } else { 1.0 },
        channels: usize::from(channels),
        scratch: vec![0.0; MAX_CHUNK],
        dry: vec![0.0; MAX_CHUNK],
        audio_tx,
        analysis_tx,
        shared: Arc::clone(shared),
    });

    let mut errors = Vec::new();
    for &rate in &rates {
        for &buffer_size in &buffers {
            side.with(|s| {
                if s.sample_rate != rate {
                    s.processor = Processor::new(rate as f32, &config.settings);
                    s.sample_rate = rate;
                }
            })
            .ok_or_else(|| build_error("input", &errors))?;
            let stream_config = cpal::StreamConfig {
                channels,
                sample_rate: cpal::SampleRate(rate),
                buffer_size,
            };
            let mut claim = side.claimer();
            let mut local: Option<InputSide> = None;
            let err_shared = Arc::clone(shared);
            let result = device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if local.is_none() {
                        local = claim();
                    }
                    if let Some(side) = local.as_mut() {
                        side.process(data);
                    }
                },
                move |err| err_shared.fail("input", &err),
                None,
            );
            match result {
                Ok(stream) => return Ok((stream, stream_config)),
                Err(e) => errors.push(format!("{rate} Hz/{buffer_size:?}: {e}")),
            }
        }
    }
    Err(build_error("input", &errors))
}

fn open_output(
    device: &cpal::Device,
    caps: &DeviceCaps,
    in_rate: u32,
    shared: &Arc<Shared>,
    audio_rx: rtrb::Consumer<f32>,
) -> Result<(cpal::Stream, cpal::StreamConfig), EngineError> {
    // Matching the input rate makes the nominal ratio 1.0. On macOS cpal
    // doesn't change the output device's rate; AUHAL converts from the stream
    // rate, so `output_sample_rate` is the stream's rate, not necessarily the
    // device's nominal one.
    let rates = device::rate_candidates(
        &caps.rate_ranges,
        &[in_rate, PREFERRED_SAMPLE_RATE],
        caps.default_rate,
    );
    let buffers = device::buffer_candidates(caps.buffer_range, PREFERRED_BUFFER_FRAMES);
    let channels = caps.channels;
    let first_rate = rates.first().copied().unwrap_or(PREFERRED_SAMPLE_RATE);

    let side = Handoff::new(OutputSide {
        drain: Drain::new(audio_rx, f64::from(in_rate), f64::from(first_rate)),
        in_rate: f64::from(in_rate),
        channels: usize::from(channels),
        shared: Arc::clone(shared),
    });

    let mut errors = Vec::new();
    for &rate in &rates {
        for &buffer_size in &buffers {
            side.with(|s| s.drain.set_rates(f64::from(in_rate), f64::from(rate)))
                .ok_or_else(|| build_error("output", &errors))?;
            let stream_config = cpal::StreamConfig {
                channels,
                sample_rate: cpal::SampleRate(rate),
                buffer_size,
            };
            let mut claim = side.claimer();
            let mut local: Option<OutputSide> = None;
            let err_shared = Arc::clone(shared);
            let result = device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if local.is_none() {
                        local = claim();
                    }
                    match local.as_mut() {
                        Some(side) => side.render(data),
                        None => data.fill(0.0),
                    }
                },
                move |err| err_shared.fail("output", &err),
                None,
            );
            match result {
                Ok(stream) => return Ok((stream, stream_config)),
                Err(e) => errors.push(format!("{rate} Hz/{buffer_size:?}: {e}")),
            }
        }
    }
    Err(build_error("output", &errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_devices_match_case_insensitively() {
        assert!(is_virtual_device("VB-Cable"));
        assert!(is_virtual_device("vb-cable"));
        assert!(is_virtual_device("BlackHole 2ch"));
        assert!(is_virtual_device("Rogue Amoeba LOOPBACK Audio"));
        assert!(!is_virtual_device("MacBook Pro Microphone"));
        assert!(!is_virtual_device("ZoomAudioDevice"));
    }

    const RATE: u32 = 48_000;
    /// 100 Hz: one period is 480 samples, so 120-frame blocks put every
    /// fourth block boundary on a sine peak, where a hard switch jumps most.
    const BLOCK: usize = 120;

    struct Harness {
        side: InputSide,
        out: rtrb::Consumer<f32>,
        _settings: triple_buffer::Input<EqSettings>,
        _analysis: rtrb::Consumer<f32>,
        n: usize,
    }

    impl Harness {
        /// Wet path is the dry signal at −12 dB, so a hard switch between
        /// them jumps by up to 0.375 on a 0.5-amplitude sine.
        fn new(bypass: bool) -> Self {
            let settings = EqSettings {
                output_gain_db: -12.0,
                ..EqSettings::FLAT
            };
            let (settings_tx, settings_rx) = triple_buffer::triple_buffer(&settings);
            let (audio_tx, out) = rtrb::RingBuffer::new(1 << 16);
            let (analysis_tx, analysis_rx) = rtrb::RingBuffer::new(16);
            let side = InputSide {
                processor: Processor::new(RATE as f32, &settings),
                sample_rate: RATE,
                settings: settings_rx,
                wet: if bypass { 0.0 } else { 1.0 },
                channels: 1,
                scratch: vec![0.0; MAX_CHUNK],
                dry: vec![0.0; MAX_CHUNK],
                audio_tx,
                analysis_tx,
                shared: Arc::new(Shared::new(bypass)),
            };
            Self {
                side,
                out,
                _settings: settings_tx,
                _analysis: analysis_rx,
                n: 0,
            }
        }

        fn input(n: usize) -> f32 {
            0.5 * (2.0 * std::f32::consts::PI * 100.0 * n as f32 / RATE as f32).sin()
        }

        /// Run `blocks` input callbacks; returns (input, output) samples.
        fn run(&mut self, blocks: usize) -> (Vec<f32>, Vec<f32>) {
            let mut input = Vec::new();
            for _ in 0..blocks {
                let block: Vec<f32> = (self.n..self.n + BLOCK).map(Self::input).collect();
                self.n += BLOCK;
                self.side.process(&block);
                input.extend(block);
            }
            let output = std::iter::from_fn(|| self.out.pop().ok()).collect();
            (input, output)
        }

        fn set_bypass(&self, bypass: bool) {
            self.side.shared.bypass.store(bypass, Ordering::Relaxed);
        }
    }

    fn max_step(samples: &[f32]) -> f32 {
        samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn input_callbacks_are_counted_per_engine() {
        let mut h = Harness::new(false);
        let other = Harness::new(false);
        h.run(3);
        assert_eq!(h.side.shared.input_callbacks.load(Ordering::Relaxed), 3);
        assert_eq!(other.side.shared.input_callbacks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bypass_toggle_crossfades_without_clicking() {
        let mut h = Harness::new(false);
        let mut output = h.run(81).1;
        // The toggles land on a sine peak.
        assert!((Harness::input(h.n) - 0.5).abs() < 1e-3);
        h.set_bypass(true);
        output.extend(h.run(40).1);
        h.set_bypass(false);
        output.extend(h.run(40).1);
        h.set_bypass(true);
        // Toggle back mid-fade.
        output.extend(h.run(2).1);
        h.set_bypass(false);
        output.extend(h.run(40).1);
        // A 100 Hz 0.5-amplitude sine moves at most 0.0065 per sample; the
        // fade adds 0.375 / 960 on top. A hard switch would jump ~0.375.
        let step = max_step(&output);
        assert!(step < 0.008, "max step {step}");
    }

    #[test]
    fn bypass_fade_takes_20_ms_then_passes_dry_signal() {
        let fade = (BYPASS_FADE_SECS * RATE as f32).round() as usize;
        let mut h = Harness::new(false);
        h.run(40);
        h.set_bypass(true);
        let (input, output) = h.run(40);
        let diff = |i: usize| (output[i] - input[i]).abs();
        // Mid-fade the output is still partly wet (quieter than dry)...
        let mid = (fade / 2 - 60..fade / 2 + 60).map(diff).fold(0.0, f32::max);
        assert!(mid > 0.05, "mid-fade diff {mid}");
        // ...and from the end of the fade it is exactly the dry input.
        let after = (fade..input.len()).map(diff).fold(0.0, f32::max);
        assert_eq!(after, 0.0);
        assert_eq!(h.side.wet, 0.0);

        // Re-enabling fades the (reset) processor back in over the same time.
        h.set_bypass(false);
        let (input, output) = h.run(40);
        let wet_gain = 10f32.powf(-12.0 / 20.0);
        let settled = (fade + 60..input.len())
            .map(|i| (output[i] - input[i] * wet_gain).abs())
            .fold(0.0, f32::max);
        assert!(settled < 1e-3, "settled diff {settled}");
        assert_eq!(h.side.wet, 1.0);
    }
}
