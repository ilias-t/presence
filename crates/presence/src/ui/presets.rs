use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::{focused, panel, theme};
use crate::app::{Active, App, Focus};

const DESCRIPTION_HEIGHT: u16 = 7;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let is_focused = focused(app, Focus::Presets);
    let block = panel("Presets", is_focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [list_area, info_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(DESCRIPTION_HEIGHT)]).areas(inner);

    let mut lines = Vec::new();
    let mut cursor_line = 0;
    let mut current_section = None;
    for (i, p) in app.presets.iter().enumerate() {
        if current_section != Some(p.section()) {
            if current_section.is_some() {
                lines.push(Line::default());
            }
            lines.push(section(p.section()));
            current_section = Some(p.section());
        }
        if i == app.preset_cursor {
            cursor_line = lines.len();
        }
        let active = app.is_active(p);
        let marker = if active {
            Span::styled(" ● ", theme::accent())
        } else if matches!(&app.active, Active::Custom { base: Some(b) } if *b == p.id) {
            Span::styled(" ◦ ", theme::muted())
        } else {
            Span::raw("   ")
        };
        let name_style = if active {
            theme::text().add_modifier(Modifier::BOLD)
        } else {
            theme::text()
        };
        let mut line = Line::from(vec![marker, Span::styled(p.name.clone(), name_style)]);
        if i == app.preset_cursor {
            line = if is_focused {
                line.style(Style::new().bg(theme::SELECTION_BG))
            } else {
                line.style(Style::new().add_modifier(Modifier::UNDERLINED))
            };
        }
        lines.push(line);
    }
    if !app.presets.iter().any(|p| p.user) {
        lines.push(Line::default());
        lines.push(section("USER"));
        lines.push(Line::from(Span::styled("   press s to save", theme::dim())));
    }

    // Keep the cursor visible on short terminals.
    let height = list_area.height as usize;
    let scroll = cursor_line.saturating_sub(height.saturating_sub(1));
    let list = Paragraph::new(lines).scroll((scroll as u16, 0));
    frame.render_widget(list, list_area);

    let mut info = vec![Line::from(vec![
        Span::styled("active ", theme::dim()),
        Span::styled(app.active_label(), theme::accent()),
    ])];
    // Name the highlighted preset so its description can't be mistaken for
    // the active sound's.
    if let Some(p) = app.presets.get(app.preset_cursor) {
        let edited_base = matches!(&app.active, Active::Custom { base: Some(b) } if *b == p.id);
        let status = if edited_base {
            " · edited, ⏎ reverts"
        } else {
            " · ⏎ applies"
        };
        if !app.is_active(p) {
            info.push(Line::from(vec![
                Span::styled(p.name.clone(), theme::text().add_modifier(Modifier::BOLD)),
                Span::styled(status, theme::dim()),
            ]));
        }
        info.push(Line::from(Span::styled(
            p.description.clone(),
            theme::muted(),
        )));
    }
    let info_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::border(false));
    frame.render_widget(
        Paragraph::new(info)
            .wrap(Wrap { trim: true })
            .block(info_block),
        info_area,
    );
}

fn section(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {label}"),
        theme::dim().add_modifier(Modifier::BOLD),
    ))
}
