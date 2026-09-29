//! The clock-domain bridge between the input and output callbacks: an SPSC
//! ring written by the input side and drained by the output side through the
//! adaptive resampler.

use rtrb::{Consumer, Producer};

use crate::resampler::{Hermite, RateController};

/// Write a block into the ring. If it doesn't fit, the tail of the block is
/// dropped (an SPSC producer can't discard the oldest samples; the consumer
/// does that on its side via its high-water resync). Returns `true` on overrun.
pub(crate) fn push_block(tx: &mut Producer<f32>, block: &[f32]) -> bool {
    let n = block.len().min(tx.slots());
    if n > 0
        && let Ok(chunk) = tx.write_chunk_uninit(n)
    {
        chunk.fill_from_iter(block.iter().copied());
    }
    n < block.len()
}

/// Samples to fade the last output value to zero over on underrun, so a
/// dropout is a soft gap rather than a click.
const UNDERRUN_RAMP: usize = 32;
/// Resync when the smoothed fill exceeds this many times the target: the
/// controller would take too long to drain that much extra latency.
const HIGH_WATER_FACTOR: f64 = 3.0;
/// Never aim for less than this many input samples of cushion.
const MIN_TARGET: f64 = 64.0;
/// Each underrun raises the fill target by this fraction of the base target:
/// the cushion wasn't enough for this machine's scheduling jitter.
const HEADROOM_STEP: f64 = 0.5;
/// Upper bound on the target, as a multiple of the base target.
const MAX_HEADROOM: f64 = 4.0;
/// Extra headroom is held this long after the last underrun...
const HEADROOM_HOLD_S: f64 = 60.0;
/// ...then decays with this time constant — slow on purpose: jitter comes in
/// bursts (a busy system, a sleeping CPU), and giving latency back too eagerly
/// just buys the next dropout...
const HEADROOM_DECAY_TAU_S: f64 = 120.0;
/// ...but only as far as the ring's measured low points over the last window
/// of this length allow (see [`Headroom::observe`]).
const SLACK_WINDOW_S: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for the ring to reach the target before producing sound.
    Priming,
    Running,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct RenderReport {
    pub underrun: bool,
    pub overrun: bool,
    /// Smoothed ring fill in input samples.
    pub fill: f64,
    pub target: f64,
}

/// Adaptive multiplier on the base fill target.
///
/// Grows on every underrun. Shrinks back toward 1 once output has been clean
/// for a while, but never below the level at which the deepest ring dip seen
/// in the last full window would come within a safety margin of running dry.
/// Decaying blindly toward the base target would, under steady jitter, walk
/// straight back into the level that already failed and underrun again every
/// few minutes.
#[derive(Debug, Clone)]
struct Headroom {
    factor: f64,
    /// Seconds of running output since the last underrun.
    since_underrun: f64,
    window_elapsed: f64,
    /// Lowest `slack - target` seen in the current window, input samples.
    window_min_dip: f64,
    /// Lowest target (as a factor) the last full window's dips allow; `None`
    /// until a window has completed since the last underrun.
    floor: Option<f64>,
    /// Off only in tests, to compare against a fixed target.
    adaptive: bool,
}

impl Headroom {
    fn new() -> Self {
        Self {
            factor: 1.0,
            since_underrun: 0.0,
            window_elapsed: 0.0,
            window_min_dip: f64::INFINITY,
            floor: None,
            adaptive: true,
        }
    }

    /// The cushion wasn't enough: grow it. Takes effect while priming, which
    /// then waits for the ring to reach the larger target.
    fn underrun(&mut self) {
        self.since_underrun = 0.0;
        self.window_elapsed = 0.0;
        self.window_min_dip = f64::INFINITY;
        self.floor = None;
        if self.adaptive {
            self.factor = (self.factor + HEADROOM_STEP).min(MAX_HEADROOM);
        }
    }

    /// Once per running output callback of `dt` seconds. `slack` is how many
    /// samples the ring will hold after this callback's pull (≤ 0 means it
    /// runs dry); `target` is the current fill target and `base` the target
    /// at factor 1; `margin` the slack to keep in reserve at the worst dip.
    fn observe(&mut self, slack: f64, target: f64, base: f64, margin: f64, dt: f64) {
        // The rate controller holds the average fill at the target, so the
        // ring's low points sit a fairly constant distance below it: lowering
        // the target by Δ lowers the dips by Δ.
        self.window_min_dip = self.window_min_dip.min(slack - target);
        self.window_elapsed += dt;
        if self.window_elapsed >= SLACK_WINDOW_S {
            let lowest_safe_target = margin - self.window_min_dip;
            self.floor = Some((lowest_safe_target / base).max(1.0));
            self.window_elapsed = 0.0;
            self.window_min_dip = f64::INFINITY;
        }
        self.since_underrun += dt;
        if self.since_underrun > HEADROOM_HOLD_S
            && let Some(floor) = self.floor
            && self.factor > floor
        {
            self.factor = floor + (self.factor - floor) * (-dt / HEADROOM_DECAY_TAU_S).exp();
        }
    }
}

