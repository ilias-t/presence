//! Non-interactive subcommands: `devices`, `presets`, `run`.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use presence_engine::{DeviceInfo, Engine, EngineConfig, EngineStatus, Meters, MicPermission};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::config::{self, Config};
use crate::devices::{self, Want};
use crate::failure::{self, Failure, Gate, NO_AUDIO, NO_AUDIO_HINT};
use crate::lock;
use crate::presets;
use crate::runner::PERMISSION_INTERVAL;
use crate::watchdog::{self, Watchdog};

pub fn devices() -> Result<()> {
    let inputs = presence_engine::list_input_devices().context("listing input devices")?;
    let outputs = presence_engine::list_output_devices().context("listing output devices")?;
    let virt = presence_engine::find_virtual_output().context("detecting virtual device")?;
    let virt_name = virt.as_ref().map(|d| d.name.as_str());
    let auto_input = presence_engine::auto_input_device().context("choosing the input")?;
    let auto_name = auto_input.as_ref().map(|d| d.name.as_str());

    let width = inputs
        .iter()
        .chain(&outputs)
        .map(|d| d.name.width())
        .max()
        .unwrap_or(0);
    let print = |d: &DeviceInfo, output: bool| {
        let mut tags = Vec::new();
        if d.is_default {
            tags.push("default");
        }
        if devices::is_virtual(&d.name) {
            tags.push("virtual");
        }
        let auto = if output { virt_name } else { auto_name };
        if auto == Some(d.name.as_str()) {
            tags.push("auto");
        }
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        };
        println!(
            "  {} {}  {:>2} ch  {:>6} Hz{tags}",
            if d.is_default { "*" } else { " " },
            pad(&d.name, width),
            d.channels,
            d.default_sample_rate,
        );
    };

    println!("Inputs:");
    inputs.iter().for_each(|d| print(d, false));
    println!("\nOutputs:");
    outputs.iter().for_each(|d| print(d, true));
    println!();
    match auto_input {
        Some(d) => println!(
            "Automatic input: {}  — the default input, unless that's a virtual device.",
            d.name
        ),
        None => println!(
            "No microphone found (virtual devices are never picked automatically); pass --input NAME."
        ),
    }
    match virt {
        Some(d) => println!(
            "Virtual device: {}  — presence plays into it; select it as the microphone in your call app.",
            d.name
        ),
        None => println!(
            "No virtual device found. Install BlackHole (`brew install blackhole-2ch`) or VB-Cable, \
             or pass --output NAME."
        ),
    }
    Ok(())
}

pub fn presets(config_path: Option<&Path>) -> Result<()> {
    let config = load_config(config_path);
    let builtins = presence_dsp::builtin_presets();
    let app = App::new(&builtins, config);

    let id_width = app
        .presets
        .iter()
        .map(|p| p.id.width())
        .max()
        .unwrap_or(2)
        .max(2);
    let name_width = app
        .presets
        .iter()
        .map(|p| p.name.width())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "  {}  {}  DESCRIPTION",
        pad("ID", id_width),
        pad("NAME", name_width)
    );
    let mut current_section = None;
    for p in &app.presets {
        if current_section != Some(p.section()) {
            println!("\n  {}", p.section());
            current_section = Some(p.section());
        }
        let mark = if app.is_active(p) { "*" } else { " " };
        println!(
            "{mark} {}  {}  {}",
            pad(&p.id, id_width),
            pad(&p.name, name_width),
            p.description
        );
    }
    println!("\nactive: {}", app.active_label());
    Ok(())
}

