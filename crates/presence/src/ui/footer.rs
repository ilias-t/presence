use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::theme;
use crate::app::{App, Focus, Modal, NoticeLevel};

/// Always shown at the right edge, whatever else has to go.
const PINNED: &[(&str, &str)] = &[("?", "help"), ("q", "quit")];

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let modal_open = app.modal.is_some();
    let pinned = if modal_open {
        Line::default()
    } else {
        hints(PINNED)
    };
    let [left, right] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(pinned.width() as u16),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(pinned), right);

    // A notice replaces the other hints while it's up rather than overlapping
    // them; cut with an ellipsis if even that is too narrow.
    let line = match &app.notice {
        Some(n) => {
            let color = match n.level {
                NoticeLevel::Info => theme::OK,
                NoticeLevel::Warn => theme::WARN,
            };
            Line::from(Span::styled(
                format!(
                    " {}",
                    ellipsize(&n.text, (left.width as usize).saturating_sub(2))
                ),
                Style::new().fg(color),
            ))
        }
        None => fitting_hints(context_hints(app), left.width as usize),
    };
    frame.render_widget(Paragraph::new(line), left);
}

fn context_hints(app: &App) -> &'static [(&'static str, &'static str)] {
    match &app.modal {
        Some(Modal::Help) => &[("any key", "close")],
        Some(Modal::SavePrompt { .. }) => &[("⏎", "save"), ("esc", "cancel")],
        Some(Modal::ConfirmDelete { .. }) => &[("y", "delete"), ("n", "keep")],
        Some(Modal::ConfirmAudibleOutput(_)) => &[("y", "play out loud"), ("n", "back")],
        Some(Modal::DevicePicker(_)) => &[
            ("↑↓", "choose"),
            ("tab", "input/output"),
            ("⏎", "apply & restart"),
            ("esc", "cancel"),
        ],
        None => match app.focus {
            Focus::Bands => &[
                ("↑↓", "row"),
                ("←→", "field"),
                ("= -", "adjust"),
                ("+ _", "coarse"),
                ("t", "type"),
                ("␣", "on/off"),
                ("b", "bypass"),
                ("s", "save"),
                ("d", "devices"),
                ("tab", "presets"),
            ],
            Focus::Presets => &[
                ("↑↓", "select"),
                ("⏎", "apply"),
                ("s", "save"),
                ("x", "delete"),
                ("b", "bypass"),
                ("d", "devices"),
                ("tab", "bands"),
            ],
        },
    }
}

/// As many hints as fit, in priority (list) order; never a partial one.
fn fitting_hints(list: &[(&str, &str)], width: usize) -> Line<'static> {
    let mut used = 1;
    let mut shown = Vec::new();
    for &(key, what) in list {
        let w = key.width() + what.width() + 3;
        if used + w > width {
            break;
        }
        used += w;
        shown.push((key, what));
    }
    let mut line = hints(&shown);
    line.spans.insert(0, Span::raw(" "));
    line
}

fn hints(list: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, what) in list {
        spans.push(Span::styled(key.to_string(), theme::key()));
        spans.push(Span::styled(format!(" {what}  "), theme::muted()));
    }
    Line::from(spans)
}

fn ellipsize(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_dropped_whole() {
        let list = [("a", "alpha"), ("b", "beta")];
        let text = |w| -> String {
            fitting_hints(&list, w)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert_eq!(text(40).trim_end(), " a alpha  b beta");
        assert_eq!(text(12).trim_end(), " a alpha");
        assert_eq!(text(3).trim_end(), "");
    }

    #[test]
    fn ellipsizes_long_notices() {
        assert_eq!(ellipsize("hello", 10), "hello");
        assert_eq!(ellipsize("hello world", 6), "hello…");
    }
}
