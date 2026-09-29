use std::collections::HashSet;
use std::f64::consts::PI;

use crate::filter::Prototype;
use crate::params::db_to_gain;
use crate::*;

const FS: f32 = 48_000.0;

fn band(kind: BandKind, freq_hz: f32, gain_db: f32, q: f32) -> Band {
    Band {
        enabled: true,
        kind,
        freq_hz,
        gain_db,
        q,
    }
}

fn with_bands(bands: &[Band]) -> EqSettings {
    let mut settings = EqSettings::FLAT;
    settings.limiter = false;
    settings.bands[..bands.len()].copy_from_slice(bands);
    settings
}

fn with_high_pass(freq_hz: f32, slope: HighPassSlope) -> EqSettings {
    let mut settings = with_bands(&[]);
    settings.high_pass = HighPass {
        enabled: true,
        freq_hz,
        slope,
    };
    settings
}

fn sine(freq_hz: f32, amplitude: f32, len: usize) -> Vec<f32> {
    (0..len)
        .map(|n| {
            amplitude * (2.0 * PI * f64::from(freq_hz) * n as f64 / f64::from(FS)).sin() as f32
        })
        .collect()
}

fn rms(buf: &[f32]) -> f64 {
    (buf.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / buf.len() as f64).sqrt()
}

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} dB, expected {expected} ± {tolerance} dB"
    );
}

/// Logarithmically spaced frequencies across the audible band.
fn sweep() -> impl Iterator<Item = f32> {
    (0..=200).map(|i| 20.0 * 1000f32.powf(i as f32 / 200.0))
}

/// Magnitude of an RBJ Audio EQ Cookbook biquad, straight from the published
/// formulas, as an independent reference for the SVF design.
fn rbj_magnitude(kind: BandKind, freq_hz: f64, q: f64, gain_db: f64, at_hz: f64) -> f64 {
    let fs = f64::from(FS);
    let w0 = 2.0 * PI * freq_hz / fs;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q);
    let a = 10f64.powf(gain_db / 40.0);
    let sa = 2.0 * a.sqrt() * alpha;
    let (b, den) = match kind {
        BandKind::Peak => (
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        ),
        BandKind::LowShelf => (
            [
                a * ((a + 1.0) - (a - 1.0) * cos + sa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sa),
            ],
            [
                (a + 1.0) + (a - 1.0) * cos + sa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sa,
            ],
        ),
        BandKind::HighShelf => (
            [
                a * ((a + 1.0) + (a - 1.0) * cos + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sa),
            ],
            [
                (a + 1.0) - (a - 1.0) * cos + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sa,
            ],
        ),
        BandKind::LowPass => (
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        ),
        BandKind::HighPass => (
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        ),
        BandKind::Notch => (
            [1.0, -2.0 * cos, 1.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        ),
    };
    let w = 2.0 * PI * at_hz / fs;
    let eval = |c: [f64; 3]| {
        let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
        let im = -c[1] * w.sin() - c[2] * (2.0 * w).sin();
        re.hypot(im)
    };
    eval(b) / eval(den)
}

#[test]
fn svf_design_matches_rbj_cookbook() {
    let kinds = [
        BandKind::Peak,
        BandKind::LowShelf,
        BandKind::HighShelf,
        BandKind::LowPass,
        BandKind::HighPass,
        BandKind::Notch,
    ];
    for kind in kinds {
        for (freq, q, gain) in [
            (100.0, 0.707, 6.0),
            (1_000.0, 2.5, -9.0),
            (9_000.0, 0.3, 15.0),
        ] {
            let proto = Prototype::design(kind, f64::from(FS), freq, q, gain);
            for at in [30.0, 250.0, 1_000.0, 4_321.0, 15_000.0, 22_000.0] {
                let ours = proto.magnitude(f64::from(FS), at);
                let rbj = rbj_magnitude(kind, freq, q, gain, at);
                // Relative tolerance; skips the ~0 values at notch/stopband.
                assert!(
                    (ours - rbj).abs() <= 1e-9 * rbj.max(1e-3),
                    "{kind:?} f={freq} q={q} g={gain} at {at}: {ours} vs {rbj}"
                );
            }
        }
    }
}

#[test]
fn peak_hits_its_gain_at_centre() {
    for (freq, gain, q) in [
        (1_000.0, 6.0, 1.0),
        (250.0, -12.0, 4.0),
        (8_000.0, 18.0, 0.5),
    ] {
        let settings = with_bands(&[band(BandKind::Peak, freq, gain, q)]);
        assert_close(response_db(&settings, FS, freq), gain, 0.1, "peak centre");
    }
}