/// Output-callback side of the bridge.
pub(crate) struct Drain {
    rx: Consumer<f32>,
    interp: Hermite,
    ctrl: RateController,
    out_rate: f64,
    state: State,
    max_out_block: usize,
    /// Last emitted sample and fade position, for the underrun ramp.
    last: f32,
    ramp: usize,
    headroom: Headroom,
}

impl Drain {
    pub(crate) fn new(rx: Consumer<f32>, in_rate: f64, out_rate: f64) -> Self {
        Self {
            rx,
            interp: Hermite::new(),
            ctrl: RateController::new(in_rate, out_rate),
            out_rate,
            state: State::Priming,
            max_out_block: 0,
            last: 0.0,
            ramp: UNDERRUN_RAMP,
            headroom: Headroom::new(),
        }
    }

    /// Re-derive the resampling ratio for new stream rates (used while
    /// negotiating a config, before any audio has flowed).
    pub(crate) fn set_rates(&mut self, in_rate: f64, out_rate: f64) {
        self.ctrl = RateController::new(in_rate, out_rate);
        self.out_rate = out_rate;
    }

    /// One output block in input samples.
    fn out_block_in(&self) -> f64 {
        self.max_out_block as f64 * self.ctrl.nominal()
    }

    /// Base fill cushion: one input block (the burst the input side delivers
    /// at once) plus one output block (what we pull at once), with 25 %
    /// headroom for scheduling jitter. ≈ 2.5 IO buffers when both sides match.
    fn base_target(&self, in_block: usize) -> f64 {
        (1.25 * (in_block as f64 + self.out_block_in())).max(MIN_TARGET)
    }

    /// Fill to steer toward: the base, scaled up after underruns.
    fn target(&self, in_block: usize) -> f64 {
        self.base_target(in_block) * self.headroom.factor
    }

    fn discard(&mut self, n: usize) {
        let n = n.min(self.rx.slots());
        if let Ok(chunk) = self.rx.read_chunk(n) {
            chunk.commit_all();
        }
    }

    /// Fill `out` (interleaved, `channels` wide) with the resampled mono signal.
    ///
    /// `in_block` is the largest input callback size seen so far (0 = unknown).
    /// `in_phase` is how many input samples' worth of time has passed since the
    /// input side's last push (clamped to `in_block`). Adding it to the raw ring
    /// fill cancels the input-burst sawtooth, which otherwise shows up as a slow
    /// beat between the two callback phases that no low-pass can remove.
    pub(crate) fn render(
        &mut self,
        out: &mut [f32],
        channels: usize,
        in_block: usize,
        in_phase: f64,
    ) -> RenderReport {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        self.max_out_block = self.max_out_block.max(frames);
        let target = self.target(in_block);
        let in_phase = in_phase.clamp(0.0, in_block as f64);
        let fill = self.rx.slots() as f64 + in_phase;
        let mut report = RenderReport {
            target,
            ..RenderReport::default()
        };

        if self.state == State::Priming {
            if fill >= target {
                // Start (or restart) at exactly the target so we don't carry
                // whatever backlog built up before the output side was ready.
                self.discard((fill - target) as usize);
                self.interp.reset();
                self.ctrl.reset();
                self.state = State::Running;
            } else {
                self.fade_out(out, channels, 0);
                report.fill = fill;
                return report;
            }
        }

        let fill = self.rx.slots() as f64 + in_phase;
        let dt = frames as f64 / self.out_rate;
        let mut step = self.ctrl.update(fill, target, dt);
        if self.ctrl.smoothed_fill().unwrap_or(fill) > HIGH_WATER_FACTOR * target {
            // Drop the oldest samples: latency matters more than continuity.
            self.discard((fill - target).max(0.0) as usize);
            self.ctrl.reset();
            step = self.ctrl.update(target, target, dt);
            report.overrun = true;
        }
        let slack = self.rx.slots() as f64 - frames as f64 * step;
        let margin = self.out_block_in().max(32.0);
        let base = self.base_target(in_block);
        self.headroom.observe(slack, target, base, margin, dt);

        for f in 0..frames {
            let rx = &mut self.rx;
            match self.interp.next(step, || rx.pop().ok()) {
                Some(y) => {
                    self.last = y;
                    self.ramp = 0;
                    if let Some(frame) = out.get_mut(f * channels..(f + 1) * channels) {
                        frame.fill(y);
                    }
                }
                None => {
                    self.fade_out(out, channels, f);
                    self.interp.reset();
                    self.state = State::Priming;
                    self.headroom.underrun();
                    report.underrun = true;
                    break;
                }
            }
        }
        report.fill = self.ctrl.smoothed_fill().unwrap_or(fill);
        report
    }

