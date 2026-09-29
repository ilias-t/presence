use presence_dsp::{BandKind, HighPassSlope};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Cell, Row as TableRow, Table};

use super::{focused, format_db, format_hz, panel, theme};
use crate::app::{App, COLUMNS, Focus, ROWS, Row, kind_uses_gain};

const NA: &str = "·";

/// Outer width of the table with roomy columns; below it, [`Density::narrow`].
pub const WIDE_WIDTH: u16 = 56;

/// How much room the table gets.
#[derive(Clone, Copy, Debug)]
pub struct Density {
    /// Tight columns and short labels.
    pub narrow: bool,
    /// No blank row before the gain rows.
    pub short: bool,
}

/// Outer height: borders + column header + one line per row (+ spacer).
pub fn height(short: bool) -> u16 {
    let spacer = if short { 0 } else { 1 };
    2 + 1 + ROWS.len() as u16 + spacer
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    let is_focused = focused(app, Focus::Bands);
    let block = panel("Bands", is_focused);

    let header = TableRow::new(
        ["", "On", "Type", "Freq", "Gain", "Q"]
            .map(|h| Cell::from(h).style(theme::dim().add_modifier(Modifier::BOLD))),
    );

    let mut rows = Vec::with_capacity(ROWS.len() + 1);
    for (r, row) in ROWS.iter().enumerate() {
        if *row == Row::InputGain && !density.short {
            rows.push(TableRow::new([Cell::from("")]).height(1));
        }
        let (label, cells, enabled) = describe(app, *row, density.narrow);
        let base = if enabled { theme::text() } else { theme::dim() };
        let selected_row = r == app.row;
        let mut row_cells =
            vec![
                Cell::from(Text::from(label).right_aligned()).style(if selected_row {
                    theme::accent().add_modifier(Modifier::BOLD)
                } else {
                    theme::muted()
                }),
            ];
        for (c, text) in cells.into_iter().enumerate() {
            let style = if selected_row && c == app.col && is_focused {
                theme::cursor()
            } else if text == NA {
                theme::dim()
            } else {
                base
            };
            let text = if COLUMNS[c] == crate::app::Column::On {
                Text::from(text).centered()
            } else {
                Text::from(text)
            };
            row_cells.push(Cell::from(text).style(style));
        }
        let mut table_row = TableRow::new(row_cells);
        if selected_row {
            table_row = table_row.style(Style::new().bg(theme::SELECTION_BG));
        }
        rows.push(table_row);
    }

    // Narrow: label 3, on 2, "High shelf" 10, "12.0 kHz" 8, "+12.0 dB" 8,
    // "18.00" 5, one column apart: 41 + borders = 43, which fits between the
    // narrow side panels at 80 columns.
    let (widths, spacing) = if density.narrow {
        ([3, 2, 10, 8, 8, 5], 1)
    } else {
        ([4, 4, 11, 10, 9, 6], 2)
    };
    let table = Table::new(rows, widths.map(Constraint::Length))
        .header(header)
        .column_spacing(spacing)
        .block(block);
    frame.render_widget(table, area);
}

fn on_off(on: bool) -> String {
    if on { "●".into() } else { "○".into() }
}

fn kind_name(kind: BandKind) -> &'static str {
    match kind {
        BandKind::Peak => "Peak",
        BandKind::LowShelf => "Low shelf",
        BandKind::HighShelf => "High shelf",
        BandKind::LowPass => "Low pass",
        BandKind::HighPass => "High pass",
        BandKind::Notch => "Notch",
    }
}

/// Row label, the five column texts, and whether the row is active.
fn describe(app: &App, row: Row, narrow: bool) -> (String, [String; 5], bool) {
    let s = &app.settings;
    let na = || NA.to_owned();
    match row {
        Row::HighPass => {
            let hp = s.high_pass;
            let slope = match hp.slope {
                HighPassSlope::Db12 => "12 dB/oct",
                HighPassSlope::Db24 => "24 dB/oct",
            };
            (
                "HP".into(),
                [
                    on_off(hp.enabled),
                    slope.into(),
                    format_hz(hp.freq_hz),
                    na(),
                    na(),
                ],
                hp.enabled,
            )
        }
        Row::Band(i) => {
            let b = s.bands[i];
            let gain = if kind_uses_gain(b.kind) {
                format_db(b.gain_db)
            } else {
                na()
            };
            (
                (i + 1).to_string(),
                [
                    on_off(b.enabled),
                    kind_name(b.kind).into(),
                    format_hz(b.freq_hz),
                    gain,
                    format!("{:.2}", b.q),
                ],
                b.enabled,
            )
        }
        Row::InputGain => (
            "In".into(),
            [
                na(),
                if narrow { "In gain" } else { "Input gain" }.into(),
                na(),
                format_db(s.input_gain_db),
                na(),
            ],
            true,
        ),
        Row::OutputGain => (
            "Out".into(),
            [
                na(),
                if narrow { "Out gain" } else { "Output gain" }.into(),
                na(),
                format_db(s.output_gain_db),
                na(),
            ],
            true,
        ),
        Row::Limiter => (
            "Lim".into(),
            [
                on_off(s.limiter),
                "Limiter".into(),
                "-1 dBFS".into(),
                na(),
                na(),
            ],
            s.limiter,
        ),
    }
}
