//! A restrained palette: neutral greys for chrome, one warm accent for the EQ
//! and the selection, and traffic-light colours reserved for levels and state.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(232, 170, 72);
pub const TEXT: Color = Color::Rgb(214, 214, 220);
pub const MUTED: Color = Color::Rgb(128, 128, 140);
pub const DIM: Color = Color::Rgb(84, 84, 94);
pub const BORDER: Color = Color::Rgb(66, 66, 76);
pub const GRID: Color = Color::Rgb(50, 50, 58);
pub const GRID_ZERO: Color = Color::Rgb(82, 82, 94);
pub const SPECTRUM: Color = Color::Rgb(46, 92, 104);
pub const SELECTION_BG: Color = Color::Rgb(44, 40, 32);
pub const METER_BG: Color = Color::Rgb(30, 30, 36);
pub const OK: Color = Color::Rgb(112, 192, 120);
pub const WARN: Color = Color::Rgb(226, 192, 84);
pub const ERR: Color = Color::Rgb(226, 94, 84);

pub fn text() -> Style {
    Style::new().fg(TEXT)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn dim() -> Style {
    Style::new().fg(DIM)
}

pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}

pub fn key() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    Style::new().fg(if focused { ACCENT } else { BORDER })
}

pub fn title(focused: bool) -> Style {
    let style = Style::new().add_modifier(Modifier::BOLD);
    if focused {
        style.fg(ACCENT)
    } else {
        style.fg(MUTED)
    }
}

/// The focused cell: accent block, readable at a glance.
pub fn cursor() -> Style {
    Style::new()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Colour zone for a level in dBFS.
pub fn level_color(db: f32) -> Color {
    if db >= -6.0 {
        ERR
    } else if db >= -18.0 {
        WARN
    } else {
        OK
    }
}
