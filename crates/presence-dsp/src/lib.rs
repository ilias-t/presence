//! Real-time voice EQ DSP for presence.
//!
//! Everything here is pure computation: no I/O, no threads, no allocation in
//! [`Processor::process`] or [`Processor::set_settings`]. The audio engine owns a
//! [`Processor`] on its real-time thread; the UI uses [`response_db`] to draw
//! the EQ curve and [`builtin_presets`] to populate the preset list.

use serde::{Deserialize, Serialize};

mod filter;
mod params;
mod presets;
mod processor;
#[cfg(test)]
mod tests;

pub use presets::builtin_presets;
pub use processor::Processor;

/// Number of parametric bands in an [`EqSettings`]. Fixed so settings are `Copy`
/// and can cross to the audio thread without allocating.
pub const MAX_BANDS: usize = 8;

pub const MIN_FREQ_HZ: f32 = 20.0;
pub const MAX_FREQ_HZ: f32 = 20_000.0;
pub const MIN_GAIN_DB: f32 = -24.0;
pub const MAX_GAIN_DB: f32 = 24.0;
pub const MIN_Q: f32 = 0.1;
pub const MAX_Q: f32 = 18.0;

/// Output ceiling of the safety limiter, in dBFS.
pub const LIMITER_CEILING_DB: f32 = -1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub enabled: bool,
    pub kind: BandKind,
    pub freq_hz: f32,
    /// Ignored by `LowPass`, `HighPass` and `Notch`.
    pub gain_db: f32,
    /// For shelves this is the RBJ shelf slope-equivalent Q (0.707 ≈ S=1).
    pub q: f32,
}

impl Band {
    /// A disabled flat peak band, used to pad unused slots.
    pub const OFF: Band = Band {
        enabled: false,
        kind: BandKind::Peak,
        freq_hz: 1_000.0,
        gain_db: 0.0,
        q: 1.0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighPassSlope {
    /// One Butterworth biquad.
    Db12,
    /// Two cascaded Butterworth biquads (Linkwitz–Riley style 4th order).
    Db24,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HighPass {
    pub enabled: bool,
    pub freq_hz: f32,
    pub slope: HighPassSlope,
}

/// Complete processing parameters. `Copy` on purpose: the engine hands whole
/// snapshots to the audio thread.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqSettings {
    pub input_gain_db: f32,
    pub high_pass: HighPass,
    pub bands: [Band; MAX_BANDS],
    pub output_gain_db: f32,
    /// Brickwall-ish safety limiter at -1 dBFS after the EQ.
    pub limiter: bool,
}

impl EqSettings {
    /// Unity gain, every stage off except the limiter.
    pub const FLAT: EqSettings = EqSettings {
        input_gain_db: 0.0,
        high_pass: HighPass {
            enabled: false,
            freq_hz: 80.0,
            slope: HighPassSlope::Db12,
        },
        bands: [Band::OFF; MAX_BANDS],
        output_gain_db: 0.0,
        limiter: true,
    };
}

impl Default for EqSettings {
    fn default() -> Self {
        Self::FLAT
    }
}

/// How a built-in preset is grouped in preset lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetCategory {
    /// Voice treatments meant for everyday calls and streaming.
    Voice,
    /// Deliberately exaggerated effects, handy for checking that audio really
    /// flows through presence.
    Fun,
}

impl PresetCategory {
    /// Section heading used by the TUI and CLI.
    pub fn label(self) -> &'static str {
        match self {
            PresetCategory::Voice => "VOICE",
            PresetCategory::Fun => "FUN",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// Stable identifier used on the CLI and in config files, e.g. `clarity`.
    pub id: &'static str,
    pub name: &'static str,
    pub category: PresetCategory,
    pub description: &'static str,
    pub settings: EqSettings,
}

/// Magnitude response in dB of the whole chain (gains, high-pass, bands;
/// excluding the limiter) at `freq_hz`. Used to draw the EQ curve.
pub fn response_db(settings: &EqSettings, sample_rate: f32, freq_hz: f32) -> f32 {
    // Floors each stage so notch centres and DC through a high-pass stay finite.
    const MIN_MAGNITUDE: f64 = 1e-10;

    let sample_rate = params::sanitize_sample_rate(sample_rate);
    let resolved = params::Resolved::new(settings, sample_rate);
    let nyquist = 0.5 * f64::from(sample_rate);
    // `max` first so NaN maps to the lower bound.
    let freq = f64::from(freq_hz).max(1e-3).min(nyquist * (1.0 - 1e-6));

    let mut db = f64::from(resolved.input_gain_db + resolved.output_gain_db);
    for stage in resolved.stages.iter().filter(|s| s.enabled) {
        let magnitude = stage
            .prototype(sample_rate, stage.gain_db)
            .magnitude(f64::from(sample_rate), freq)
            .max(MIN_MAGNITUDE);
        let sections = if stage.shape.cascade { 2.0 } else { 1.0 };
        db += sections * 20.0 * magnitude.log10();
    }
    db as f32
}
