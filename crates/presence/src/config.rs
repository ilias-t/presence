//! Persistent settings at `~/Library/Application Support/presence/config.toml`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use presence_dsp::{
    Band, EqSettings, HighPass, MAX_BANDS, MAX_FREQ_HZ, MAX_GAIN_DB, MAX_Q, MIN_FREQ_HZ,
    MIN_GAIN_DB, MIN_Q,
};
use serde::{Deserialize, Serialize};

use crate::presets::UserPreset;

pub const DEFAULT_PRESET: &str = "clarity";
/// `active_preset` value for settings that don't match a saved preset.
pub const CUSTOM: &str = "custom";

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Config {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Built-in preset id, user preset name, or [`CUSTOM`].
    pub active_preset: String,
    /// The preset a custom setting was derived from, for display.
    pub custom_base: Option<String>,
    pub bypass: bool,
    /// Whether the user has already seen the "select the virtual device" hint.
    pub onboarded: bool,
    /// `None` means "use the active preset's settings".
    pub settings: Option<EqSettings>,
    pub user_presets: Vec<UserPreset>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            active_preset: DEFAULT_PRESET.to_owned(),
            custom_base: None,
            bypass: false,
            onboarded: false,
            settings: None,
            user_presets: Vec::new(),
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("presence").join("config.toml"))
}

/// Result of [`load_from`]. Loading never fails and never touches the file.
#[derive(Debug, Default)]
pub struct Loaded {
    pub config: Config,
    /// Problems worth telling the user about (one line each).
    pub warnings: Vec<String>,
    /// The file exists but couldn't be parsed. Whoever is about to overwrite it
    /// (the TUI's autosave) should call [`back_up_corrupt`] first.
    pub corrupt: bool,
}

/// Loads the config without side effects: a missing file yields defaults
/// silently; an unreadable or invalid one yields defaults plus a warning.
/// Hand-edited files are accepted as far as possible: missing fields take
/// defaults, fewer than [`MAX_BANDS`] bands are padded with [`Band::OFF`], more
/// are truncated, and out-of-range values are clamped.
pub fn load_from(path: &Path) -> Loaded {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => {
            return Loaded {
                warnings: vec![format!(
                    "couldn't read {}: {e}; using defaults",
                    path.display()
                )],
                ..Loaded::default()
            };
        }
    };
    match toml::from_str::<RawConfig>(&text) {
        Ok(raw) => {
            let mut warnings = Vec::new();
            let config = raw.into_config(&mut warnings);
            Loaded {
                config,
                warnings,
                corrupt: false,
            }
        }
        Err(e) => Loaded {
            warnings: vec![format!(
                "config {} is invalid ({}); using defaults",
                path.display(),
                e.message()
            )],
            corrupt: true,
            ..Loaded::default()
        },
    }
}

/// Moves an unparseable config aside to `config.toml.corrupt` so the next save
/// doesn't destroy whatever the user had in there. Returns the backup path.
pub fn back_up_corrupt(path: &Path) -> io::Result<PathBuf> {
    let backup = path.with_extension("toml.corrupt");
    fs::rename(path, &backup)?;
    Ok(backup)
}

/// Warns about an `active_preset` that names no built-in or user preset.
pub fn check_active_preset(config: &Config, builtin_ids: &[&str]) -> Option<String> {
    let key = config.active_preset.as_str();
    let known = key == CUSTOM
        || builtin_ids.iter().any(|id| id.eq_ignore_ascii_case(key))
        || config
            .user_presets
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(key));
    (!known).then(|| {
        let fallback = if config.settings.is_some() {
            "keeping the saved settings as custom"
        } else {
            "using the default preset"
        };
        format!("unknown active_preset \"{key}\" in config; {fallback}")
    })
}

/// On-disk shape of [`Config`], more forgiving than the in-memory one.
/// `EqSettings` lives in the DSP crate with a fixed `[Band; MAX_BANDS]`, so the
/// tolerant parsing happens here and is converted afterwards.
#[derive(Deserialize)]
#[serde(default)]
struct RawConfig {
    input_device: Option<String>,
    output_device: Option<String>,
    active_preset: String,
    custom_base: Option<String>,
    bypass: bool,
    onboarded: bool,
    settings: Option<RawEqSettings>,
    user_presets: Vec<RawUserPreset>,
}