pub fn run(
    config_path: Option<&Path>,
    input: Option<String>,
    output: Option<String>,
    preset: Option<String>,
    permission: fn() -> MicPermission,
) -> Result<()> {
    let config = load_config(config_path);
    let builtins = presence_dsp::builtin_presets();
    let _lock = lock::acquire(&lock::lock_path(config_path))?;

    // Flags are passed through as given (a typo errors); remembered devices
    // that aren't connected fall back to the defaults.
    let inputs = presence_engine::list_input_devices().ok();
    let outputs = presence_engine::list_output_devices().ok();
    let auto_input = presence_engine::auto_input_device().ok().flatten();
    let plan = devices::plan(
        want(input.as_deref(), config.input_device.as_deref()),
        want(output.as_deref(), config.output_device.as_deref()),
        inputs.as_deref(),
        outputs.as_deref(),
        auto_input.as_ref().map(|d| d.name.as_str()),
    );
    for notice in &plan.notices {
        eprintln!("presence: note: {notice}");
    }
    if config.bypass {
        eprintln!("presence: note: the saved bypass setting is ignored in headless mode");
    }
    let app = App::new(&builtins, config);

    let (settings, label) = match preset {
        Some(key) => match presets::find(&app.presets, &key) {
            Some(p) => (p.settings, p.name.clone()),
            None => {
                let ids: Vec<&str> = app.presets.iter().map(|p| p.id.as_str()).collect();
                bail!("unknown preset \"{key}\"; available: {}", ids.join(", "));
            }
        },
        None => (app.settings, app.active_label()),
    };

    match failure::gate(permission()) {
        Gate::Refuse(f) => return Err(failure_error(&f)),
        Gate::StartAfterPrompt => eprintln!("presence: {}", failure::MIC_PROMPT),
        Gate::Start => {}
    }
    let engine = Engine::start(EngineConfig {
        input_device: plan.input,
        output_device: plan.output,
        settings,
        bypass: false,
    })
    .map_err(|e| failure_error(&Failure::from_engine(&e)))?;
    let p = permission();
    if failure::revoked(p) {
        drop(engine);
        return Err(failure_error(&Failure::mic_permission(p)));
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .context("installing Ctrl-C handler")?;
    }

    let stats = engine.stats();
    println!(
        "presence: {} → {}  ·  {} Hz  ·  preset {label}  ·  Ctrl-C to stop",
        stats.input_device, stats.output_device, stats.input_sample_rate
    );
    if let Some(warning) = devices::virtual_input_warning(&stats.input_device, &stats.output_device)
    {
        eprintln!("presence: warning: {warning}");
    }
    if devices::is_virtual(&stats.output_device) {
        println!(
            "In your call app, choose \"{}\" as the microphone.",
            stats.output_device
        );
    } else {
        eprintln!(
            "presence: warning: \"{}\" is not a known loopback device (BlackHole, VB-Cable), so \
             call apps won't see it as a mic. If it's speakers, your mic plays out loud and can \
             feed back — use headphones.",
            stats.output_device
        );
    }

    let tty = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout();
    let mut next_print = Instant::now();
    let mut next_permission_check = Instant::now() + PERMISSION_INTERVAL;
    let mut watchdog = Watchdog::new(Instant::now(), engine.callback_count());
    while !stop.load(Ordering::SeqCst) {
        if let EngineStatus::Failed(msg) = engine.status() {
            if tty {
                println!();
            }
            bail!("audio engine stopped: {msg}");
        }
        let now = Instant::now();
        if now >= next_permission_check {
            next_permission_check = now + PERMISSION_INTERVAL;
            let p = permission();
            if failure::revoked(p) {
                drop(engine);
                if tty {
                    println!();
                }
                return Err(failure_error(&Failure::mic_permission(p)));
            }
        }
        let note = match watchdog.update(now, engine.callback_count()) {
            watchdog::Change::Stalled => Some(format!("warning: {NO_AUDIO} — {NO_AUDIO_HINT}")),
            watchdog::Change::Recovered => Some("audio from the mic is flowing now".to_owned()),
            watchdog::Change::None => None,
        };
        if let Some(note) = note {
            if tty {
                // Clear the in-place status line so the note doesn't land on it.
                write!(stdout, "\r\x1b[2K")?;
                stdout.flush()?;
            }
            eprintln!("presence: {note}");
        }
        if now >= next_print {
            next_print = now + Duration::from_millis(500);
            if tty {
                // Overwrite in place on a terminal, never wider than it (a
                // wrapped line can't be overwritten); plain lines when piped.
                let width = crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
                let line = status_line(engine.meters(), &engine.stats(), Some(width));
                write!(stdout, "\r\x1b[2K{line}")?;
                stdout.flush()?;
            } else {
                let line = status_line(engine.meters(), &engine.stats(), None);
                writeln!(stdout, "{line}")?;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(engine);
    if tty {
        println!();
    }
    println!("stopped.");
    Ok(())
}

fn want<'a>(flag: Option<&'a str>, saved: Option<&'a str>) -> Want<'a> {
    Want {
        name: flag.or(saved),
        explicit: flag.is_some(),
    }
}

fn failure_error(f: &Failure) -> anyhow::Error {
    match f.cli_hint() {
        Some(hint) => anyhow!("{}\n  hint: {hint}", f.message),
        None => anyhow!("{}", f.message),
    }
}

/// Left-aligns `s` in `width` terminal columns (`format!` pads by chars, which
/// misaligns double-width names).
fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.width())))
}

/// Read-only: never moves or rewrites the file, even an invalid one.
fn load_config(path: Option<&Path>) -> Config {
    let Some(path) = path else {
        return Config::default();
    };
    let loaded = config::load_from(path);
    for w in &loaded.warnings {
        eprintln!("presence: warning: {w}");
    }
    let ids: Vec<&str> = presence_dsp::builtin_presets()
        .iter()
        .map(|p| p.id)
        .collect();
    if let Some(w) = config::check_active_preset(&loaded.config, &ids) {
        eprintln!("presence: warning: {w}");
    }
    loaded.config
}

