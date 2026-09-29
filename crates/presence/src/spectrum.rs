//! Spectrum analyzer for the processed signal: 4096-point Hann-windowed FFT,
//! binned onto the same log-spaced frequencies the EQ curve is drawn at.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const FFT_SIZE: usize = 4096;
/// Number of log-spaced points for both the EQ curve and the spectrum.
pub const DISPLAY_POINTS: usize = 200;
pub const MIN_DISPLAY_HZ: f32 = 20.0;
pub const MAX_DISPLAY_HZ: f32 = 20_000.0;
pub const FLOOR_DB: f32 = -100.0;
/// Falling peaks decay at this rate so the display reads as motion, not flicker.
pub const DECAY_DB_PER_SEC: f32 = 15.0;

/// Log-spaced frequencies from 20 Hz to 20 kHz.
pub fn display_freqs() -> Vec<f32> {
    let ratio = MAX_DISPLAY_HZ / MIN_DISPLAY_HZ;
    (0..DISPLAY_POINTS)
        .map(|i| MIN_DISPLAY_HZ * ratio.powf(i as f32 / (DISPLAY_POINTS - 1) as f32))
        .collect()
}

/// Maps FFT magnitudes (`mag_db[k]` for bins `0..=N/2`) onto `points`.
///
/// Each point covers the band between the geometric midpoints to its
/// neighbours and takes the loudest bin inside it, so narrow peaks survive at
/// high frequencies where many bins share one point. At low frequencies a
/// point can be narrower than one bin (11.7 Hz at 48 kHz); there we
/// interpolate between the two nearest bins instead.
pub fn bin_log(mag_db: &[f32], sample_rate: f32, points: &[f32]) -> Vec<f32> {
    if mag_db.len() < 2 || points.is_empty() {
        return vec![FLOOR_DB; points.len()];
    }
    let fft_size = (mag_db.len() - 1) * 2;
    let bin_hz = sample_rate / fft_size as f32;
    let last_bin = mag_db.len() - 1;
    let edge = |a: f32, b: f32| (a * b).sqrt();

    (0..points.len())
        .map(|i| {
            let f = points[i];
            let lo = if i > 0 { edge(points[i - 1], f) } else { f };
            let hi = if i + 1 < points.len() {
                edge(f, points[i + 1])
            } else {
                f
            };
            // Skip DC: it's offset, not spectrum.
            let first = ((lo / bin_hz).ceil() as usize).max(1);
            let end = ((hi / bin_hz).floor() as usize).min(last_bin);
            if first <= end {
                mag_db[first..=end].iter().copied().fold(FLOOR_DB, f32::max)
            } else {
                let pos = (f / bin_hz).clamp(1.0, last_bin as f32);
                let k = pos.floor() as usize;
                let next = (k + 1).min(last_bin);
                let t = pos - k as f32;
                mag_db[k] * (1.0 - t) + mag_db[next] * t
            }
        })
        .collect()
}

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Scales |X| so a full-scale sine reads 0 dBFS.
    norm: f32,
    ring: Vec<f32>,
    write_pos: usize,
    filled: usize,
    fresh: bool,
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    mag_db: Vec<f32>,
    freqs: Vec<f32>,
    display: Vec<f32>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let norm = 2.0 / window.iter().sum::<f32>();
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let freqs = display_freqs();
        Self {
            fft,
            window,
            norm,
            ring: vec![0.0; FFT_SIZE],
            write_pos: 0,
            filled: 0,
            fresh: false,
            buf: vec![Complex::default(); FFT_SIZE],
            scratch,
            mag_db: vec![FLOOR_DB; FFT_SIZE / 2 + 1],
            display: vec![FLOOR_DB; freqs.len()],
            freqs,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        // Only the newest FFT_SIZE samples matter.
        let samples = &samples[samples.len().saturating_sub(FFT_SIZE)..];
        for &s in samples {
            self.ring[self.write_pos] = s;
            self.write_pos = (self.write_pos + 1) % FFT_SIZE;
        }
        self.filled = (self.filled + samples.len()).min(FFT_SIZE);
        self.fresh |= !samples.is_empty();
    }

    /// Recomputes the spectrum if new audio arrived and applies decay for `dt_secs`.
    pub fn update(&mut self, sample_rate: f32, dt_secs: f32) -> &[f32] {
        let floor_after_decay = DECAY_DB_PER_SEC * dt_secs;
        if self.fresh && self.filled == FFT_SIZE {
            self.fresh = false;
            for (i, c) in self.buf.iter_mut().enumerate() {
                let s = self.ring[(self.write_pos + i) % FFT_SIZE];
                *c = Complex::new(s * self.window[i], 0.0);
            }
            self.fft
                .process_with_scratch(&mut self.buf, &mut self.scratch);
            for (m, c) in self.mag_db.iter_mut().zip(&self.buf) {
                *m = (20.0 * (c.norm() * self.norm).max(1e-9).log10()).max(FLOOR_DB);
            }
            let binned = bin_log(&self.mag_db, sample_rate, &self.freqs);
            for (d, b) in self.display.iter_mut().zip(binned) {
                *d = b.max(*d - floor_after_decay).max(FLOOR_DB);
            }
        } else {
            for d in &mut self.display {
                *d = (*d - floor_after_decay).max(FLOOR_DB);
            }
        }
        &self.display
    }

    /// Forget history, e.g. after the engine restarts on another device.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.filled = 0;
        self.fresh = false;
        self.display.fill(FLOOR_DB);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nearest_point(freqs: &[f32], hz: f32) -> usize {
        freqs
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1.ln() - hz.ln())
                    .abs()
                    .total_cmp(&(b.1.ln() - hz.ln()).abs())
            })
            .unwrap()
            .0
    }

    #[test]
    fn display_freqs_span_audio_range_logarithmically() {
        let f = display_freqs();
        assert_eq!(f.len(), DISPLAY_POINTS);
        assert!((f[0] - 20.0).abs() < 1e-3);
        assert!((f[DISPLAY_POINTS - 1] - 20_000.0).abs() < 1.0);
        let r1 = f[1] / f[0];
        let r2 = f[150] / f[149];
        assert!((r1 - r2).abs() < 1e-4);
    }

    #[test]
    fn spike_lands_on_matching_point() {
        let sr = 48_000.0;
        let mut mags = vec![FLOOR_DB; FFT_SIZE / 2 + 1];
        let bin_hz = sr / FFT_SIZE as f32;
        let k = (5_000.0 / bin_hz).round() as usize;
        mags[k] = -6.0;
        let freqs = display_freqs();
        let out = bin_log(&mags, sr, &freqs);
        let expected = nearest_point(&freqs, k as f32 * bin_hz);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, expected);
        assert_eq!(out[peak], -6.0);
        assert_eq!(
            out.iter().filter(|&&v| v > FLOOR_DB).count(),
            1,
            "spike claimed by one point only"
        );
    }

    #[test]
    fn low_points_interpolate_between_bins() {
        let sr = 48_000.0;
        let bin_hz = sr / FFT_SIZE as f32;
        // Linear ramp in bin index; interpolation should reproduce f / bin_hz.
        let mags: Vec<f32> = (0..=FFT_SIZE / 2).map(|k| k as f32).collect();
        let out = bin_log(&mags, sr, &[30.0]);
        assert!((out[0] - 30.0 / bin_hz).abs() < 1e-3);
    }

    #[test]
    fn full_scale_sine_reads_near_zero_dbfs() {
        let sr = 48_000.0;
        let hz = 1_000.0;
        let samples: Vec<f32> = (0..FFT_SIZE)
            .map(|n| (2.0 * std::f32::consts::PI * hz * n as f32 / sr).sin())
            .collect();
        let mut a = Analyzer::new();
        a.push(&samples);
        let display = a.update(sr, 0.033).to_vec();
        let i = nearest_point(&display_freqs(), hz);
        // Hann scalloping loss is at most ~1.4 dB.
        assert!(display[i] > -1.6 && display[i] < 0.5, "got {}", display[i]);
        assert!(display[nearest_point(&display_freqs(), 100.0)] < -60.0);
    }

    #[test]
    fn peaks_decay_at_fixed_rate() {
        let sr = 48_000.0;
        let samples: Vec<f32> = (0..FFT_SIZE)
            .map(|n| (2.0 * std::f32::consts::PI * 1_000.0 * n as f32 / sr).sin())
            .collect();
        let mut a = Analyzer::new();
        a.push(&samples);
        let i = nearest_point(&display_freqs(), 1_000.0);
        let before = a.update(sr, 0.0)[i];
        a.push(&vec![0.0; FFT_SIZE]);
        let after = a.update(sr, 1.0)[i];
        assert!(
            (before - after - DECAY_DB_PER_SEC).abs() < 1e-3,
            "{before} -> {after}"
        );
        // No new audio: keeps decaying.
        let later = a.update(sr, 1.0)[i];
        assert!((after - later - DECAY_DB_PER_SEC).abs() < 1e-3);
    }
}
