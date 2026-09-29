//! Turns user-facing [`EqSettings`] into sanitised per-stage parameters shared
//! by the processor and [`crate::response_db`], so the drawn curve always
//! matches what is heard.

use std::f32::consts::FRAC_1_SQRT_2;

use crate::filter::Prototype;
use crate::{
    Band, BandKind, EqSettings, HighPassSlope, MAX_BANDS, MAX_FREQ_HZ, MAX_GAIN_DB, MAX_Q,
    MIN_FREQ_HZ, MIN_GAIN_DB, MIN_Q,
};

/// High-pass stage followed by the parametric bands.
pub(crate) const STAGES: usize = MAX_BANDS + 1;

/// Keeps filters away from Nyquist, where the bilinear transform squashes
/// their response and `tan` blows up.
const MAX_FREQ_NYQUIST_FRACTION: f32 = 0.95;

const FALLBACK_SAMPLE_RATE: f32 = 48_000.0;

pub(crate) fn sanitize_sample_rate(sample_rate: f32) -> f32 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate.clamp(1_000.0, 1_000_000.0)
    } else {
        FALLBACK_SAMPLE_RATE
    }
}

/// Structural part of a stage: changing it cannot be smoothed, so the
/// processor crossfades through neutral instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub kind: BandKind,
    /// Run the section twice (24 dB/oct high-pass).
    pub cascade: bool,
}

impl Shape {
    /// Peak and shelves fade by sliding their gain to 0 dB; the others have no
    /// neutral setting and fade by dry/wet mix.
    pub(crate) fn has_gain(self) -> bool {
        matches!(
            self.kind,
            BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StageParams {
    pub enabled: bool,
    pub shape: Shape,
    pub freq_hz: f32,
    /// Always 0 for shapes without gain.
    pub gain_db: f32,
    pub q: f32,
}

impl StageParams {
    fn band(band: &Band, sample_rate: f32) -> Self {
        let shape = Shape {
            kind: band.kind,
            cascade: false,
        };
        let gain_db = if shape.has_gain() {
            finite_or(band.gain_db, 0.0).clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        Self {
            enabled: band.enabled,
            shape,
            freq_hz: sanitize_freq(band.freq_hz, sample_rate),
            gain_db,
            q: finite_or(band.q, FRAC_1_SQRT_2).clamp(MIN_Q, MAX_Q),
        }
    }

    pub(crate) fn prototype(&self, sample_rate: f32, gain_db: f32) -> Prototype {
        Prototype::design(
            self.shape.kind,
            f64::from(sample_rate),
            f64::from(self.freq_hz),
            f64::from(self.q),
            f64::from(gain_db),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Resolved {
    pub input_gain_db: f32,
    pub output_gain_db: f32,
    pub limiter: bool,
    pub stages: [StageParams; STAGES],
}

impl Resolved {
    pub(crate) fn new(settings: &EqSettings, sample_rate: f32) -> Self {
        let hp = &settings.high_pass;
        let high_pass = StageParams {
            enabled: hp.enabled,
            shape: Shape {
                kind: BandKind::HighPass,
                cascade: hp.slope == HighPassSlope::Db24,
            },
            freq_hz: sanitize_freq(hp.freq_hz, sample_rate),
            gain_db: 0.0,
            q: FRAC_1_SQRT_2,
        };
        let mut stages = [high_pass; STAGES];
        for (stage, band) in stages[1..].iter_mut().zip(&settings.bands) {
            *stage = StageParams::band(band, sample_rate);
        }
        Self {
            input_gain_db: sanitize_gain(settings.input_gain_db),
            output_gain_db: sanitize_gain(settings.output_gain_db),
            limiter: settings.limiter,
            stages,
        }
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn sanitize_gain(gain_db: f32) -> f32 {
    finite_or(gain_db, 0.0).clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

fn sanitize_freq(freq_hz: f32, sample_rate: f32) -> f32 {
    // The Nyquist bound is applied with `min`, not `clamp`: at low sample rates
    // it can fall below MIN_FREQ_HZ, and `clamp` panics on inverted bounds.
    finite_or(freq_hz, 1_000.0)
        .clamp(MIN_FREQ_HZ, MAX_FREQ_HZ)
        .min(0.5 * sample_rate * MAX_FREQ_NYQUIST_FRACTION)
}

pub(crate) fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}