#[test]
fn shelves_reach_gain_deep_in_the_shelf() {
    let low = with_bands(&[band(BandKind::LowShelf, 300.0, 6.0, 0.707)]);
    assert_close(response_db(&low, FS, 20.0), 6.0, 0.1, "low shelf at 20 Hz");
    assert_close(
        response_db(&low, FS, 10_000.0),
        0.0,
        0.1,
        "low shelf at 10 kHz",
    );
    assert_close(response_db(&low, FS, 300.0), 3.0, 0.1, "low shelf midpoint");

    let high = with_bands(&[band(BandKind::HighShelf, 3_000.0, -8.0, 0.707)]);
    assert_close(
        response_db(&high, FS, 20_000.0),
        -8.0,
        0.1,
        "high shelf at 20 kHz",
    );
    assert_close(
        response_db(&high, FS, 50.0),
        0.0,
        0.1,
        "high shelf at 50 Hz",
    );
    assert_close(
        response_db(&high, FS, 3_000.0),
        -4.0,
        0.1,
        "high shelf midpoint",
    );
}

#[test]
fn high_pass_slopes() {
    let hp12 = with_high_pass(100.0, HighPassSlope::Db12);
    assert_close(
        response_db(&hp12, FS, 100.0),
        -3.01,
        0.05,
        "12 dB/oct at cutoff",
    );
    // Butterworth: |H|² = Ω⁴ / (1 + Ω⁴) → -12.3 dB one octave down.
    assert_close(
        response_db(&hp12, FS, 50.0),
        -12.3,
        0.1,
        "12 dB/oct one octave down",
    );
    assert_close(
        response_db(&hp12, FS, 2_000.0),
        0.0,
        0.01,
        "12 dB/oct passband",
    );

    // Two cascaded Butterworth sections (LR4): -6 dB at cutoff.
    let hp24 = with_high_pass(100.0, HighPassSlope::Db24);
    assert_close(
        response_db(&hp24, FS, 100.0),
        -6.02,
        0.1,
        "24 dB/oct at cutoff",
    );
    assert!(response_db(&hp24, FS, 50.0) <= -23.0);
    assert!(response_db(&hp24, FS, 25.0) <= -47.0);
    assert_close(
        response_db(&hp24, FS, 2_000.0),
        0.0,
        0.01,
        "24 dB/oct passband",
    );
}

#[test]
fn disabled_stages_and_gains() {
    let mut settings = with_bands(&[Band {
        enabled: false,
        ..band(BandKind::Peak, 1_000.0, 12.0, 1.0)
    }]);
    settings.high_pass.enabled = false;
    settings.input_gain_db = 4.0;
    settings.output_gain_db = -1.5;
    for f in sweep() {
        assert_close(response_db(&settings, FS, f), 2.5, 1e-4, "gains only");
    }
}

/// Runs a sine through a fresh processor and returns the measured gain in dB.
/// The measurement window starts 50 ms in: long enough for the filters' own
/// transients, too short for a processor that smooths in from flat.
fn measured_gain_db(settings: &EqSettings, freq_hz: f32) -> f32 {
    let input = sine(freq_hz, 0.1, (0.15 * FS) as usize);
    let mut output = input.clone();
    let mut processor = Processor::new(FS, settings);
    // Odd chunk size so ticks straddle buffer boundaries.
    for chunk in output.chunks_mut(97) {
        processor.process(chunk);
    }
    let window = (0.05 * FS) as usize..input.len();
    (20.0 * (rms(&output[window.clone()]) / rms(&input[window])).log10()) as f32
}

#[test]
fn processor_matches_response() {
    let mut settings = with_bands(&[
        band(BandKind::Peak, 1_000.0, 6.0, 1.4),
        band(BandKind::LowShelf, 150.0, -4.0, 0.707),
        band(BandKind::HighShelf, 8_000.0, 3.0, 0.707),
        band(BandKind::Notch, 3_000.0, 0.0, 8.0),
        band(BandKind::LowPass, 15_000.0, 0.0, 0.707),
        band(BandKind::Peak, 400.0, -3.0, 2.0),
    ]);
    settings.high_pass = HighPass {
        enabled: true,
        freq_hz: 80.0,
        slope: HighPassSlope::Db24,
    };
    settings.input_gain_db = 3.0;
    settings.output_gain_db = -2.0;
    for freq in [100.0, 400.0, 1_000.0, 5_000.0, 10_000.0] {
        let expected = response_db(&settings, FS, freq);
        let measured = measured_gain_db(&settings, freq);
        assert_close(measured, expected, 0.2, &format!("measured at {freq} Hz"));
    }
}