    /// Write the underrun ramp/silence from frame `from` to the end.
    fn fade_out(&mut self, out: &mut [f32], channels: usize, from: usize) {
        let start = (from * channels).min(out.len());
        for frame in out[start..].chunks_mut(channels) {
            let y = if self.ramp < UNDERRUN_RAMP {
                self.ramp += 1;
                self.last * (1.0 - self.ramp as f32 / UNDERRUN_RAMP as f32)
            } else {
                0.0
            };
            frame.fill(y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sim {
        tx: Producer<f32>,
        drain: Drain,
        in_rate: f64,
        out_rate: f64,
        in_block: usize,
        out_block: usize,
        out_channels: usize,
        underruns: usize,
        overruns: usize,
        fills: Vec<(f64, f64, f64)>,
        /// Output times of each underrun.
        underrun_at: Vec<f64>,
        produced: u64,
        /// Max extra delay of each callback past its nominal time, seconds
        /// (uniform, deterministic PRNG).
        in_jitter: f64,
        out_jitter: f64,
        rng: u64,
    }

    impl Sim {
        /// `in_rate` is the input device's true rate as seen by the output clock
        /// (nominal rate × (1 + drift)); the drain is told the nominal rates.
        fn new(
            nominal_in: f64,
            true_in: f64,
            out_rate: f64,
            in_block: usize,
            out_block: usize,
        ) -> Self {
            let (tx, rx) = rtrb::RingBuffer::new(1 << 16);
            Self {
                tx,
                drain: Drain::new(rx, nominal_in, out_rate),
                in_rate: true_in,
                out_rate,
                in_block,
                out_block,
                out_channels: 2,
                underruns: 0,
                overruns: 0,
                fills: Vec::new(),
                underrun_at: Vec::new(),
                produced: 0,
                in_jitter: 0.0,
                out_jitter: 0.0,
                rng: 0x9E37_79B9_7F4A_7C15,
            }
        }

        /// Uniform in [0, 1), xorshift64*.
        fn uniform(&mut self) -> f64 {
            self.rng ^= self.rng >> 12;
            self.rng ^= self.rng << 25;
            self.rng ^= self.rng >> 27;
            (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        }

        /// Event-driven simulation of both callbacks for `seconds`; input stops
        /// producing while `pause(t)` returns true.
        /// Callbacks are due every block period on each device's clock and
        /// run up to `*_jitter` late (never early; lateness doesn't
        /// accumulate). Continues from where a previous `run` stopped.
        fn run(&mut self, seconds: f64, pause: impl Fn(f64) -> bool) {
            let in_period = self.in_block as f64 / self.in_rate;
            let out_period = self.out_block as f64 / self.out_rate;
            let start = self.fills.last().map_or(0.0, |f| f.0 + out_period);
            let (mut due_in, mut due_out) = (start, start + 0.3 * out_period);
            let mut t_in = due_in + self.in_jitter * self.uniform();
            let mut t_out = due_out + self.out_jitter * self.uniform();
            let mut last_push = f64::NEG_INFINITY;
            let mut out = vec![0.0; self.out_block * self.out_channels];
            let block: Vec<f32> = vec![0.25; self.in_block];
            let end = start + seconds;
            while t_in.min(t_out) < end {
                if t_in <= t_out {
                    if !pause(t_in) {
                        self.overruns += push_block(&mut self.tx, &block) as usize;
                        last_push = t_in;
                    }
                    self.produced += self.in_block as u64;
                    due_in += in_period;
                    t_in = due_in + self.in_jitter * self.uniform();
                } else {
                    let phase = ((t_out - last_push) * self.in_rate).min(self.in_block as f64);
                    let r = self
                        .drain
                        .render(&mut out, self.out_channels, self.in_block, phase);
                    self.underruns += r.underrun as usize;
                    self.overruns += r.overrun as usize;
                    if r.underrun {
                        self.underrun_at.push(t_out);
                    }
                    self.fills.push((t_out, r.fill, r.target));
                    due_out += out_period;
                    t_out = due_out + self.out_jitter * self.uniform();
                }
            }
        }

        fn stats_after(&self, t0: f64) -> (f64, f64) {
            let tail: Vec<_> = self.fills.iter().filter(|(t, _, _)| *t > t0).collect();
            let max_dev = tail
                .iter()
                .map(|(_, f, tg)| (f - tg).abs())
                .fold(0.0, f64::max);
            (max_dev, tail.last().map(|x| x.2).unwrap_or(0.0))
        }
    }

    /// 48 kHz, 512-frame mic blocks into 128-frame output blocks, both
    /// callbacks up to 4 ms late: enough to underrun the base target over and
    /// over.
    fn jittery_sim(adaptive: bool) -> Sim {
        let mut sim = Sim::new(48_000.0, 48_000.0 * (1.0 + 50e-6), 48_000.0, 512, 128);
        sim.in_jitter = 0.004;
        sim.out_jitter = 0.004;
        sim.drain.headroom.adaptive = adaptive;
        sim
    }

    #[test]
    fn jitter_underruns_stop_once_headroom_adapts() {
        let base = 1.25 * (512.0 + 128.0);
        let mut fixed = jittery_sim(false);
        fixed.run(900.0, |_| false);
        let late_fixed = fixed.underrun_at.iter().filter(|&&t| t > 60.0).count();

        let mut sim = jittery_sim(true);
        sim.run(900.0, |_| false);
        let late = sim.underrun_at.iter().filter(|&&t| t > 60.0).count();
        let max_target = sim.fills.iter().map(|f| f.2).fold(0.0, f64::max);
        let (max_dev, final_target) = sim.stats_after(840.0);
        eprintln!(
            "fixed: {} underruns ({late_fixed} after 60 s); adaptive: {} ({late} after 60 s) at {:?}, \
             max target {max_target:.0}, final target {final_target:.0}, max dev {max_dev:.1}",
            fixed.underruns, sim.underruns, sim.underrun_at
        );
        assert!(late_fixed >= 10, "scenario too gentle: {late_fixed}");
        assert_eq!(late, 0, "underruns after adapting: {:?}", sim.underrun_at);
        assert!(max_target <= MAX_HEADROOM * base + 1e-9);
        // It bought what it needed, not the maximum.
        assert!(final_target < 2.0 * base, "final_target={final_target}");
    }

    #[test]
    fn headroom_decays_to_base_once_jitter_stops() {
        let base = 1.25 * (512.0 + 128.0);
        let mut sim = Sim::new(48_000.0, 48_000.0, 48_000.0, 512, 128);
        sim.in_jitter = 0.008;
        sim.run(60.0, |_| false);
        assert!(sim.underruns > 0);
        let raised = sim.fills.last().unwrap().2;
        assert!(raised > 1.4 * base, "raised={raised}");

        sim.in_jitter = 0.0;
        let calm_from = sim.fills.last().unwrap().0;
        sim.run(1800.0, |_| false);
        let target_at = |dt: f64| {
            sim.fills
                .iter()
                .find(|f| f.0 >= calm_from + dt)
                .map(|f| f.2)
                .unwrap()
        };
        eprintln!(
            "raised {raised:.0}; after calm +30 s {:.0}, +5 min {:.0}, +15 min {:.0}, +30 min {:.0}",
            target_at(30.0),
            target_at(300.0),
            target_at(900.0),
            sim.fills.last().unwrap().2
        );
        assert_eq!(
            sim.underrun_at.iter().filter(|&&t| t > calm_from).count(),
            0
        );
        // Slow: still elevated well after the jitter stopped...
        assert!(target_at(30.0) > 1.3 * base);
        // ...but back near base within half an hour.
        let final_target = sim.fills.last().unwrap().2;
        assert!(final_target < 1.05 * base, "final_target={final_target}");
        let (max_dev, _) = sim.stats_after(calm_from + 1700.0);
        assert!(max_dev < 16.0, "max_dev={max_dev}");
    }

    #[test]
    fn push_block_reports_overrun_when_full() {
        let (mut tx, mut rx) = rtrb::RingBuffer::new(10);
        assert!(!push_block(&mut tx, &[1.0; 6]));
        assert!(push_block(&mut tx, &[2.0; 6]));
        let got: Vec<f32> = std::iter::from_fn(|| rx.pop().ok()).collect();
        assert_eq!(got, [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn converges_under_clock_drift() {
        for ppm in [-300.0, -50.0, 0.0, 80.0, 500.0] {
            let true_in = 48_000.0 * (1.0 + ppm * 1e-6);
            let mut sim = Sim::new(48_000.0, true_in, 48_000.0, 256, 256);
            sim.run(120.0, |_| false);
            let (max_dev, target) = sim.stats_after(60.0);
            assert_eq!(sim.underruns, 0, "ppm={ppm}");
            assert_eq!(sim.overruns, 0, "ppm={ppm}");
            assert!((target - 640.0).abs() < 1e-9, "target={target}");
            // Smoothed fill stays within a few samples of target once settled.
            assert!(max_dev < 8.0, "ppm={ppm} max_dev={max_dev}");
        }
    }

    #[test]
    fn converges_across_rates_and_block_sizes() {
        // 44.1 kHz mic in 512-frame blocks feeding a 48 kHz device in 128s.
        let true_in = 44_100.0 * (1.0 + 120e-6);
        let mut sim = Sim::new(44_100.0, true_in, 48_000.0, 512, 128);
        sim.run(120.0, |_| false);
        let (max_dev, _) = sim.stats_after(60.0);
        assert_eq!(sim.underruns, 0);
        assert_eq!(sim.overruns, 0);
        assert!(max_dev < 16.0, "max_dev={max_dev}");
    }

    #[test]
    fn counts_underrun_then_recovers() {
        let mut sim = Sim::new(48_000.0, 48_000.0, 48_000.0, 256, 256);
        sim.run(30.0, |t| (10.0..10.2).contains(&t));
        assert_eq!(sim.underruns, 1);
        assert_eq!(sim.overruns, 0);
        let (max_dev, _) = sim.stats_after(25.0);
        assert!(max_dev < 8.0, "max_dev={max_dev}");
    }

    #[test]
    fn startup_backlog_is_discarded_without_counting() {
        let (mut tx, rx) = rtrb::RingBuffer::new(1 << 16);
        let mut drain = Drain::new(rx, 48_000.0, 48_000.0);
        // Output starts 200 ms after input.
        for _ in 0..(9600 / 256) {
            push_block(&mut tx, &[0.1; 256]);
        }
        let mut out = vec![0.0; 256];
        let r = drain.render(&mut out, 1, 256, 0.0);
        assert!(!r.overrun && !r.underrun);
        // target 640 at 256/256 blocks; one block consumed.
        let left = drain.rx.slots() as f64;
        assert!((left - (640.0 - 256.0)).abs() <= 2.0, "left={left}");
    }

    #[test]
    fn flood_triggers_overrun_resync() {
        let (mut tx, rx) = rtrb::RingBuffer::new(1 << 16);
        let mut drain = Drain::new(rx, 48_000.0, 48_000.0);
        let mut out = vec![0.0; 256];
        push_block(&mut tx, &[0.1; 640]);
        let r = drain.render(&mut out, 1, 256, 0.0);
        assert!(!r.overrun);
        // Input side suddenly dumps 20k samples (e.g. output stalled).
        for _ in 0..80 {
            push_block(&mut tx, &[0.1; 256]);
        }
        // The smoothed fill needs a moment to cross high water.
        let mut overruns = 0;
        for _ in 0..200 {
            push_block(&mut tx, &[0.1; 256]);
            overruns += drain.render(&mut out, 1, 256, 0.0).overrun as usize;
        }
        assert_eq!(overruns, 1);
        assert!(drain.rx.slots() < 3 * 640);
    }

    #[test]
    fn underrun_fades_instead_of_clicking() {
        let (mut tx, rx) = rtrb::RingBuffer::new(4096);
        let mut drain = Drain::new(rx, 48_000.0, 48_000.0);
        push_block(&mut tx, &[0.8; 800]);
        let mut out = vec![0.0; 256];
        drain.render(&mut out, 1, 256, 0.0);
        drain.render(&mut out, 1, 256, 0.0);
        let r = drain.render(&mut out, 1, 256, 0.0);
        assert!(r.underrun);
        let max_jump = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            max_jump <= 0.8 / UNDERRUN_RAMP as f32 + 1e-6,
            "max_jump={max_jump}"
        );
        assert_eq!(*out.last().unwrap(), 0.0);
    }
}
