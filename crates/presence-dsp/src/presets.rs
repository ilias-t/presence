use std::f32::consts::FRAC_1_SQRT_2;

use crate::{
    Band, BandKind, EqSettings, HighPass, HighPassSlope, MAX_BANDS, Preset, PresetCategory,
};

/// Q values of a 4th-order Butterworth low-pass built from two biquads.
const BUTTERWORTH4_Q: [f32; 2] = [0.541_196_1, 1.306_563];

/// Built-in presets: `flat`, then the voice presets, then the fun ones. Lists
/// group them in this order. See docs/PRESETS.md for the rationale and
/// measurements behind every curve.
///
/// Descriptions are shown to users, so they describe what the preset does.
pub fn builtin_presets() -> Vec<Preset> {
    use PresetCategory::{Fun, Voice};

    vec![
        Preset {
            id: "flat",
            name: "Flat",
            category: Voice,
            description: "No EQ: unity gain; safety limiter only.",
            settings: EqSettings::FLAT,
        },
        Preset {
            id: "natural",
            name: "Natural",
            category: Voice,
            description: "Subtle cleanup: less rumble and mud, your voice as is.",
            settings: eq(
                hp(80.0, HighPassSlope::Db12),
                &[
                    peak(300.0, -1.5, 0.8),
                    peak(4_500.0, 1.0, 0.7),
                    high_shelf(12_000.0, 1.5),
                ],
                0.5,
            ),
        },
        Preset {
            id: "clarity",
            name: "Clarity",
            category: Voice,
            description: "Default. Crisp calls: less boxiness, more presence and air.",
            settings: eq(
                hp(80.0, HighPassSlope::Db24),
                &[
                    peak(280.0, -2.0, 1.0),
                    peak(3_500.0, 2.5, 1.0),
                    high_shelf(10_000.0, 2.5),
                ],
                0.5,
            ),
        },
        Preset {
            id: "broadcast",
            name: "Warm broadcast",
            category: Voice,
            description: "Radio/podcast voice: fuller chest, less mud, smooth top.",
            settings: eq(
                hp(70.0, HighPassSlope::Db12),
                &[
                    low_shelf(150.0, 3.0),
                    peak(400.0, -2.5, 1.2),
                    peak(3_000.0, 1.5, 0.8),
                    high_shelf(12_000.0, 2.0),
                ],
                0.0,
            ),
        },
        Preset {
            id: "laptop",
            name: "Laptop mic",
            category: Voice,
            description: "Built-in mics: less fan rumble and boxiness, more body.",
            settings: eq(
                hp(110.0, HighPassSlope::Db24),
                &[
                    peak(200.0, 2.0, 0.9),
                    peak(550.0, -2.5, 1.2),
                    peak(3_000.0, 1.5, 1.0),
                    high_shelf(10_000.0, 1.5),
                ],
                0.0,
            ),
        },
        Preset {
            id: "headset",
            name: "Headset / AirPods",
            category: Voice,
            description: "Headset and Bluetooth mics: less nasal, clearer words.",
            settings: eq(
                hp(90.0, HighPassSlope::Db12),
                &[
                    peak(400.0, -1.5, 1.0),
                    peak(1_100.0, -1.5, 1.2),
                    peak(3_200.0, 2.0, 0.9),
                    high_shelf(8_000.0, 1.5),
                ],
                0.5,
            ),
        },
        Preset {
            id: "telephone",
            name: "Old telephone",
            category: Fun,
            description: "Squeezes your voice into a 1990s phone line.",
            settings: eq(
                hp(300.0, HighPassSlope::Db24),
                &[
                    low_pass(3_400.0, BUTTERWORTH4_Q[0]),
                    low_pass(3_400.0, BUTTERWORTH4_Q[1]),
                    peak(1_500.0, 4.0, 1.0),
                ],
                3.5,
            ),
        },
        Preset {
            id: "megaphone",
            name: "Megaphone",
            category: Fun,
            description: "Honky, ringing bullhorn. Attention, please.",
            settings: eq(
                hp(450.0, HighPassSlope::Db24),
                &[
                    band(BandKind::HighPass, 450.0, 0.0, FRAC_1_SQRT_2),
                    low_pass(4_000.0, BUTTERWORTH4_Q[0]),
                    low_pass(4_000.0, BUTTERWORTH4_Q[1]),
                    peak(1_100.0, 6.0, 2.5),
                    peak(2_400.0, 5.0, 3.0),
                ],
                4.5,
            ),
        },
        Preset {
            id: "next-room",
            name: "Through the wall",
            category: Fun,
            description: "Muffled, like you're talking from the next room.",
            settings: eq(
                hp(50.0, HighPassSlope::Db12),
                &[
                    low_pass(500.0, BUTTERWORTH4_Q[0]),
                    low_pass(500.0, BUTTERWORTH4_Q[1]),
                    low_pass(500.0, FRAC_1_SQRT_2),
                ],
                2.0,
            ),
        },
        Preset {
            id: "late-night",
            name: "Late-night DJ",
            category: Fun,
            description: "Big, boomy radio bass with extra sizzle.",
            settings: eq(
                hp(45.0, HighPassSlope::Db24),
                &[
                    low_shelf(220.0, 12.0),
                    peak(1_000.0, -8.0, 0.7),
                    high_shelf(5_000.0, 6.0),
                ],
                -4.5,
            ),
        },
    ]
}

fn eq(high_pass: HighPass, bands: &[Band], output_gain_db: f32) -> EqSettings {
    let mut slots = [Band::OFF; MAX_BANDS];
    slots[..bands.len()].copy_from_slice(bands);
    EqSettings {
        high_pass,
        bands: slots,
        output_gain_db,
        ..EqSettings::FLAT
    }
}

fn hp(freq_hz: f32, slope: HighPassSlope) -> HighPass {
    HighPass {
        enabled: true,
        freq_hz,
        slope,
    }
}

fn band(kind: BandKind, freq_hz: f32, gain_db: f32, q: f32) -> Band {
    Band {
        enabled: true,
        kind,
        freq_hz,
        gain_db,
        q,
    }
}

fn peak(freq_hz: f32, gain_db: f32, q: f32) -> Band {
    band(BandKind::Peak, freq_hz, gain_db, q)
}

fn low_pass(freq_hz: f32, q: f32) -> Band {
    band(BandKind::LowPass, freq_hz, 0.0, q)
}

/// Shelves use Q = 1/√2, the cookbook equivalent of slope S = 1.
fn low_shelf(freq_hz: f32, gain_db: f32) -> Band {
    band(BandKind::LowShelf, freq_hz, gain_db, FRAC_1_SQRT_2)
}

fn high_shelf(freq_hz: f32, gain_db: f32) -> Band {
    band(BandKind::HighShelf, freq_hz, gain_db, FRAC_1_SQRT_2)
}
