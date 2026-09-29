//! Adaptive-ratio resampling: a 4-point cubic Hermite interpolator plus the
//! controller that nudges its ratio so the input→output ring stays near a
//! target fill despite the two devices running on independent clocks.

/// 4-point, 3rd-order Hermite (Catmull-Rom) interpolator over a pulled stream.
///
/// Reproduces constant and linear signals exactly; for voice at 48 kHz the
/// residual error is far below the mic noise floor. Adds 2 samples of delay.
#[derive(Debug, Clone)]
pub(crate) struct Hermite {
    /// `[x[-1], x[0], x[1], x[2]]`; output is interpolated between x[0] and x[1].
    hist: [f32; 4],
    /// Read position between x[0] (0.0) and x[1] (1.0).
    frac: f64,
}

impl Hermite {
    pub(crate) fn new() -> Self {
        Self {
            hist: [0.0; 4],
            frac: 0.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Produce one output sample, advancing by `step` input samples per output
    /// sample. Returns `None` if `pull` runs dry; the state stays consistent, so
    /// calling again once more input exists resumes exactly where it left off.
    #[inline]
    pub(crate) fn next(&mut self, step: f64, mut pull: impl FnMut() -> Option<f32>) -> Option<f32> {
        while self.frac >= 1.0 {
            let s = pull()?;
            self.hist = [self.hist[1], self.hist[2], self.hist[3], s];
            self.frac -= 1.0;
        }
        let [xm1, x0, x1, x2] = self.hist;
        let t = self.frac as f32;
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        self.frac += step;
        Some(((c3 * t + c2) * t + c1) * t + x0)
    }
}

/// PI controller that turns ring fill into a resampling ratio.
///
/// Plant: `d(fill)/dt = in_rate·(drift − correction)`, i.e. a pure integrator,
/// so a PI loop settles with zero steady-state error. Fill is a sawtooth
/// (blocks arrive in bursts), so it is low-passed first; the loop is kept an
/// order of magnitude slower than that filter to stay stable, and the
/// correction is clamped so a glitch can never become an audible pitch bend.
#[derive(Debug, Clone)]
pub(crate) struct RateController {
    nominal: f64,
    in_rate: f64,
    smoothed: Option<f64>,
    integral: f64,
}

/// Fill low-pass time constant. Much longer than any IO period (~5 ms) so the
/// sawtooth averages out.
const FILL_TAU_S: f64 = 0.5;
/// Proportional gain, per second of fill error. 100 ppm of drift leaves only
/// 0.5 ms of error before the integrator even kicks in.
const KP: f64 = 0.2;
/// ≈ KP²/4: critically damped for an integrator plant.
const KI: f64 = 0.01;
/// ±0.5 %: covers any real crystal drift (≤ ~0.1 %) with headroom, while a
/// transient at the limit is only ~9 cents of pitch.
const MAX_CORRECTION: f64 = 0.005;

impl RateController {
    /// `nominal` = input samples consumed per output sample (in_rate / out_rate).
    pub(crate) fn new(in_rate: f64, out_rate: f64) -> Self {
        Self {
            nominal: in_rate / out_rate,
            in_rate,
            smoothed: None,
            integral: 0.0,
        }
    }

    pub(crate) fn nominal(&self) -> f64 {
        self.nominal
    }

    /// Forget accumulated state, e.g. after a resync that jumped the fill.
    pub(crate) fn reset(&mut self) {
        self.smoothed = None;
        self.integral = 0.0;
    }

    /// Smoothed fill in input samples (the raw value before the first update).
    pub(crate) fn smoothed_fill(&self) -> Option<f64> {
        self.smoothed
    }

    /// Feed the fill measured at the start of an output period of `dt` seconds;
    /// returns the ratio to use for that period.
    pub(crate) fn update(&mut self, fill: f64, target: f64, dt: f64) -> f64 {
        let smoothed = match self.smoothed {
            None => fill,
            Some(prev) => prev + (1.0 - (-dt / FILL_TAU_S).exp()) * (fill - prev),
        };
        self.smoothed = Some(smoothed);

        let err_s = (smoothed - target) / self.in_rate;
        // Anti-windup: clamp the integrator's contribution on its own.
        self.integral = (self.integral + KI * err_s * dt).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let correction = (KP * err_s + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.nominal * (1.0 + correction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[f32], step: f64, n_out: usize) -> Vec<f32> {
        let mut h = Hermite::new();
        let mut it = input.iter().copied();
        (0..n_out)
            .map_while(|_| h.next(step, || it.next()))
            .collect()
    }

    #[test]
    fn unity_step_is_pure_delay() {
        let input: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
        let out = run(&input, 1.0, 60);
        // After priming with three zeros, x[0] at frac 0 is input[i-2].
        for i in 3..60 {
            assert!((out[i] - input[i - 3]).abs() < 1e-6, "i={i}");
        }
    }

    #[test]
    fn reproduces_linear_ramps_at_any_ratio() {
        let input: Vec<f32> = (0..2000).map(|i| i as f32 * 0.001).collect();
        for step in [0.3, 0.918_75, 1.0, 1.088_4, 2.5] {
            let out = run(&input, step, 500);
            // Skip the zero-primed start; afterwards the output lies exactly on
            // the ramp: out[n] = (n·step − 3)·0.001.
            let primed = (5.0 / step).ceil() as usize;
            for (n, y) in out.iter().enumerate().skip(primed) {
                let expected = (n as f64 * step - 3.0) * 0.001;
                assert!((*y as f64 - expected).abs() < 1e-5, "step={step} n={n}");
            }
        }
    }

    #[test]
    fn low_error_on_voice_band_sine() {
        // 3 kHz at 44.1 kHz resampled to 48 kHz.
        let (fin, fout, f) = (44_100.0_f64, 48_000.0_f64, 3_000.0_f64);
        let input: Vec<f32> = (0..20_000)
            .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / fin).sin() as f32)
            .collect();
        let step = fin / fout;
        let out = run(&input, step, 20_000);
        let mut max_err = 0.0_f64;
        for (n, y) in out.iter().enumerate().skip(10) {
            let t_in = n as f64 * step - 3.0;
            let ideal = (2.0 * std::f64::consts::PI * f * t_in / fin).sin();
            max_err = max_err.max((*y as f64 - ideal).abs());
        }
        // ≈ −40 dB worst case at 3 kHz; it falls ~24 dB per octave below that.
        assert!(max_err < 0.01, "max_err={max_err}");
    }

    #[test]
    fn running_dry_resumes_without_skipping() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut h = Hermite::new();
        let mut pos = 0;
        let mut out = Vec::new();
        // Supply input in bursts of 7 with dry spells in between.
        let mut available = 0;
        while out.len() < 80 {
            available = (available + 7).min(input.len());
            while let Some(y) = h.next(1.0, || {
                (pos < available).then(|| {
                    pos += 1;
                    input[pos - 1]
                })
            }) {
                out.push(y);
            }
        }
        for (i, y) in out.iter().enumerate().skip(3) {
            assert_eq!(*y, (i - 3) as f32);
        }
    }

    #[test]
    fn controller_output_is_clamped() {
        let mut c = RateController::new(48_000.0, 48_000.0);
        for _ in 0..10_000 {
            let r = c.update(1e9, 512.0, 0.005);
            assert!(r <= 1.0 + MAX_CORRECTION + 1e-12);
        }
        c.reset();
        for _ in 0..10_000 {
            let r = c.update(0.0, 1e9, 0.005);
            assert!(r >= 1.0 - MAX_CORRECTION - 1e-12);
        }
    }
}
