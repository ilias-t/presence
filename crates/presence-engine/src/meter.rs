//! Level metering split across threads: the audio callback accumulates raw
//! peak and energy into atomics; the UI thread drains them and applies meter
//! ballistics, so meters keep decaying even if the audio thread stalls.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub(crate) const FLOOR_DB: f32 = -100.0;
/// Peak-hold fall rate.
const PEAK_DECAY_DB_PER_S: f32 = 20.0;
/// RMS integration time; ~300 ms is the classic VU-like feel.
const RMS_TAU_S: f64 = 0.3;

pub(crate) fn lin_to_db(amplitude: f32) -> f32 {
    if amplitude > 0.0 {
        (20.0 * amplitude.log10()).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

fn power_to_db(mean_square: f64) -> f32 {
    if mean_square > 0.0 {
        ((10.0 * mean_square.log10()) as f32).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

/// Audio-thread side: lock-free accumulators since the last [`MeterTap::take`].
#[derive(Default)]
pub(crate) struct MeterTap {
    /// Max |x| as f32 bits. Non-negative floats order the same as their bit
    /// patterns, so `fetch_max` on the bits is a float max.
    peak: AtomicU32,
    /// Sum of squares as f64 bits (f64 so long gaps between reads don't lose
    /// precision).
    sum_sq: AtomicU64,
    count: AtomicU64,
}

impl MeterTap {
    #[inline]
    pub(crate) fn record(&self, block: &[f32]) {
        let mut peak = 0.0_f32;
        let mut sum_sq = 0.0_f64;
        for &x in block {
            let a = x.abs();
            // `>` is false for NaN, so garbage input can't poison the meter.
            if a > peak && a.is_finite() {
                peak = a;
            }
            if x.is_finite() {
                sum_sq += f64::from(x) * f64::from(x);
            }
        }
        self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        // CAS loop only contends with the UI's once-per-frame swap.
        let _ = self
            .sum_sq
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                Some((f64::from_bits(bits) + sum_sq).to_bits())
            });
        self.count.fetch_add(block.len() as u64, Ordering::Relaxed);
    }

    /// Returns `(peak, mean_square)` accumulated since the previous call;
    /// `mean_square` is `None` if no samples arrived.
    pub(crate) fn take(&self) -> (f32, Option<f64>) {
        let peak = f32::from_bits(self.peak.swap(0, Ordering::Relaxed));
        let sum_sq = f64::from_bits(self.sum_sq.swap(0, Ordering::Relaxed));
        let count = self.count.swap(0, Ordering::Relaxed);
        // The three swaps aren't one atomic snapshot; a block landing in
        // between skews one reading slightly, which a meter can ignore.
        let mean_sq = (count > 0).then(|| sum_sq / count as f64);
        (peak, mean_sq)
    }
}

/// UI-thread side: peak hold with linear dB fall, and exponentially smoothed RMS.
#[derive(Debug, Clone)]
pub(crate) struct Ballistics {
    peak_db: f32,
    mean_sq: f64,
}

impl Default for Ballistics {
    fn default() -> Self {
        Self {
            peak_db: FLOOR_DB,
            mean_sq: 0.0,
        }
    }
}

impl Ballistics {
    /// Advance by `dt` seconds with the raw values from [`MeterTap::take`];
    /// returns `(peak_db, rms_db)`.
    pub(crate) fn update(&mut self, peak: f32, mean_sq: Option<f64>, dt: f32) -> (f32, f32) {
        let dt = dt.max(0.0);
        let fallen = (self.peak_db - PEAK_DECAY_DB_PER_S * dt).max(FLOOR_DB);
        self.peak_db = lin_to_db(peak).max(fallen);

        // No samples means the stream is idle: integrate silence so it decays.
        let target = mean_sq.unwrap_or(0.0);
        let alpha = 1.0 - (-f64::from(dt) / RMS_TAU_S).exp();
        self.mean_sq += alpha * (target - self.mean_sq);
        (self.peak_db, power_to_db(self.mean_sq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn db_conversion_and_floor() {
        assert!(approx(lin_to_db(1.0), 0.0, 1e-6));
        assert!(approx(lin_to_db(0.5), -6.0206, 1e-3));
        assert_eq!(lin_to_db(0.0), FLOOR_DB);
        assert_eq!(lin_to_db(1e-9), FLOOR_DB);
        assert_eq!(power_to_db(0.0), FLOOR_DB);
    }

    #[test]
    fn tap_accumulates_peak_and_energy_across_blocks() {
        let tap = MeterTap::default();
        tap.record(&[0.1, -0.5, 0.2]);
        tap.record(&[0.3, f32::NAN, -0.4]);
        let (peak, ms) = tap.take();
        assert_eq!(peak, 0.5);
        let expected = (0.01 + 0.25 + 0.04 + 0.09 + 0.16) / 6.0;
        assert!((ms.unwrap() - expected).abs() < 1e-7);
        assert_eq!(tap.take(), (0.0, None));
    }

    #[test]
    fn full_scale_sine_reads_minus_3_db_rms() {
        let tap = MeterTap::default();
        let sine: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 997.0 / 48_000.0).sin())
            .collect();
        tap.record(&sine);
        let (peak, ms) = tap.take();
        let mut b = Ballistics::default();
        // A long dt makes the RMS integrator settle in one step.
        let (peak_db, rms_db) = b.update(peak, ms, 10.0);
        assert!(approx(peak_db, 0.0, 0.01));
        assert!(approx(rms_db, -3.01, 0.02), "rms_db={rms_db}");
    }

    #[test]
    fn peak_holds_then_falls_20_db_per_second() {
        let mut b = Ballistics::default();
        b.update(1.0, Some(1.0), 0.0);
        let (p, _) = b.update(0.0, Some(0.0), 0.5);
        assert!(approx(p, -10.0, 1e-4), "p={p}");
        let (p, _) = b.update(0.0, None, 0.25);
        assert!(approx(p, -15.0, 1e-4), "p={p}");
        // A louder new peak replaces the falling one immediately.
        let (p, _) = b.update(0.5, Some(0.0), 0.01);
        assert!(approx(p, -6.0206, 1e-3));
        // Eventually floors.
        let (p, _) = b.update(0.0, None, 100.0);
        assert_eq!(p, FLOOR_DB);
    }

    #[test]
    fn rms_decays_when_stream_goes_idle() {
        let mut b = Ballistics::default();
        let (_, r0) = b.update(0.5, Some(0.25), 10.0);
        assert!(approx(r0, -6.0206, 1e-3));
        let (_, r1) = b.update(0.0, None, RMS_TAU_S as f32);
        // One time constant of silence: power × e⁻¹ → −4.34 dB.
        assert!(approx(r1, r0 - 4.343, 0.01), "r1={r1}");
        let (_, r2) = b.update(0.0, None, 1000.0);
        assert_eq!(r2, FLOOR_DB);
    }
}
