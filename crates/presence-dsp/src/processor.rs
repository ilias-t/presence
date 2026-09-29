use crate::EqSettings;
use crate::LIMITER_CEILING_DB;
use crate::filter::{Svf, SvfCoeffs};
use crate::params::{Resolved, STAGES, Shape, StageParams, db_to_gain, sanitize_sample_rate};

/// Smoothers advance and coefficients are recomputed once per this many
/// samples, counted across `process` calls so behaviour is independent of the
/// host buffer size.
const TICK: usize = 32;
/// One-pole time constant for gain, frequency and Q.
const SMOOTHING_SECS: f32 = 0.03;
/// Linear fade used when a stage is enabled, disabled or changes shape.
const FADE_SECS: f32 = 0.03;
const LIMITER_ATTACK_SECS: f32 = 0.001;
const LIMITER_RELEASE_SECS: f32 = 0.1;

/// Snap thresholds, in each smoother's own domain.
const EPS_DB: f32 = 1e-3;
const EPS_LOG: f32 = 1e-4;

/// One-pole smoother stepped once per tick.
#[derive(Clone, Copy, Debug, Default)]
struct Smoothed {
    value: f32,
    target: f32,
}

impl Smoothed {
    fn snap(&mut self) {
        self.value = self.target;
    }

    /// Returns whether the value changed.
    fn advance(&mut self, alpha: f32, eps: f32) -> bool {
        if self.value == self.target {
            return false;
        }
        self.value += (self.target - self.value) * alpha;
        if (self.target - self.value).abs() < eps {
            self.value = self.target;
        }
        true
    }
}

/// Per-sample linear ramp that lands exactly on its target at the next tick,
/// so gains and mixes never step within a tick.
#[derive(Clone, Copy, Debug, Default)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
}

impl Ramp {
    fn snap(&mut self, value: f32) {
        *self = Self {
            value,
            target: value,
            step: 0.0,
        };
    }

    /// Must be called exactly once per tick.
    fn aim(&mut self, target: f32) {
        // The previous ramp has just completed; remove accumulated rounding.
        self.value = self.target;
        self.target = target;
        self.step = (target - self.value) / TICK as f32;
    }

    #[inline]
    fn next(&mut self) -> f32 {
        let v = self.value;
        self.value += self.step;
        v
    }
}

#[derive(Clone, Copy, Debug)]
struct Stage {
    target: StageParams,
    /// Shape currently running; lags `target.shape` while fading through neutral.
    shape: Shape,
    log2_freq: Smoothed,
    gain_db: Smoothed,
    ln_q: Smoothed,
    /// 0 = neutral, 1 = fully applied.
    level: f32,
    /// Dry/wet mix; stays at 1 for shapes with gain, which fade via `level`.
    mix: Ramp,
    coeffs: SvfCoeffs,
    /// SVF output weights, interpolated per sample: they act on the output
    /// directly, so stepping them once per tick would be audible.
    weights: [Ramp; 3],
    svf: [Svf; 2],
    idle: bool,
}

impl Stage {
    fn new(target: StageParams) -> Self {
        let mut stage = Self {
            target,
            shape: target.shape,
            log2_freq: Smoothed::default(),
            gain_db: Smoothed::default(),
            ln_q: Smoothed::default(),
            level: 0.0,
            mix: Ramp::default(),
            coeffs: SvfCoeffs::default(),
            weights: [Ramp::default(); 3],
            svf: [Svf::default(); 2],
            idle: true,
        };
        stage.retarget(target);
        stage
    }

    fn retarget(&mut self, target: StageParams) {
        self.target = target;
        self.log2_freq.target = target.freq_hz.log2();
        self.gain_db.target = target.gain_db;
        self.ln_q.target = target.q.ln();
    }

    fn wants(&self) -> bool {
        self.target.enabled && self.target.shape == self.shape
    }

