//! Rendering. Everything here is a pure function of [`App`], so it can be
//! exercised with ratatui's `TestBackend`.

mod bands;
mod curve;
mod footer;
mod header;
mod meters;
mod modal;
mod presets;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, BorderType, Borders};

use crate::app::{App, Focus};

const PRESETS_WIDTH: u16 = 28;
/// Fits "Warm / Broadcast" with its marker.
const PRESETS_WIDTH_NARROW: u16 = 23;
const METERS_WIDTH: u16 = 18;
const METERS_WIDTH_NARROW: u16 = meters::MIN_INNER_WIDTH + 2;
/// Below this the curve gives up its height to keep every band row visible.
const MIN_CURVE_HEIGHT: u16 = 8;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let [header_area, body, footer_area] = Layout::vertical([
        Constraint::Length(header::height(app, area.width)),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    // Down to 80 columns everything keeps a usable width: the side panels
    // shrink first, then the band table tightens its columns.
    let roomy = body.width >= PRESETS_WIDTH + bands::WIDE_WIDTH + METERS_WIDTH;
    let (presets_width, meters_width) = if roomy {
        (PRESETS_WIDTH, METERS_WIDTH)
    } else {
        (PRESETS_WIDTH_NARROW, METERS_WIDTH_NARROW)
    };
    let [presets_area, main, meters_area] = Layout::horizontal([
        Constraint::Length(presets_width),
        Constraint::Min(0),
        Constraint::Length(meters_width),
    ])
    .areas(body);

    let density = bands::Density {
        narrow: main.width < bands::WIDE_WIDTH,
        short: main.height < bands::height(false) + MIN_CURVE_HEIGHT,
    };
    let [curve_area, bands_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(bands::height(density.short)),
    ])
    .areas(main);

    header::render(frame, header_area, app);
    presets::render(frame, presets_area, app);
    curve::render(frame, curve_area, app);
    bands::render(frame, bands_area, app, density);
    meters::render(frame, meters_area, app);
    footer::render(frame, footer_area, app);
    modal::render(frame, app);
}

fn panel(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(format!(" {title} "))
        .title_style(theme::title(focused))
}

fn focused(app: &App, focus: Focus) -> bool {
    app.modal.is_none() && app.focus == focus
}

pub fn format_hz(hz: f32) -> String {
    if hz >= 10_000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else if hz >= 1_000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else if hz >= 100.0 {
        format!("{hz:.0} Hz")
    } else {
        format!("{hz:.1} Hz")
    }
}

