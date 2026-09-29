use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::{centered, panel, theme};
use crate::app::{App, DeviceChoice, DevicePicker, Modal, PickerSide};

pub fn render(frame: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else {
        return;
    };
    match modal {
        Modal::Help => help(frame),
        Modal::SavePrompt { name, error } => save_prompt(frame, name, error.as_deref()),
        Modal::ConfirmDelete { index } => {
            let name = app.presets.get(*index).map_or("", |p| p.name.as_str());
            confirm_delete(frame, name);
        }
        Modal::DevicePicker(picker) => device_picker(frame, picker),
        Modal::ConfirmAudibleOutput(picker) => {
            device_picker(frame, picker);
            let name = picker
                .outputs
                .get(picker.output_idx)
                .and_then(|c| c.name.as_deref())
                .unwrap_or_default();
            confirm_audible(frame, name);
        }
    }
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("tab / shift-tab", "switch between presets and bands"),
            ("b", "bypass on/off — A/B compare"),
            ("s", "save current settings as a user preset"),
            ("d", "choose input and output devices"),
            ("r", "restart the audio engine"),
            ("?", "this help"),
            ("q  esc  ctrl-c", "quit"),
        ],
    ),
    (
        "Presets",
        &[
            ("↑ ↓  k j", "move"),
            ("enter", "apply preset"),
            ("x  delete", "delete user preset"),
        ],
    ),
    (
        "Bands",
        &[
            ("↑ ↓  k j", "select row"),
            ("← →  h l", "select field"),
            ("=  -", "adjust: 1/12 oct · 0.5 dB · Q ×1.1"),
            ("+  _", "coarse (shifted keys): 1/3 oct · 3 dB · Q ×1.5"),
            ("t  T", "cycle filter type / high-pass slope"),
            ("space", "enable / disable"),
            ("0", "reset gain to 0 dB"),
        ],
    ),
];

fn help(frame: &mut Frame) {
    let mut lines = Vec::new();
    for (section, keys) in HELP {
        lines.push(Line::from(Span::styled(
            *section,
            theme::accent().add_modifier(Modifier::BOLD),
        )));
        for (key, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<17}"), theme::key()),
                Span::styled(*what, theme::text()),
            ]));
        }
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        "Route: pick the virtual device (BlackHole / VB-Cable) as the mic in your call app,",
        theme::muted(),
    )));
    lines.push(Line::from(Span::styled(
        "or set it as the macOS default input. Edits are saved automatically.",
        theme::muted(),
    )));
    let area = centered(frame.area(), 88, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(panel("Keys", true)), area);
}

fn save_prompt(frame: &mut Frame, name: &str, error: Option<&str>) {
    let area = centered(frame.area(), 48, 6);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Name  ", theme::muted()),
            Span::styled(name.to_owned(), theme::text().add_modifier(Modifier::BOLD)),
            Span::styled("▏", theme::accent()),
        ]),
        Line::default(),
    ];
    lines.push(match error {
        Some(e) => Line::from(Span::styled(e.to_owned(), Style::new().fg(theme::ERR))),
        None => Line::from(Span::styled("enter save · esc cancel", theme::dim())),
    });
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(panel("Save preset", true)),
        area,
    );
}

fn confirm_delete(frame: &mut Frame, name: &str) {
    let area = centered(frame.area(), 48, 5);
    let lines = vec![
        Line::from(vec![
            Span::styled("Delete user preset ", theme::text()),
            Span::styled(format!("“{name}”"), theme::accent()),
            Span::styled("?", theme::text()),
        ]),
        Line::from(Span::styled("y delete · n keep", theme::dim())),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(panel("Delete", true)),
        area,
    );
}

fn confirm_audible(frame: &mut Frame, name: &str) {
    let area = centered(frame.area(), 60, 7);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("“{name}”"), theme::accent()),
            Span::styled(" is not a known loopback device.", theme::text()),
        ]),
        Line::from(Span::styled(
            "If it's speakers, your mic plays out loud and may feed back.",
            theme::text(),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("Continue? ", theme::text()),
            Span::styled("y/N", theme::key()),
        ]),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(panel("Play out loud?", true)),
        area,
    );
}

fn device_picker(frame: &mut Frame, picker: &DevicePicker) {
    let area = centered(frame.area(), 96, 18);
    frame.render_widget(Clear, area);
    let outer = panel("Devices — enter applies and restarts audio", true);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(inner);
    device_list(
        frame,
        left,
        "Input (microphone)",
        &picker.inputs,
        picker.input_idx,
        picker.side == PickerSide::Input,
    );
    device_list(
        frame,
        right,
        "Output (use a virtual one)",
        &picker.outputs,
        picker.output_idx,
        picker.side == PickerSide::Output,
    );
}

fn device_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    choices: &[DeviceChoice],
    selected: usize,
    focused: bool,
) {
    let items: Vec<ListItem> = choices
        .iter()
        .map(|c| {
            ListItem::new(Line::from(Span::styled(
                format!(" {}", c.label),
                theme::text(),
            )))
        })
        .collect();
    let highlight = if focused {
        theme::cursor()
    } else {
        Style::new().bg(theme::SELECTION_BG)
    };
    let list = List::new(items)
        .block(panel(title, focused))
        .highlight_style(highlight);
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}