/// One status line. With a `width` (a terminal), it is kept to `width - 1`
/// columns: bars shrink first, then the latency/xrun details go, and as a last
/// resort the line is cut.
fn status_line(m: Meters, stats: &presence_engine::EngineStats, width: Option<usize>) -> String {
    let build = |cells: usize, tail: &str| {
        format!(
            "IN {} pk {}  OUT {} pk {}{tail}",
            bar(m.input_rms_db, cells),
            peak(m.input_peak_db),
            bar(m.output_rms_db, cells),
            peak(m.output_peak_db),
        )
    };
    let full_tail = format!(
        "  {:.1} ms  xrun {}/{}",
        stats.latency_ms, stats.underruns, stats.overruns
    );
    let Some(width) = width else {
        return build(BAR_CELLS, &full_tail);
    };
    // Writing into the last column makes some terminals wrap.
    let max = width.saturating_sub(1);
    let short_tail = format!("  {:.0} ms", stats.latency_ms);
    for tail in [full_tail.as_str(), short_tail.as_str(), ""] {
        let fixed = build(0, tail).chars().count();
        let cells = (max.saturating_sub(fixed) / 2).min(BAR_CELLS);
        if cells >= MIN_BAR_CELLS {
            return build(cells, tail);
        }
    }
    let compact = format!(
        "IN {}  OUT {}",
        peak(m.input_peak_db).trim_start(),
        peak(m.output_peak_db).trim_start()
    );
    compact.chars().take(max).collect()
}

const BAR_CELLS: usize = 12;
const MIN_BAR_CELLS: usize = 4;

/// A tiny RMS bar over −60..0 dBFS.
fn bar(db: f32, cells: usize) -> String {
    let frac = if db.is_finite() {
        ((db + 60.0) / 60.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (frac * cells as f32).round() as usize;
    format!("▕{}{}▏", "█".repeat(filled), "·".repeat(cells - filled))
}

fn peak(db: f32) -> String {
    if db.is_finite() && db > -99.0 {
        format!("{db:>6.1} dB")
    } else {
        "  -inf dB".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_scales_and_handles_silence() {
        assert_eq!(bar(f32::NEG_INFINITY, 12), format!("▕{}▏", "·".repeat(12)));
        assert_eq!(bar(0.0, 12), format!("▕{}▏", "█".repeat(12)));
        assert_eq!(
            bar(-30.0, 12),
            format!("▕{}{}▏", "█".repeat(6), "·".repeat(6))
        );
        assert_eq!(peak(f32::NEG_INFINITY), "  -inf dB");
        assert_eq!(peak(-3.04), "  -3.0 dB");
    }

    fn stats() -> presence_engine::EngineStats {
        presence_engine::EngineStats {
            input_device: "MacBook Pro Microphone".into(),
            output_device: "BlackHole 2ch".into(),
            input_sample_rate: 48_000,
            output_sample_rate: 48_000,
            latency_ms: 12.4,
            underruns: 12,
            overruns: 345,
        }
    }

    fn meters() -> Meters {
        Meters {
            input_peak_db: -51.1,
            input_rms_db: -60.0,
            output_peak_db: -52.2,
            output_rms_db: -61.0,
        }
    }

    #[test]
    fn status_line_fits_the_terminal() {
        let piped = status_line(meters(), &stats(), None);
        assert!(piped.contains(&"·".repeat(12)), "{piped}");
        assert!(piped.contains("xrun 12/345"), "{piped}");
        for width in [200, 120, 80, 60, 40, 20, 1, 0] {
            let line = status_line(meters(), &stats(), Some(width));
            assert!(
                line.chars().count() < width.max(1),
                "width {width}: {} chars: {line}",
                line.chars().count()
            );
        }
        let at_80 = status_line(meters(), &stats(), Some(80));
        assert!(at_80.contains("OUT"), "{at_80}");
        assert!(at_80.contains("ms"), "{at_80}");
        // Readouts never run together.
        assert!(
            at_80.contains("-51.1 dB") && at_80.contains("-52.2 dB"),
            "{at_80}"
        );
        assert!(!at_80.contains("-51.1-52.2"));
    }

    #[test]
    fn pad_counts_terminal_columns() {
        assert_eq!(pad("ab", 4), "ab  ");
        // Two double-width chars occupy four columns.
        assert_eq!(pad("麦克", 6), "麦克  ");
        assert_eq!(pad("toolong", 3), "toolong");
    }

    #[test]
    fn device_flags_are_explicit_and_win() {
        let w = want(Some("usb"), Some("MacBook Pro Microphone"));
        assert_eq!(w.name, Some("usb"));
        assert!(w.explicit);
        let w = want(None, Some("MacBook Pro Microphone"));
        assert_eq!(w.name, Some("MacBook Pro Microphone"));
        assert!(!w.explicit);
    }
}