pub fn format_db(db: f32) -> String {
    // Avoid "-0.0" after rounding.
    let db = if db.abs() < 0.05 { 0.0 } else { db };
    format!("{db:+.1} dB")
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;
    use crate::app::{EngineState, Focus, Modal, NoticeLevel};
    use crate::failure::{Failure, FailureKind};
    use presence_engine::{EngineStats, Meters};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn running_app() -> App {
        let mut app = test_app();
        app.engine.state = EngineState::Running;
        app.engine.stats = Some(EngineStats {
            input_device: "MacBook Pro Microphone".into(),
            output_device: "BlackHole 2ch".into(),
            input_sample_rate: 48_000,
            output_sample_rate: 48_000,
            latency_ms: 12.4,
            underruns: 0,
            overruns: 3,
        });
        app.engine.meters = Meters {
            input_peak_db: -9.0,
            input_rms_db: -22.0,
            output_peak_db: -4.0,
            output_rms_db: -16.0,
        };
        let freqs = crate::spectrum::display_freqs();
        // Roughly voice-shaped: strongest around 300 Hz, rolling off both ways.
        app.spectrum = freqs
            .iter()
            .map(|f| -38.0 - 14.0 * (f / 300.0).log10().powi(2) * 3.0)
            .collect();
        app
    }

    #[test]
    fn formats_values() {
        assert_eq!(format_hz(90.0), "90.0 Hz");
        assert_eq!(format_hz(250.0), "250 Hz");
        assert_eq!(format_hz(3_500.0), "3.50 kHz");
        assert_eq!(format_hz(12_000.0), "12.0 kHz");
        assert_eq!(format_db(2.5), "+2.5 dB");
        assert_eq!(format_db(-0.01), "+0.0 dB");
        assert_eq!(format_db(-3.0), "-3.0 dB");
    }

    #[test]
    fn renders_main_screen() {
        let app = running_app();
        let screen = draw(&app, 120, 40);
        for needle in [
            "PRESENCE",
            "● running",
            "MacBook Pro Microphone → BlackHole 2ch",
            "48 kHz",
            "12.4 ms",
            "Presets",
            "Clarity",
            "Crisp, present voice",
            "HP",
            "Freq",
            "Gain",
            "250 Hz",
            "+2.5 dB",
            "1k",
            "10k",
            "IN",
            "OUT",
            "? help",
            "call app",
        ] {
            assert!(screen.contains(needle), "missing {needle:?} in:\n{screen}");
        }
        // The EQ curve and spectrum are braille.
        assert!(
            screen
                .chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "no curve drawn:\n{screen}"
        );
    }

    fn failed(kind: FailureKind, message: &str) -> App {
        let mut app = test_app();
        app.engine.state = EngineState::Failed(Failure::new(kind, message));
        app
    }

    /// The screen with line breaks removed, so wrapped text can be searched.
    fn joined(screen: &str) -> String {
        screen.lines().map(str::trim).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn renders_engine_error_with_hint() {
        let mut app = failed(
            FailureKind::NoVirtualDevice,
            "no virtual audio device found",
        );
        app.bypass = true;
        let screen = draw(&app, 120, 40);
        assert!(screen.contains("✖"), "{screen}");
        assert!(screen.contains("no virtual audio device found"));
        assert!(joined(&screen).contains("press r"), "{screen}");
        assert!(screen.contains("BlackHole"));
    }

    #[test]
    fn error_hints_fit_the_failure_and_are_never_cut_off() {
        for (width, height) in [(80, 24), (110, 30), (120, 40)] {
            for kind in [
                FailureKind::NoVirtualDevice,
                FailureKind::DeviceNotFound,
                FailureKind::SameDevice,
                FailureKind::MicPermission,
                FailureKind::Other,
            ] {
                let failure = Failure::new(kind, "audio device not found: USB Mic");
                let app = failed(kind, &failure.message);
                let screen = joined(&draw(&app, width, height));
                let hint = failure.tui_hint();
                // The last words of the hint are on screen: nothing truncated.
                let tail: String = hint
                    .split_whitespace()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    screen.contains(&tail),
                    "{kind:?} at {width}x{height}: missing {tail:?} in:\n{screen}"
                );
                assert_eq!(
                    screen.contains("BlackHole"),
                    kind == FailureKind::NoVirtualDevice,
                    "{kind:?}: install advice only for a missing virtual device"
                );
            }
        }
    }

    #[test]
    fn preset_panel_has_voice_fun_and_user_sections() {
        let builtins = presence_dsp::builtin_presets();
        let app = App::new(&builtins, crate::config::Config::default());
        let screen = draw(&app, 120, 40);
        // Each needle must appear after the previous one.
        let mut from = 0;
        for needle in [" VOICE", "Clarity", " FUN", "Old telephone", " USER"] {
            let found = screen[from..]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} out of order in:\n{screen}"));
            from += found + needle.len();
        }
    }

    #[test]
    fn renders_starting_state_before_engine_is_up() {
        let screen = draw(&test_app(), 120, 40);
        assert!(screen.contains("◌ starting"), "{screen}");
        assert!(screen.contains("Starting audio"));
    }

    #[test]
    fn renders_mic_prompt_and_denial() {
        let mut app = test_app();
        app.engine.mic_prompt = true;
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("click Allow"), "{screen}");

        let app = failed(FailureKind::MicPermission, "microphone access denied");
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("✖ error"), "{screen}");
        assert!(!screen.contains("● running"));
        let flat = joined(&screen);
        assert!(flat.contains("Privacy & Security"), "{screen}");
        assert!(flat.contains("enable your terminal app"), "{screen}");
    }

    #[test]
    fn renders_no_audio_instead_of_running() {
        let mut app = running_app();
        app.bypass = true;
        app.engine.no_audio = true;
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("⚠ no audio"), "{screen}");
        assert!(!screen.contains("● running"), "{screen}");
        assert!(!screen.contains("bypassed"), "{screen}");
        assert!(
            !screen.contains("12.4 ms"),
            "stats aren't measurements: {screen}"
        );
        let flat = joined(&screen);
        assert!(flat.contains("no audio from the mic"), "{screen}");
        assert!(flat.contains("Privacy & Security → Microphone"), "{screen}");
        assert!(flat.contains("Terminal.app"), "{screen}");

        app.engine.no_audio = false;
        app.bypass = false;
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("● running"), "{screen}");
        assert!(!joined(&screen).contains("no audio"), "{screen}");
    }

    #[test]
    fn audible_output_shows_monitoring_warning_not_routing_hint() {
        let mut app = running_app();
        app.engine.stats.as_mut().unwrap().output_device = "MacBook Pro Speakers".into();
        let screen = draw(&app, 120, 40);
        assert!(screen.contains("Monitoring"), "{screen}");
        assert!(screen.contains("MacBook Pro Speakers"));
        assert!(!screen.contains("call app"), "{screen}");
    }

    fn assert_complete_screen(screen: &str) {
        for needle in [
            "PRESENCE", "Clarity", "HP", "Freq", "8", "In", "Out", "Lim", "Limiter", "250 Hz",
            "-2.0 dB", "IN", "OUT", "Levels", "Response", "? help", "q quit",
        ] {
            assert!(screen.contains(needle), "missing {needle:?} in:\n{screen}");
        }
        // Band 8's row, and the bottom gain rows, are all on screen.
        let rows: Vec<&str> = screen.lines().collect();
        for label in ["│  8 ", "│In ", "│Out ", "│Lim "] {
            let short = label.trim_start_matches('│').trim_end();
            assert!(
                rows.iter().any(|l| l.contains(&format!("│{short:>3} "))
                    || l.contains(&format!("│{short:>4} "))),
                "no {short:?} row in:\n{screen}"
            );
        }
    }

    #[test]
    fn usable_at_80x24() {
        let app = running_app();
        let screen = draw(&app, 80, 24);
        assert_complete_screen(&screen);
        // The meter readouts are separate numbers.
        assert!(screen.contains("-9"), "{screen}");
        assert!(screen.contains("-4"), "{screen}");
        assert!(!screen.contains("-9-4"), "{screen}");
        // Stats moved out of the way rather than truncating the preset name.
        assert!(screen.contains("preset Clarity"), "{screen}");
        assert!(screen.contains("12.4 ms"), "{screen}");
    }

    #[test]
    fn long_preset_label_is_not_truncated_at_80() {
        let mut app = running_app();
        app.active = crate::app::Active::Custom {
            base: Some("clarity".into()),
        };
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("preset custom (from Clarity)"), "{screen}");
        assert_complete_screen(&screen);
    }

    #[test]
    fn usable_at_120x40() {
        let screen = draw(&running_app(), 120, 40);
        assert_complete_screen(&screen);
        assert!(
            screen.contains("-9.0") && screen.contains("-4.0"),
            "{screen}"
        );
        assert!(!screen.contains("-9.0-4.0"));
    }

    #[test]
    fn notice_replaces_hints_but_keeps_help_and_quit() {
        let mut app = running_app();
        app.notify("Saved preset \"Podcast\"", NoticeLevel::Info);
        let screen = draw(&app, 80, 24);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("Saved preset \"Podcast\""), "{footer}");
        assert!(
            footer.contains("? help") && footer.contains("q quit"),
            "{footer}"
        );
        assert!(
            !footer.contains("adjust"),
            "notice shares the line with hints: {footer}"
        );

        app.notice = None;
        app.focus = Focus::Presets;
        let footer = draw(&app, 80, 24).lines().last().unwrap().to_owned();
        assert!(footer.ends_with("q quit"), "{footer}");
    }

    #[test]
    fn device_picker_marks_virtual_and_missing_devices() {
        use crate::devices::tests::dev;
        let mut app = running_app();
        app.input_device = Some("USB Mic".into());
        app.open_device_picker(
            &[dev("MacBook Pro Microphone", false), dev("VB-Cable", true)],
            &[
                dev("MacBook Pro Speakers", true),
                dev("BlackHole 2ch", false),
            ],
            Some("BlackHole 2ch"),
            Some("MacBook Pro Microphone"),
        );
        let screen = draw(&app, 100, 30);
        assert!(screen.contains("USB Mic  (not connected)"), "{screen}");
        assert!(screen.contains("BlackHole 2ch  (virtual)"), "{screen}");
        assert!(screen.contains("Auto — MacBook Pro Microphone"), "{screen}");
        assert!(screen.contains("VB-Cable  (default, virtual)"), "{screen}");
        assert!(!screen.contains("Speakers  (virtual)"));
    }

    #[test]
    fn renders_modals() {
        let mut app = running_app();
        app.modal = Some(Modal::Help);
        assert!(draw(&app, 120, 40).contains("Keys"));
        app.modal = Some(Modal::SavePrompt {
            name: "Podcast".into(),
            error: None,
        });
        assert!(draw(&app, 120, 40).contains("Podcast"));
    }

    #[test]
    fn survives_tiny_terminals() {
        let app = running_app();
        for (w, h) in [(20, 5), (60, 20), (1, 1)] {
            draw(&app, w, h);
        }
    }

    /// Prints a frame for eyeballing: `cargo test -p presence print_frame -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_frame() {
        let mut failing = failed(FailureKind::MicPermission, "microphone access denied");
        failing.notify(
            "mic \"USB Mic\" not connected; using \"MacBook Pro Microphone\"",
            NoticeLevel::Warn,
        );
        for (app, w, h) in [
            (running_app(), 120, 40),
            (running_app(), 80, 24),
            (failing, 80, 24),
        ] {
            println!("{}\n", draw(&app, w, h));
        }
    }
}
