//! Renders a WAV file through a preset, so presets can be heard (and measured)
//! offline.
//!
//! ```sh
//! cargo run -p presence-dsp --example render -- --preset clarity in.wav out.wav
//! cargo run -p presence-dsp --example render -- --table a.wav b.wav ...
//! ```
//!
//! Input may be any sample rate and channel count (channels are averaged to
//! mono); the output is 32-bit float mono at the input rate. `--preset`
//! prints loudness and peak compared with `flat`. `--table` renders every
//! built-in preset over all the inputs and prints a Markdown table of the
//! same numbers plus the octave-band spectrum change, without writing audio.
//! The top octave is left open above (at 48 kHz, "16k" is everything from
//! 11.3 kHz up), and octaves the inputs have almost no energy in are skipped.
//!
//! Loudness is K-weighted RMS (ITU-R BS.1770 weighting, ungated) summed over
//! the whole input; comparing two renders of the same input needs no gating.

use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};
use std::process::ExitCode;

use presence_dsp::{
    Band, BandKind, EqSettings, HighPass, HighPassSlope, MAX_BANDS, Preset, Processor,
    builtin_presets,
};

const BLOCK: usize = 512;
const OCTAVES_HZ: [f32; 8] = [
    125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

struct Audio {
    sample_rate: u32,
    samples: Vec<f32>,
}

/// Energies accumulated over one or more inputs.
#[derive(Clone, Copy, Default)]
struct Stats {
    k_energy: f64,
    energy: f64,
    samples: usize,
    peak: f32,
    /// Peak with the limiter bypassed, to show how hard it works.
    peak_unlimited: f32,
    octaves: [f64; OCTAVES_HZ.len()],
    /// Whether each octave starts safely below the Nyquist frequency of every
    /// input.
    octave_valid: [bool; OCTAVES_HZ.len()],
}

impl Stats {
    fn add(&mut self, audio: &Audio, settings: &EqSettings) {
        let rate = audio.sample_rate as f32;
        let out = render(audio, settings);
        let unlimited = render(
            audio,
            &EqSettings {
                limiter: false,
                ..*settings
            },
        );
        let first = self.samples == 0;
        self.samples += out.len();
        self.energy += energy(&out);
        self.k_energy += energy(&filtered(&out, rate, &k_weighting()));
        self.peak = self.peak.max(peak(&out));
        self.peak_unlimited = self.peak_unlimited.max(peak(&unlimited));
        for (i, &centre) in OCTAVES_HZ.iter().enumerate() {
            let valid = centre / SQRT_2 < 0.45 * rate;
            self.octave_valid[i] = valid && (first || self.octave_valid[i]);
            if valid {
                self.octaves[i] += energy(&filtered(&out, rate, &octave_band(centre, rate)));
            }
        }
    }

    fn rms_db(&self) -> f64 {
        power_db(self.energy / self.samples as f64)
    }

    fn k_db(&self) -> f64 {
        power_db(self.k_energy / self.samples as f64)
    }
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("render: {message}");
            eprintln!(
                "usage: render --preset ID IN.wav OUT.wav\n       render --table IN.wav [IN.wav ...]"
            );
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("--preset") => match &args[1..] {
            [id, input, output] => render_one(id, input, output),
            _ => Err("--preset takes an ID, an input and an output path".into()),
        },
        Some("--table") if args.len() > 1 => table(&args[1..]),
        _ => Err("missing arguments".into()),
    }
}

fn find_preset(id: &str) -> Result<Preset, String> {
    let presets = builtin_presets();
    let ids: Vec<&str> = presets.iter().map(|p| p.id).collect();
    let known = ids.join(", ");
    presets
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("unknown preset \"{id}\"; available: {known}"))
}

fn render_one(id: &str, input: &str, output: &str) -> Result<(), String> {
    let preset = find_preset(id)?;
    let audio = read_wav(input)?;
    let out = render(&audio, &preset.settings);
    write_wav(output, audio.sample_rate, &out)?;

    let mut flat = Stats::default();
    flat.add(&audio, &EqSettings::FLAT);
    let mut stats = Stats::default();
    stats.add(&audio, &preset.settings);
    eprintln!(
        "{id}: wrote {output} ({:.1} s at {} Hz)",
        out.len() as f64 / f64::from(audio.sample_rate),
        audio.sample_rate
    );
    eprintln!(
        "  loudness vs flat: {:+.1} dB K-weighted, {:+.1} dB RMS",
        stats.k_db() - flat.k_db(),
        stats.rms_db() - flat.rms_db()
    );
    eprintln!(
        "  peak: {:.1} dBFS (input {:.1} dBFS, {:.1} dBFS before the limiter)",
        amplitude_db(stats.peak),
        amplitude_db(peak(&audio.samples)),
        amplitude_db(stats.peak_unlimited)
    );
    Ok(())
}