#[test]
fn output_is_independent_of_buffer_size() {
    let settings = with_bands(&[band(BandKind::Peak, 1_000.0, 6.0, 1.4)]);
    let mut changed = settings;
    changed.bands[0].gain_db = -6.0;
    changed.bands[1] = band(BandKind::HighPass, 300.0, 0.0, 0.707);
    let input = sine(700.0, 0.3, 4_800);

    let render = |chunk: usize| {
        let mut processor = Processor::new(FS, &settings);
        let mut out = input.clone();
        let (a, b) = out.split_at_mut(1_000);
        a.chunks_mut(chunk).for_each(|c| processor.process(c));
        processor.set_settings(&changed);
        b.chunks_mut(chunk).for_each(|c| processor.process(c));
        out
    };
    let reference = render(4_800);
    for chunk in [1, 7, 32, 100, 512] {
        assert_eq!(render(chunk), reference, "chunk size {chunk}");
    }
}

#[test]
fn limiter_holds_the_ceiling() {
    let ceiling = db_to_gain(LIMITER_CEILING_DB);
    let mut settings = with_bands(&[]);
    settings.limiter = true;
    let mut processor = Processor::new(FS, &settings);

    // Silence, then a +12 dBFS sine arriving without warning.
    let mut buf = vec![0.0; 4_800];
    buf.extend(sine(1_000.0, db_to_gain(12.0), 48_000));
    processor.process(&mut buf);
    let peak = buf.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak <= ceiling, "peak {peak} above ceiling {ceiling}");
    // It limits rather than mutes: the settled level sits just under the ceiling.
    let settled_peak = buf[40_000..].iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(settled_peak > 0.97 * ceiling, "settled peak {settled_peak}");

    // Quiet material passes untouched.
    let quiet = sine(1_000.0, 0.25, 48_000);
    let mut out = quiet.clone();
    processor.reset();
    processor.process(&mut out);
    assert!(quiet.iter().zip(&out).all(|(a, b)| (a - b).abs() < 1e-6));
}

#[test]
fn extreme_parameters_stay_finite() {
    let mut hot = EqSettings::FLAT;
    hot.limiter = false;
    hot.input_gain_db = f32::INFINITY;
    hot.output_gain_db = 1e9;
    hot.high_pass = HighPass {
        enabled: true,
        freq_hz: f32::NAN,
        slope: HighPassSlope::Db24,
    };
    let kinds = [
        BandKind::Peak,
        BandKind::LowShelf,
        BandKind::HighShelf,
        BandKind::LowPass,
        BandKind::HighPass,
        BandKind::Notch,
    ];
    let wild = [
        (f32::NAN, f32::NAN, f32::NAN),
        (1e9, 1e9, 1e9),
        (-5.0, -1e9, 0.0),
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY),
        (19_999.0, MAX_GAIN_DB, MAX_Q),
        (20.0, MAX_GAIN_DB, MIN_Q),
        (15_000.0, MIN_GAIN_DB, MAX_Q),
        (30.0, MAX_GAIN_DB, MAX_Q),
    ];
    for (i, (freq, gain, q)) in wild.into_iter().enumerate() {
        hot.bands[i] = band(kinds[i % kinds.len()], freq, gain, q);
    }
    let mut limited = hot;
    limited.limiter = true;

    for sample_rate in [FS, 44_100.0, 8_000.0, f32::NAN, 0.0, -1.0, f32::INFINITY] {
        for settings in [hot, limited, EqSettings::FLAT] {
            let mut processor = Processor::new(sample_rate, &settings);
            let mut buf = sine(40.0, 1.0, 9_600);
            buf[100] = f32::NAN;
            buf[200] = f32::INFINITY;
            buf[300] = 1e30;
            buf[400] = f32::MIN_POSITIVE / 8.0;
            processor.process(&mut buf);
            processor.set_settings(&EqSettings::FLAT);
            processor.process(&mut buf);
            processor.set_settings(&settings);
            processor.process(&mut buf);
            assert!(buf.iter().all(|x| x.is_finite()), "fs {sample_rate}");

            for f in [f32::NAN, -1.0, 0.0, 1.0, 1_000.0, 23_999.0, 24_000.0, 1e9] {
                let db = response_db(&settings, sample_rate, f);
                assert!(db.is_finite(), "response at {f} Hz, fs {sample_rate}: {db}");
            }
        }
    }
}

/// Largest sample-to-sample step; a click shows up as a step comparable to
/// the signal amplitude, while a clean sine at `f` never steps by more than
/// `2π·f/fs` times its peak.
fn max_step(buf: &[f32]) -> f32 {
    buf.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()))
}