    /// Jumps to the target shape and parameters at `level`, clearing state.
    fn snap(&mut self, sample_rate: f32, level: f32) {
        self.shape = self.target.shape;
        self.log2_freq.snap();
        self.gain_db.snap();
        self.ln_q.snap();
        self.svf = [Svf::default(); 2];
        self.level = level;
        self.idle = level == 0.0 && !self.target.enabled;
        self.mix.snap(self.mix_target());
        let weights = self.update_coeffs(sample_rate);
        for (ramp, w) in self.weights.iter_mut().zip(weights) {
            ramp.snap(w);
        }
    }

    fn mix_target(&self) -> f32 {
        if self.shape.has_gain() {
            1.0
        } else {
            self.level
        }
    }

    /// Updates the recursive coefficients and returns the new output weights.
    fn update_coeffs(&mut self, sample_rate: f32) -> [f32; 3] {
        let current = StageParams {
            freq_hz: self.log2_freq.value.exp2(),
            q: self.ln_q.value.exp(),
            shape: self.shape,
            ..self.target
        };
        let gain_db = if self.shape.has_gain() {
            self.gain_db.value * self.level
        } else {
            0.0
        };
        let prototype = current.prototype(sample_rate, gain_db);
        self.coeffs = prototype.coeffs();
        prototype.weights()
    }

    fn tick(&mut self, sample_rate: f32, alpha: f32, fade_step: f32) {
        // A completed fade-out has now been rendered for a full tick.
        if !self.idle && self.level == 0.0 && !self.wants() {
            self.idle = true;
        }
        let mut dirty = false;
        if self.idle {
            if !self.target.enabled {
                return;
            }
            // Output is neutral at level 0, so jump straight to the new shape
            // and parameters and let the fade-in hide the filter warming up.
            self.snap(sample_rate, 0.0);
        }

        for svf in &mut self.svf {
            svf.sanitize();
        }

        let level_target = if self.wants() { 1.0 } else { 0.0 };
        if self.level != level_target {
            self.level = if level_target > self.level {
                (self.level + fade_step).min(1.0)
            } else {
                (self.level - fade_step).max(0.0)
            };
            dirty = true;
        }
        dirty |= self.log2_freq.advance(alpha, EPS_LOG);
        dirty |= self.gain_db.advance(alpha, EPS_DB);
        dirty |= self.ln_q.advance(alpha, EPS_LOG);
        if dirty {
            let weights = self.update_coeffs(sample_rate);
            for (ramp, w) in self.weights.iter_mut().zip(weights) {
                ramp.aim(w);
            }
        } else {
            for ramp in &mut self.weights {
                ramp.aim(ramp.target);
            }
        }
        self.mix.aim(self.mix_target());
    }

