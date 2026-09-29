use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::theme;
use crate::app::{App, EngineState};
use crate::failure::{MIC_PROMPT, NO_AUDIO, NO_AUDIO_HINT};

/// Title and route lines plus the bottom rule; the hint below them wraps onto
/// as many lines as it needs, up to [`MAX_HINT_LINES`].
const FIXED_HEIGHT: u16 = 3;
const MAX_HINT_LINES: u16 = 3;

/// Header height for this state and width.
pub fn height(app: &App, width: u16) -> u16 {
    let text: String = hint_line(app, width)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    FIXED_HEIGHT + wrapped_lines(&text, width as usize).clamp(1, MAX_HINT_LINES)
}

/// Lines a greedy word wrap needs for `text` at `width` columns.
fn wrapped_lines(text: &str, width: usize) -> u16 {
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return 1;
    }
    let (mut lines, mut used) = (1u16, 0usize);
    for word in text.split_whitespace() {
        let w = word.width();
        let needed = if used == 0 { w } else { used + 1 + w };
        if needed <= width {
            used = needed;
        } else {
            lines = lines.saturating_add(1);
            used = w.min(width);
            // A word longer than the line is broken across lines.
            lines = lines.saturating_add((w.saturating_sub(1) / width) as u16);
        }
    }
    lines
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(theme::border(false));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [top, route, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    let title = title_line(app);
    let route_line = route_line(app);
    // Stats go right of the title if they fit, else right of the route line,
    // in the long form first — never at the expense of the preset name or the
    // devices.
    let placed = [stats_line(app, false), stats_line(app, true)]
        .into_iter()
        .filter(|s| s.width() > 0)
        .find_map(|stats| {
            let w = stats.width() + 2;
            if title.width() + w <= top.width as usize {
                Some((top, stats))
            } else if route_line.width() + w <= route.width as usize {
                Some((route, stats))
            } else {
                None
            }
        });
    frame.render_widget(Paragraph::new(title), top);
    frame.render_widget(Paragraph::new(route_line), route);
    if let Some((row, stats)) = placed {
        frame.render_widget(Paragraph::new(stats).right_aligned(), row);
    }
    frame.render_widget(
        Paragraph::new(hint_line(app, area.width)).wrap(Wrap { trim: true }),
        hint,
    );
}

fn title_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " PRESENCE ",
            Style::new().fg(Color::Black).bg(theme::ACCENT).bold(),
        ),
        Span::raw("  "),
        state_span(app),
        Span::styled("   preset ", theme::dim()),
        Span::styled(
            app.active_label(),
            theme::text().add_modifier(Modifier::BOLD),
        ),
    ])
}

fn state_span(app: &App) -> Span<'static> {
    match (&app.engine.state, app.bypass) {
        (EngineState::Running, _) if app.engine.no_audio => {
            Span::styled("⚠ no audio", Style::new().fg(theme::WARN).bold())
        }
        (EngineState::Running, true) => {
            Span::styled("⏸ bypassed", Style::new().fg(theme::WARN).bold())
        }
        (EngineState::Running, false) => {
            Span::styled("● running", Style::new().fg(theme::OK).bold())
        }
        (EngineState::Failed(_), _) => Span::styled("✖ error", Style::new().fg(theme::ERR).bold()),
        (EngineState::Starting, _) => Span::styled("◌ starting", theme::muted()),
    }
}

fn stats_line(app: &App, short: bool) -> Line<'static> {
    let Some(stats) = app
        .engine
        .stats
        .as_ref()
        // Without callbacks the figures are all zero, not measurements.
        .filter(|_| app.engine.state == EngineState::Running && !app.engine.no_audio)
    else {
        return Line::default();
    };
    let khz = |hz: u32| {
        let k = hz as f32 / 1000.0;
        if k.fract() == 0.0 {
            format!("{k:.0}")
        } else {
            format!("{k:.1}")
        }
    };
    let rate = if stats.input_sample_rate == stats.output_sample_rate {
        format!("{} kHz", khz(stats.input_sample_rate))
    } else {
        format!(
            "{}→{} kHz",
            khz(stats.input_sample_rate),
            khz(stats.output_sample_rate)
        )
    };
    let xrun_style = if stats.underruns + stats.overruns > 0 {
        Style::new().fg(theme::WARN)
    } else {
        theme::muted()
    };
    let sep = if short { " · " } else { "  ·  " };
    Line::from(vec![
        Span::styled(rate, theme::text()),
        Span::styled(sep, theme::dim()),
        Span::styled(format!("{:.1} ms", stats.latency_ms), theme::text()),
        Span::styled(sep, theme::dim()),
        Span::styled(
            format!("xrun {}↓ {}↑ ", stats.underruns, stats.overruns),
            xrun_style,
        ),
    ])
}

fn route_line(app: &App) -> Line<'static> {
    let (input, output) = match (&app.engine.state, &app.engine.stats) {
        (EngineState::Running, Some(s)) => (s.input_device.clone(), s.output_device.clone()),
        _ => (
            app.input_device
                .clone()
                .unwrap_or_else(|| "auto input".into()),
            app.output_device
                .clone()
                .unwrap_or_else(|| "auto (virtual device)".into()),
        ),
    };
    Line::from(vec![
        Span::styled(" mic ", theme::dim()),
        Span::styled(format!("{input} → {output}"), theme::text()),
    ])
}

fn warn_mark() -> Span<'static> {
    Span::styled(" ! ", Style::new().fg(theme::WARN).bold())
}

fn hint_line(app: &App, width: u16) -> Line<'static> {
    match &app.engine.state {
        EngineState::Failed(failure) => Line::from(vec![
            Span::styled(
                format!(" ✖ {}", failure.message),
                Style::new().fg(theme::ERR),
            ),
            Span::styled(format!(" — {}", failure.tui_hint()), theme::muted()),
        ]),
        EngineState::Starting if app.engine.mic_prompt => {
            Line::from(vec![warn_mark(), Span::styled(MIC_PROMPT, theme::text())])
        }
        EngineState::Starting => Line::from(Span::styled(" Starting audio…", theme::muted())),
        EngineState::Running if app.engine.mic_prompt => Line::from(vec![
            warn_mark(),
            Span::styled(
                "waiting for microphone access: click Allow in the macOS prompt",
                theme::text(),
            ),
        ]),
        EngineState::Running if app.engine.no_audio => Line::from(vec![
            warn_mark(),
            Span::styled(NO_AUDIO, Style::new().fg(theme::WARN)),
            Span::styled(format!(" — {NO_AUDIO_HINT}"), theme::muted()),
        ]),
        EngineState::Running => {
            if let Some(output) = app.output_is_audible() {
                Line::from(vec![
                    warn_mark(),
                    Span::styled(format!("Monitoring through “{output}”"), theme::text()),
                    Span::styled(
                        " — not a known loopback device; if it's speakers, use headphones to avoid feedback (d to change)",
                        theme::muted(),
                    ),
                ])
            } else if app.show_onboarding {
                let device = app.engine.stats.as_ref().map_or_else(
                    || "the virtual device".to_owned(),
                    |s| format!("“{}”", s.output_device),
                );
                let mut line = Line::from(vec![
                    Span::styled(" → ", theme::accent()),
                    Span::styled(
                        format!("In your call app, choose {device} as the microphone"),
                        theme::text(),
                    ),
                    Span::styled(" — or set it as the macOS default input.", theme::muted()),
                ]);
                // The alternative is optional; don't spend a line on it.
                if line.width() > width as usize {
                    line.spans.pop();
                }
                line
            } else {
                Line::default()
            }
        }
    }
}