#[test]
fn parameter_changes_do_not_click() {
    let tone = 150.0;
    let base = with_bands(&[band(BandKind::Peak, 150.0, 12.0, 1.0)]);

    let mut cut = base;
    cut.bands[0] = band(BandKind::Peak, 150.0, -12.0, 4.0);
    cut.input_gain_db = -6.0;
    cut.high_pass = HighPass {
        enabled: true,
        freq_hz: 120.0,
        slope: HighPassSlope::Db12,
    };

    // Shape changes: peak → high-pass on the same slot, 12 → 24 dB/oct.
    let mut reshaped = cut;
    reshaped.bands[0] = band(BandKind::HighPass, 250.0, 0.0, 0.707);
    reshaped.bands[1] = band(BandKind::Notch, 150.0, 0.0, 2.0);
    reshaped.high_pass.slope = HighPassSlope::Db24;

    let mut disabled = reshaped;
    disabled.bands[0].enabled = false;
    disabled.bands[1].enabled = false;
    disabled.high_pass.enabled = false;
    disabled.output_gain_db = 12.0;

    let schedule = [base, cut, reshaped, disabled, base];
    // Not a whole number of periods, so changes land away from zero crossings.
    let segment = 12_345;
    let input = sine(tone, 0.2, segment * schedule.len());

    let mut smooth = input.clone();
    let mut processor = Processor::new(FS, &base);
    for (settings, chunk) in schedule.iter().zip(smooth.chunks_mut(segment)) {
        processor.set_settings(settings);
        for block in chunk.chunks_mut(128) {
            processor.process(block);
        }
    }
    let peak = smooth.iter().fold(0f32, |m, x| m.max(x.abs()));
    let sine_step = (2.0 * PI * f64::from(tone) / f64::from(FS)) as f32 * peak;
    let step = max_step(&smooth);
    assert!(
        step <= 1.25 * sine_step,
        "max step {step} vs clean-sine bound {sine_step}"
    );

    // Sanity check that the metric catches clicks: switching settings by
    // swapping in a fresh processor produces a step well above the bound.
    let mut hard = input.clone();
    for (settings, chunk) in schedule.iter().zip(hard.chunks_mut(segment)) {
        Processor::new(FS, settings).process(chunk);
    }
    assert!(max_step(&hard) > 3.0 * sine_step);
}

#[test]
fn unchanged_settings_are_a_no_op() {
    let settings = with_bands(&[band(BandKind::Peak, 1_000.0, 6.0, 1.4)]);
    let input = sine(1_000.0, 0.3, 4_800);
    let mut a = input.clone();
    let mut b = input;
    let mut p = Processor::new(FS, &settings);
    let mut q = Processor::new(FS, &settings);
    for (ca, cb) in a.chunks_mut(64).zip(b.chunks_mut(64)) {
        p.process(ca);
        q.set_settings(&settings);
        q.process(cb);
    }
    assert_eq!(a, b);
}

#[test]
fn reset_snaps_to_new_settings() {
    let boosted = with_bands(&[band(BandKind::Peak, 1_000.0, 6.0, 1.0)]);
    let mut processor = Processor::new(FS, &EqSettings::FLAT);
    processor.set_settings(&boosted);
    processor.reset();
    let input = sine(1_000.0, 0.1, 4_800);
    let mut out = input.clone();
    processor.process(&mut out);
    let gain = 20.0 * (rms(&out[480..]) / rms(&input[480..])).log10();
    assert!((gain - 6.0).abs() < 0.1, "gain after reset: {gain}");
}

#[test]
fn preset_ids_are_unique() {
    let presets = builtin_presets();
    let ids: HashSet<_> = presets.iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), presets.len());
    assert_eq!(presets[0].id, "flat");
    assert_eq!(presets[0].settings, EqSettings::FLAT);
    assert!(presets.iter().all(|p| p.settings.limiter));
    assert!(presets.iter().any(|p| p.id == "clarity"), "default preset");

    let voice = presets
        .iter()
        .filter(|p| p.category == PresetCategory::Voice)
        .count();
    assert_eq!(voice, 6, "flat + 5 voice presets");
    assert_eq!(presets.len() - voice, 4, "fun presets");
    // Lists show sections in this order.
    let first_fun = presets
        .iter()
        .position(|p| p.category == PresetCategory::Fun)
        .unwrap();
    assert!(
        presets[first_fun..]
            .iter()
            .all(|p| p.category == PresetCategory::Fun)
    );
}

