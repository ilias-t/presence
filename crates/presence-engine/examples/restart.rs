//! Live check that dropping an [`Engine`] really stops its audio.
//!
//! Opens a monitor on the loopback device's *input* side, then runs several
//! start → measure → drop cycles of an engine that plays the mic (bypassed)
//! into the loopback's output side. After each drop the loopback must fall to
//! digital silence and the engine callback counter must stop; while an engine
//! runs, the loopback level must match the engine's own output level (a
//! leaked earlier engine would add a second copy: about +6 dB).
//!
//! cargo run -p presence-engine --example restart -- \
//!     [--input NAME] [--loopback NAME] [--cycles N]
//!
//! `--input` defaults to the engine's automatic choice (never a virtual
//! device) and `--loopback` to the detected virtual device.
//!
//! Nothing else may be playing into the loopback device while this runs.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use presence_dsp::EqSettings;
use presence_engine::{Engine, EngineConfig, audio_callback_count};

/// Largest acceptable loopback/engine level difference while one engine runs.
const MAX_LEVEL_DIFF_DB: f64 = 1.5;
/// Below this the mic is too quiet for the level comparison to mean anything.
const MIN_MIC_DB: f64 = -85.0;

#[derive(Default)]
struct Acc {
    sum_sq: f64,
    count: u64,
    peak: f32,
}

impl Acc {
    fn add(&mut self, samples: impl Iterator<Item = f32>) {
        for x in samples {
            self.sum_sq += f64::from(x) * f64::from(x);
            self.count += 1;
            self.peak = self.peak.max(x.abs());
        }
    }

    fn rms_db(&self) -> f64 {
        if self.count == 0 || self.sum_sq == 0.0 {
            f64::NEG_INFINITY
        } else {
            10.0 * (self.sum_sq / self.count as f64).log10()
        }
    }
}

struct Monitor {
    acc: Arc<Mutex<Acc>>,
    _stream: cpal::Stream,
}

impl Monitor {
    /// Channel 0 of the loopback's input, at its current nominal rate so
    /// opening it never changes the device's rate under the engine.
    fn open(name: &str) -> Result<Self, String> {
        let info = presence_engine::list_input_devices()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or(format!("no input device named {name:?}"))?;
        let device = cpal::default_host()
            .devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().is_ok_and(|n| n == name))
            .ok_or(format!("cpal has no device named {name:?}"))?;
        let channels = usize::from(info.channels.max(1));
        let config = cpal::StreamConfig {
            channels: info.channels,
            sample_rate: cpal::SampleRate(info.default_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };
        let acc = Arc::new(Mutex::new(Acc::default()));
        let cb_acc = Arc::clone(&acc);
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut acc) = cb_acc.lock() {
                        acc.add(data.iter().step_by(channels).copied());
                    }
                },
                |err| eprintln!("monitor stream error: {err}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            acc,
            _stream: stream,
        })
    }

    fn reset(&self) {
        *self.acc.lock().unwrap() = Acc::default();
    }

    fn take(&self) -> Acc {
        std::mem::take(&mut *self.acc.lock().unwrap())
    }
}

struct Args {
    input: Option<String>,
    loopback: Option<String>,
    cycles: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: None,
        loopback: None,
        cycles: 3,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--input" => args.input = Some(value("--input")?),
            "--loopback" => args.loopback = Some(value("--loopback")?),
            "--cycles" => {
                args.cycles = value("--cycles")?
                    .parse()
                    .map_err(|e| format!("--cycles: {e}"))?
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

fn run(args: &Args) -> Result<bool, String> {
    let loopback = match &args.loopback {
        Some(name) => name.clone(),
        None => {
            presence_engine::find_virtual_output()
                .map_err(|e| e.to_string())?
                .ok_or("no virtual device found; pass --loopback NAME")?
                .name
        }
    };
    let monitor = Monitor::open(&loopback)?;
    let mut ok = true;

    std::thread::sleep(Duration::from_millis(300));
    monitor.reset();
    std::thread::sleep(Duration::from_secs(1));
    let base = monitor.take();
    println!(
        "baseline (no engine): loopback rms {:.1} dBFS peak {:.3e} over {} samples",
        base.rms_db(),
        base.peak,
        base.count
    );
    if base.count == 0 {
        return Err("monitor received no audio".into());
    }
    if base.peak != 0.0 {
        return Err(
            "loopback is not silent before any engine started; something else is playing into it"
                .into(),
        );
    }

    for cycle in 1..=args.cycles {
        let engine = Engine::start(EngineConfig {
            input_device: args.input.clone(),
            output_device: Some(loopback.clone()),
            settings: EqSettings::default(),
            bypass: true,
        })
        .map_err(|e| format!("cycle {cycle}: start: {e}"))?;
        let stats = engine.stats();

        // Let priming and the rate controller settle, then measure both ends
        // over the same wall-clock window.
        std::thread::sleep(Duration::from_secs(1));
        let mut analysis = Vec::new();
        engine.drain_analysis(&mut analysis);
        analysis.clear();
        monitor.reset();
        let mut engine_out = Acc::default();
        let window = Instant::now();
        while window.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
            engine.drain_analysis(&mut analysis);
            engine_out.add(analysis.drain(..));
        }
        let loop_acc = monitor.take();
        let diff = loop_acc.rms_db() - engine_out.rms_db();
        let level_ok = engine_out.rms_db() > MIN_MIC_DB && diff.abs() <= MAX_LEVEL_DIFF_DB;
        println!(
            "cycle {cycle}: {} Hz -> {} Hz | engine out rms {:.1} dBFS | loopback rms {:.1} dBFS \
             | diff {:+.2} dB {}",
            stats.input_sample_rate,
            stats.output_sample_rate,
            engine_out.rms_db(),
            loop_acc.rms_db(),
            diff,
            if level_ok { "OK" } else { "FAIL" }
        );
        ok &= level_ok;

        drop(engine);
        std::thread::sleep(Duration::from_millis(300));
        let c0 = audio_callback_count();
        monitor.reset();
        std::thread::sleep(Duration::from_secs(1));
        let c1 = audio_callback_count();
        let after = monitor.take();
        let silent = after.count > 0 && after.peak == 0.0 && c1 == c0;
        println!(
            "cycle {cycle}: after drop | callbacks in 1 s: {} | loopback rms {:.1} dBFS peak {:.3e} \
             over {} samples {}",
            c1 - c0,
            after.rms_db(),
            after.peak,
            after.count,
            if silent { "OK" } else { "FAIL" }
        );
        ok &= silent;
    }
    Ok(ok)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(&args) {
        Ok(true) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("FAIL");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