    fn run(&mut self, buf: &mut [f32]) {
        let c = self.coeffs;
        let [first, second] = &mut self.svf;
        let [w0, w1, w2] = &mut self.weights;
        for x in buf {
            let m = [w0.next(), w1.next(), w2.next()];
            let mut wet = first.process(&c, m, *x);
            if self.shape.cascade {
                wet = second.process(&c, m, wet);
            }
            *x += self.mix.next() * (wet - *x);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Gain {
    db: Smoothed,
    linear: Ramp,
}

impl Gain {
    fn new(db: f32) -> Self {
        let mut gain = Self {
            db: Smoothed::default(),
            linear: Ramp::default(),
        };
        gain.db.target = db;
        gain.snap();
        gain
    }

    fn snap(&mut self) {
        self.db.snap();
        self.linear.snap(db_to_gain(self.db.value));
    }

    fn tick(&mut self, alpha: f32) {
        self.db.advance(alpha, EPS_DB);
        self.linear.aim(db_to_gain(self.db.value));
    }

    fn run(&mut self, buf: &mut [f32]) {
        if self.linear.step == 0.0 && self.linear.value == 1.0 {
            return;
        }
        for x in buf {
            *x *= self.linear.next();
        }
    }
}

/// Zero-latency peak limiter: a fast-attack envelope drives the gain and a
/// final clamp catches whatever the 1 ms attack lets through.
#[derive(Clone, Copy, Debug)]
struct Limiter {
    ceiling: f32,
    attack: f32,
    release: f32,
    envelope: f32,
}

impl Limiter {
    fn new(sample_rate: f32) -> Self {
        Self {
            ceiling: db_to_gain(LIMITER_CEILING_DB),
            attack: (-1.0 / (LIMITER_ATTACK_SECS * sample_rate)).exp(),
            release: (-1.0 / (LIMITER_RELEASE_SECS * sample_rate)).exp(),
            envelope: 0.0,
        }
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
    }

    fn run(&mut self, buf: &mut [f32]) {
        for x in buf {
            let peak = x.abs();
            let coeff = if peak > self.envelope {
                self.attack
            } else {
                self.release
            };
            self.envelope = peak + (self.envelope - peak) * coeff;
            if self.envelope > self.ceiling {
                *x *= self.ceiling / self.envelope;
            }
            *x = x.clamp(-self.ceiling, self.ceiling);
        }
        if self.envelope < 1e-20 {
            self.envelope = 0.0;
        }
    }
}

/// Mono, in-place, real-time-safe voice processor:
/// input gain → high-pass → bands → output gain → limiter.
pub struct Processor {
    sample_rate: f32,
    settings: Resolved,
    alpha: f32,
    fade_step: f32,
    until_tick: usize,
    input_gain: Gain,
    stages: [Stage; STAGES],
    output_gain: Gain,
    limiter: Limiter,
}

impl Processor {
    pub fn new(sample_rate: f32, settings: &EqSettings) -> Self {
        let sample_rate = sanitize_sample_rate(sample_rate);
        let resolved = Resolved::new(settings, sample_rate);
        let ticks_per_sec = sample_rate / TICK as f32;
        let mut processor = Self {
            sample_rate,
            settings: resolved,
            alpha: 1.0 - (-1.0 / (SMOOTHING_SECS * ticks_per_sec)).exp(),
            fade_step: 1.0 / (FADE_SECS * ticks_per_sec),
            until_tick: 0,
            input_gain: Gain::new(resolved.input_gain_db),
            stages: resolved.stages.map(Stage::new),
            output_gain: Gain::new(resolved.output_gain_db),
            limiter: Limiter::new(sample_rate),
        };
        processor.reset();
        processor
    }

    /// Retarget parameters. Must not allocate; changes are smoothed over
    /// ~20–50 ms so moving a knob never clicks.
    pub fn set_settings(&mut self, settings: &EqSettings) {
        let resolved = Resolved::new(settings, self.sample_rate);
        if resolved == self.settings {
            return;
        }
        self.settings = resolved;
        self.input_gain.db.target = resolved.input_gain_db;
        self.output_gain.db.target = resolved.output_gain_db;
        for (stage, target) in self.stages.iter_mut().zip(resolved.stages) {
            stage.retarget(target);
        }
        if !resolved.limiter {
            self.limiter.reset();
        }
    }

    /// Process a block of mono samples in place. Real-time safe.
    pub fn process(&mut self, buf: &mut [f32]) {
        let mut rest = buf;
        while !rest.is_empty() {
            if self.until_tick == 0 {
                self.tick();
            }
            let n = self.until_tick.min(rest.len());
            let (run, tail) = rest.split_at_mut(n);
            self.run(run);
            self.until_tick -= n;
            rest = tail;
        }
    }

    /// Clear filter state (e.g. after a device change or bypass toggle).
    pub fn reset(&mut self) {
        self.input_gain.snap();
        self.output_gain.snap();
        for stage in &mut self.stages {
            let level = if stage.target.enabled { 1.0 } else { 0.0 };
            stage.snap(self.sample_rate, level);
        }
        self.limiter.reset();
        self.until_tick = TICK;
    }

    fn tick(&mut self) {
        self.input_gain.tick(self.alpha);
        self.output_gain.tick(self.alpha);
        for stage in &mut self.stages {
            stage.tick(self.sample_rate, self.alpha, self.fade_step);
        }
        self.until_tick = TICK;
    }

    fn run(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            // A single NaN would otherwise live in the filter state forever.
            if !x.is_finite() {
                *x = 0.0;
            }
        }
        self.input_gain.run(buf);
        for stage in &mut self.stages {
            if !stage.idle {
                stage.run(buf);
            }
        }
        self.output_gain.run(buf);
        if self.settings.limiter {
            self.limiter.run(buf);
        }
    }
}