#[test]
fn presets_are_sane() {
    for preset in builtin_presets() {
        let s = &preset.settings;
        let id = preset.id;
        let voice = preset.category == PresetCategory::Voice;
        // The high-pass rightly drives the deep bass far down; voice presets
        // must stay subtle from an octave above its cutoff.
        let subtle_from = if s.high_pass.enabled {
            2.0 * s.high_pass.freq_hz
        } else {
            0.0
        };
        for f in sweep() {
            let db = response_db(s, FS, f);
            assert!(db.is_finite(), "{id}: {f} Hz");
            assert!(db <= 15.0, "{id}: {db} dB at {f} Hz");
            // Voice curves: restrained moves around the output gain.
            let shape = db - s.output_gain_db;
            if voice && f >= subtle_from && f <= 16_000.0 {
                assert!((-4.0..=4.0).contains(&shape), "{id}: {shape} dB at {f} Hz");
            }
        }
        if voice {
            assert!(s.output_gain_db.abs() <= 2.0, "{id}: output gain");
            for b in s.bands.iter().filter(|b| b.enabled) {
                assert!(b.gain_db.abs() <= 4.0, "{id}: {b:?}");
                assert!(b.q <= 2.0, "{id}: no narrow boosts or cuts: {b:?}");
                assert!(
                    matches!(
                        b.kind,
                        BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf
                    ),
                    "{id}: {b:?}"
                );
            }
            if id != "flat" {
                assert!(s.high_pass.enabled, "{id}: low cut");
                assert!(
                    (60.0..=120.0).contains(&s.high_pass.freq_hz),
                    "{id}: high-pass"
                );
            }
        }

        let mut buf = sine(440.0, 0.5, 4_800);
        Processor::new(FS, s).process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()), "{id}");
    }
}

/// Two seconds of noise with a long-term speech-like spectrum: rolled off
/// below 100 Hz and falling ~6–12 dB/oct above 500 Hz, close to the ANSI
/// S3.5 speech spectrum.
fn speech_shaped_noise() -> Vec<f32> {
    let mut state = 0x2545_f491_u32;
    let mut white = || {
        // xorshift32, uniform in [-1, 1).
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / 2_147_483_648.0 - 1.0
    };
    let pole = |hz: f32| 1.0 - (-2.0 * std::f32::consts::PI * hz / FS).exp();
    let (a1, a2) = (pole(500.0), pole(3_000.0));
    let (mut y1, mut y2) = (0.0, 0.0);
    let mut noise: Vec<f32> = (0..2 * FS as usize)
        .map(|_| {
            y1 += a1 * (white() - y1);
            y2 += a2 * (y1 - y2);
            y2
        })
        .collect();
    Processor::new(FS, &with_high_pass(100.0, HighPassSlope::Db12)).process(&mut noise);
    // About -24 dBFS RMS, a typical speaking level.
    let gain = 0.063 / rms(&noise) as f32;
    noise.iter_mut().for_each(|x| *x *= gain);
    noise
}

/// K-weighted RMS in dB (ITU-R BS.1770 weighting, ungated).
fn k_weighted_db(buf: &[f32]) -> f64 {
    let mut k = with_bands(&[
        band(BandKind::HighShelf, 1_681.974_5, 3.999_843_9, 0.707_175_2),
        band(BandKind::HighPass, 38.135_47, 0.0, 0.500_327),
    ]);
    k.limiter = false;
    let mut weighted = buf.to_vec();
    Processor::new(FS, &k).process(&mut weighted);
    20.0 * rms(&weighted).log10()
}

#[test]
fn presets_are_loudness_matched_to_flat() {
    let noise = speech_shaped_noise();
    let loudness = |settings: &EqSettings| {
        let mut buf = noise.clone();
        Processor::new(FS, settings).process(&mut buf);
        k_weighted_db(&buf)
    };
    let flat = loudness(&EqSettings::FLAT);
    let mut report = String::new();
    let mut ok = true;
    for preset in builtin_presets() {
        let delta = loudness(&preset.settings) - flat;
        // Output gains are matched on recorded speech (docs/PRESETS.md). Noise
        // is a good stand-in for the voice presets; the fun ones keep only a
        // narrow slice of the spectrum, where its balance differs from real
        // speech by a few dB, so they only need to be in the right ballpark.
        let tolerance = match preset.category {
            PresetCategory::Voice => 1.5,
            PresetCategory::Fun => 5.0,
        };
        ok &= delta.abs() <= tolerance;
        report += &format!("{}: {delta:+.2} dB (± {tolerance})\n", preset.id);
    }
    assert!(ok, "loudness vs flat:\n{report}");
}