impl Default for RawConfig {
    fn default() -> Self {
        let c = Config::default();
        Self {
            input_device: c.input_device,
            output_device: c.output_device,
            active_preset: c.active_preset,
            custom_base: c.custom_base,
            bypass: c.bypass,
            onboarded: c.onboarded,
            settings: None,
            user_presets: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
struct RawUserPreset {
    name: String,
    settings: RawEqSettings,
}

#[derive(Deserialize)]
#[serde(default)]
struct RawEqSettings {
    input_gain_db: f32,
    high_pass: HighPass,
    bands: Vec<Band>,
    output_gain_db: f32,
    limiter: bool,
}

impl Default for RawEqSettings {
    fn default() -> Self {
        let flat = EqSettings::FLAT;
        Self {
            input_gain_db: flat.input_gain_db,
            high_pass: flat.high_pass,
            bands: Vec::new(),
            output_gain_db: flat.output_gain_db,
            limiter: flat.limiter,
        }
    }
}

impl RawEqSettings {
    fn into_settings(self, what: &str, warnings: &mut Vec<String>) -> EqSettings {
        if self.bands.len() > MAX_BANDS {
            warnings.push(format!(
                "{what} has {} bands; only the first {MAX_BANDS} are used",
                self.bands.len()
            ));
        }
        let mut bands = [Band::OFF; MAX_BANDS];
        for (slot, band) in bands.iter_mut().zip(self.bands) {
            *slot = band;
        }
        sanitize(EqSettings {
            input_gain_db: self.input_gain_db,
            high_pass: self.high_pass,
            bands,
            output_gain_db: self.output_gain_db,
            limiter: self.limiter,
        })
    }
}

impl RawConfig {
    fn into_config(self, warnings: &mut Vec<String>) -> Config {
        let settings = self
            .settings
            .map(|s| s.into_settings("saved settings", warnings));
        let user_presets = self
            .user_presets
            .into_iter()
            .map(|p| {
                let what = format!("user preset \"{}\"", p.name);
                UserPreset {
                    settings: p.settings.into_settings(&what, warnings),
                    name: p.name,
                }
            })
            .collect();
        Config {
            input_device: self.input_device,
            output_device: self.output_device,
            active_preset: self.active_preset,
            custom_base: self.custom_base,
            bypass: self.bypass,
            onboarded: self.onboarded,
            settings,
            user_presets,
        }
    }
}

/// Writes atomically (temp file + rename) so a crash mid-save can't leave a
/// truncated config behind.
pub fn save_to(path: &Path, config: &Config) -> Result<()> {
    let dir = path
        .parent()
        .context("config path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = toml::to_string_pretty(config).context("serializing config")?;
    let tmp = path.with_extension("toml.tmp");
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn clamp_or(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        fallback
    }
}

/// Brings hand-edited values back into the ranges the DSP supports.
pub fn sanitize(mut s: EqSettings) -> EqSettings {
    let flat = EqSettings::FLAT;
    s.input_gain_db = clamp_or(s.input_gain_db, MIN_GAIN_DB, MAX_GAIN_DB, 0.0);
    s.output_gain_db = clamp_or(s.output_gain_db, MIN_GAIN_DB, MAX_GAIN_DB, 0.0);
    s.high_pass.freq_hz = clamp_or(
        s.high_pass.freq_hz,
        MIN_FREQ_HZ,
        crate::app::MAX_HIGH_PASS_HZ,
        flat.high_pass.freq_hz,
    );
    for band in s.bands.iter_mut().take(MAX_BANDS) {
        let off = Band::OFF;
        band.freq_hz = clamp_or(band.freq_hz, MIN_FREQ_HZ, MAX_FREQ_HZ, off.freq_hz);
        band.gain_db = clamp_or(band.gain_db, MIN_GAIN_DB, MAX_GAIN_DB, 0.0);
        band.q = clamp_or(band.q, MIN_Q, MAX_Q, off.q);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use presence_dsp::{BandKind, HighPassSlope};

    fn sample_config() -> Config {
        let mut settings = EqSettings::FLAT;
        settings.high_pass.enabled = true;
        settings.high_pass.slope = HighPassSlope::Db24;
        settings.bands[0] = Band {
            enabled: true,
            kind: BandKind::HighShelf,
            freq_hz: 10_000.0,
            gain_db: 2.5,
            q: 0.707,
        };
        Config {
            input_device: Some("MacBook Pro Microphone".into()),
            output_device: Some("BlackHole 2ch".into()),
            active_preset: CUSTOM.into(),
            custom_base: Some("clarity".into()),
            bypass: true,
            onboarded: true,
            settings: Some(settings),
            user_presets: vec![UserPreset {
                name: "Podcast".into(),
                settings,
            }],
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = sample_config();
        save_to(&path, &config).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(!loaded.corrupt);
        assert_eq!(loaded.config, config);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file must be renamed away"
        );
    }

    #[test]
    fn missing_file_gives_defaults_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_from(&dir.path().join("config.toml"));
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.config.active_preset, DEFAULT_PRESET);
        assert!(loaded.warnings.is_empty());
        assert!(!loaded.corrupt);
    }

    #[test]
    fn corrupt_file_falls_back_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let junk = "bypass = \"definitely not a bool\"\n[[[";
        fs::write(&path, junk).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.corrupt);
        assert!(loaded.warnings[0].contains("invalid"));
        // Loading is read-only: `presets` and `run` must never move the file.
        assert_eq!(fs::read_to_string(&path).unwrap(), junk);
        assert!(!dir.path().join("config.toml.corrupt").exists());

        // Only an explicit backup (the TUI's save path) moves it aside.
        let backup = back_up_corrupt(&path).unwrap();
        assert_eq!(backup, dir.path().join("config.toml.corrupt"));
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(backup).unwrap(), junk);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "bypass = true\n").unwrap();
        let Loaded {
            config, warnings, ..
        } = load_from(&path);
        assert!(warnings.is_empty());
        assert!(config.bypass);
        assert_eq!(config.active_preset, DEFAULT_PRESET);
        assert_eq!(config.settings, None);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = sample_config();
        let s = config.settings.as_mut().unwrap();
        s.bands[0].gain_db = 99.0;
        s.bands[1].freq_hz = 5.0;
        s.bands[2].q = 1000.0;
        save_to(&path, &config).unwrap();
        let s = load_from(&path).config.settings.unwrap();
        assert_eq!(s.bands[0].gain_db, MAX_GAIN_DB);
        assert_eq!(s.bands[1].freq_hz, MIN_FREQ_HZ);
        assert_eq!(s.bands[2].q, MAX_Q);
    }

    const BAND: &str =
        "enabled = true\nkind = \"peak\"\nfreq_hz = 1000.0\ngain_db = 3.0\nq = 1.0\n";

    fn settings_toml(bands: usize) -> String {
        let mut text = String::from(
            "[settings]\ninput_gain_db = 0.0\noutput_gain_db = 0.0\nlimiter = true\n\
             [settings.high_pass]\nenabled = true\nfreq_hz = 90.0\nslope = \"db12\"\n",
        );
        for _ in 0..bands {
            text.push_str("[[settings.bands]]\n");
            text.push_str(BAND);
        }
        text
    }

    #[test]
    fn fewer_bands_are_padded_with_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, settings_toml(3)).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let s = loaded.config.settings.unwrap();
        assert!(s.bands[..3].iter().all(|b| b.enabled && b.gain_db == 3.0));
        assert!(s.bands[3..].iter().all(|b| *b == Band::OFF));
    }

    #[test]
    fn extra_bands_are_truncated_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, settings_toml(10)).unwrap();
        let loaded = load_from(&path);
        assert!(!loaded.corrupt);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(
            loaded.warnings[0].contains("10 bands"),
            "{:?}",
            loaded.warnings
        );
        let s = loaded.config.settings.unwrap();
        assert!(s.bands.iter().all(|b| b.enabled));
    }

    #[test]
    fn missing_settings_fields_take_flat_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[settings]\ninput_gain_db = 2.0\n").unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let s = loaded.config.settings.unwrap();
        assert_eq!(s.input_gain_db, 2.0);
        assert_eq!(s.bands, EqSettings::FLAT.bands);
        assert!(s.limiter);
    }

    #[test]
    fn user_preset_bands_are_tolerated_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = format!(
            "[[user_presets]]\nname = \"Mine\"\n[user_presets.settings]\n\
             [[user_presets.settings.bands]]\n{BAND}"
        );
        fs::write(&path, text).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let p = &loaded.config.user_presets[0];
        assert_eq!(p.name, "Mine");
        assert!(p.settings.bands[0].enabled);
        assert_eq!(p.settings.bands[1], Band::OFF);
    }

    #[test]
    fn unknown_active_preset_is_reported() {
        let ids = ["flat", "clarity"];
        let mut config = Config::default();
        assert_eq!(check_active_preset(&config, &ids), None);
        config.active_preset = CUSTOM.into();
        assert_eq!(check_active_preset(&config, &ids), None);
        config.active_preset = "Mine".into();
        config.user_presets.push(UserPreset {
            name: "mine".into(),
            settings: EqSettings::FLAT,
        });
        assert_eq!(check_active_preset(&config, &ids), None);
        config.active_preset = "gone".into();
        let w = check_active_preset(&config, &ids).unwrap();
        assert!(w.contains("\"gone\""), "{w}");
        assert!(w.contains("default preset"), "{w}");
    }
}