fn table(inputs: &[String]) -> Result<(), String> {
    let audio = inputs
        .iter()
        .map(|path| read_wav(path))
        .collect::<Result<Vec<_>, _>>()?;
    let measure = |settings: &EqSettings| {
        let mut stats = Stats::default();
        for a in &audio {
            stats.add(a, settings);
        }
        stats
    };
    let flat = measure(&EqSettings::FLAT);

    // Skip bands holding under -40 dB of the flat energy, e.g. above 11 kHz
    // in speech synthesised at 22.05 kHz: their ratios would be noise.
    let octaves: Vec<usize> = (0..OCTAVES_HZ.len())
        .filter(|&i| flat.octave_valid[i] && flat.octaves[i] > 1e-4 * flat.energy)
        .collect();
    let labels: Vec<String> = octaves
        .iter()
        .map(|&i| match OCTAVES_HZ[i] {
            f if f >= 1_000.0 => format!("{}k", f / 1_000.0),
            f => format!("{f}"),
        })
        .collect();
    println!(
        "| id | K-wtd Δ | RMS Δ | peak | pre-limiter peak | {} |",
        labels.join(" | ")
    );
    println!("|---|---:|---:|---:|---:|{}", "---:|".repeat(labels.len()));
    for preset in builtin_presets() {
        let stats = measure(&preset.settings);
        let bands: Vec<String> = octaves
            .iter()
            .map(|&i| format!("{:+.1}", power_db(stats.octaves[i] / flat.octaves[i])))
            .collect();
        println!(
            "| `{}` | {:+.1} | {:+.1} | {:.1} | {:.1} | {} |",
            preset.id,
            stats.k_db() - flat.k_db(),
            stats.rms_db() - flat.rms_db(),
            amplitude_db(stats.peak),
            amplitude_db(stats.peak_unlimited),
            bands.join(" | ")
        );
    }
    println!(
        "\nflat: {:.1} dB K-weighted RMS, {:.1} dBFS RMS, peak {:.1} dBFS",
        flat.k_db(),
        flat.rms_db(),
        amplitude_db(flat.peak)
    );
    Ok(())
}

fn render(audio: &Audio, settings: &EqSettings) -> Vec<f32> {
    filtered(&audio.samples, audio.sample_rate as f32, settings)
}

fn filtered(samples: &[f32], sample_rate: f32, settings: &EqSettings) -> Vec<f32> {
    let mut processor = Processor::new(sample_rate, settings);
    let mut out = samples.to_vec();
    for block in out.chunks_mut(BLOCK) {
        processor.process(block);
    }
    out
}

/// A filter-only chain (no limiter) made of the given bands.
fn filter_chain(high_pass: Option<(f32, HighPassSlope)>, bands: &[Band]) -> EqSettings {
    let mut settings = EqSettings::FLAT;
    settings.limiter = false;
    if let Some((freq_hz, slope)) = high_pass {
        settings.high_pass = HighPass {
            enabled: true,
            freq_hz,
            slope,
        };
    }
    assert!(bands.len() <= MAX_BANDS);
    settings.bands[..bands.len()].copy_from_slice(bands);
    settings
}

/// ITU-R BS.1770 K-weighting: the analog-matched shelf and high-pass used by
/// common loudness meters, so it works at any sample rate.
fn k_weighting() -> EqSettings {
    filter_chain(
        None,
        &[
            Band {
                enabled: true,
                kind: BandKind::HighShelf,
                freq_hz: 1_681.974_5,
                gain_db: 3.999_843_9,
                q: 0.707_175_2,
            },
            Band {
                enabled: true,
                kind: BandKind::HighPass,
                freq_hz: 38.135_47,
                gain_db: 0.0,
                q: 0.500_327,
            },
        ],
    )
}

/// Octave band around `centre`: 24 dB/oct Linkwitz–Riley edges at ±½ octave.
/// When the upper edge is too close to Nyquist the band is left open above
/// (the "16k" band at 48 kHz covers everything from 11.3 kHz up).
fn octave_band(centre: f32, sample_rate: f32) -> EqSettings {
    let high_pass = Some((centre / SQRT_2, HighPassSlope::Db24));
    if centre * SQRT_2 >= 0.45 * sample_rate {
        return filter_chain(high_pass, &[]);
    }
    let low_pass = Band {
        enabled: true,
        kind: BandKind::LowPass,
        freq_hz: centre * SQRT_2,
        gain_db: 0.0,
        q: FRAC_1_SQRT_2,
    };
    filter_chain(high_pass, &[low_pass, low_pass])
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|&x| f64::from(x).powi(2)).sum()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn power_db(power: f64) -> f64 {
    10.0 * power.max(1e-20).log10()
}

fn amplitude_db(amplitude: f32) -> f64 {
    20.0 * f64::from(amplitude).max(1e-10).log10()
}

fn read_wav(path: &str) -> Result<Audio, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| format!("{path}: {e}"))?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| format!("{path}: {e}"))?
        }
    };
    let samples = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(Audio {
        sample_rate: spec.sample_rate,
        samples,
    })
}

fn write_wav(path: &str, sample_rate: u32, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let error = |e: hound::Error| format!("{path}: {e}");
    let mut writer = hound::WavWriter::create(path, spec).map_err(error)?;
    for &s in samples {
        writer.write_sample(s).map_err(error)?;
    }
    writer.finalize().map_err(error)
}
