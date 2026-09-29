use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::canvas::{Canvas, Line};

use super::{panel, theme};
use crate::app::App;
use crate::spectrum::{MAX_DISPLAY_HZ, MIN_DISPLAY_HZ, display_freqs};

const DB_RANGE: f64 = 15.0;
const GRID_DB: [f64; 5] = [-12.0, -6.0, 0.0, 6.0, 12.0];
const GRID_HZ: [(f32, &str); 8] = [
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
];
/// The spectrum's dBFS range, stretched over the curve's vertical extent.
const SPECTRUM_FLOOR_DB: f32 = -90.0;
const DB_LABEL_WIDTH: u16 = 4;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Response", false).title_bottom(
        TextLine::from(vec![
            Span::styled(" ── ", theme::accent()),
            Span::styled("EQ  ", theme::muted()),
            Span::styled("── ", Style::new().fg(theme::SPECTRUM)),
            Span::styled("output spectrum ", theme::muted()),
        ])
        .right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width <= DB_LABEL_WIDTH + 2 || inner.height == 0 {
        return;
    }
    // On short terminals the curve keeps a row or two without labels or
    // markers: enough to see the overall shape.
    let tiny = inner.height < 3;
    let (plot_row, freq_row) = if tiny {
        (inner, None)
    } else {
        let [plot_row, freq_row] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        (plot_row, Some(freq_row))
    };
    let [db_col, plot] =
        Layout::horizontal([Constraint::Length(DB_LABEL_WIDTH), Constraint::Min(1)])
            .areas(plot_row);

    let freqs = display_freqs();
    let sample_rate = app.sample_rate();
    let response: Vec<f64> = freqs
        .iter()
        .map(|&f| presence_dsp::response_db(&app.settings, sample_rate, f) as f64)
        .collect();
    let markers = if tiny {
        Vec::new()
    } else {
        band_markers(app, sample_rate)
    };

    let curve_color = if app.bypass {
        theme::DIM
    } else {
        theme::ACCENT
    };
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([x_of(MIN_DISPLAY_HZ), x_of(MAX_DISPLAY_HZ)])
        .y_bounds([-DB_RANGE, DB_RANGE])
        .paint(|ctx| {
            let (x0, x1) = (x_of(MIN_DISPLAY_HZ), x_of(MAX_DISPLAY_HZ));
            // In a row or two the grid would cover everything; only the
            // 0 dB line stays as a reference.
            let grid_db: &[f64] = if tiny { &[0.0] } else { &GRID_DB };
            let grid_hz: &[(f32, &str)] = if tiny { &[] } else { &GRID_HZ };
            for &db in grid_db {
                let color = if db == 0.0 {
                    theme::GRID_ZERO
                } else {
                    theme::GRID
                };
                ctx.draw(&Line {
                    x1: x0,
                    y1: db,
                    x2: x1,
                    y2: db,
                    color,
                });
            }
            for &(hz, _) in grid_hz {
                let x = x_of(hz);
                ctx.draw(&Line {
                    x1: x,
                    y1: -DB_RANGE,
                    x2: x,
                    y2: DB_RANGE,
                    color: theme::GRID,
                });
            }
            ctx.layer();

            // An outline rather than a filled area, so the grid and the EQ
            // curve stay readable underneath.
            for (f, db) in freqs.windows(2).zip(app.spectrum.windows(2)) {
                let (y1, y2) = (spectrum_y(db[0]), spectrum_y(db[1]));
                if y1 > -DB_RANGE || y2 > -DB_RANGE {
                    ctx.draw(&Line {
                        x1: x_of(f[0]),
                        y1,
                        x2: x_of(f[1]),
                        y2,
                        color: theme::SPECTRUM,
                    });
                }
            }
            ctx.layer();

            for (i, pair) in response.windows(2).enumerate() {
                ctx.draw(&Line {
                    x1: x_of(freqs[i]),
                    y1: clamp_db(pair[0]),
                    x2: x_of(freqs[i + 1]),
                    y2: clamp_db(pair[1]),
                    color: curve_color,
                });
            }

            for m in &markers {
                ctx.print(
                    x_of(m.freq),
                    clamp_db(m.db),
                    Span::styled(m.label.clone(), m.style),
                );
            }
        });
    frame.render_widget(canvas, plot);

    let buf = frame.buffer_mut();
    if let Some(freq_row) = freq_row {
        draw_db_labels(buf, db_col, plot);
        draw_freq_labels(buf, freq_row, plot);
    }
}

struct BandMarker {
    label: String,
    freq: f32,
    db: f64,
    style: Style,
}

fn band_markers(app: &App, sample_rate: f32) -> Vec<BandMarker> {
    let selected = app.selected_band();
    let hp_selected = matches!(app.selected_row(), crate::app::Row::HighPass);
    let style_for = |is_selected: bool| {
        if is_selected {
            theme::cursor()
        } else {
            Style::new()
                .fg(Color::Black)
                .bg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        }
    };
    let at = |f: f32| presence_dsp::response_db(&app.settings, sample_rate, f) as f64;
    let mut markers = Vec::new();
    let hp = app.settings.high_pass;
    if hp.enabled || hp_selected {
        markers.push(BandMarker {
            label: "HP".into(),
            freq: hp.freq_hz,
            db: at(hp.freq_hz),
            style: style_for(hp_selected),
        });
    }
    for (i, band) in app.settings.bands.iter().enumerate() {
        let is_selected = selected == Some(i);
        if band.enabled || is_selected {
            markers.push(BandMarker {
                label: (i + 1).to_string(),
                freq: band.freq_hz,
                db: at(band.freq_hz),
                style: style_for(is_selected),
            });
        }
    }
    // Draw the selected marker last so it sits on top of any neighbour.
    markers.sort_by_key(|m| m.style == theme::cursor());
    markers
}

fn x_of(hz: f32) -> f64 {
    (hz as f64).log10()
}

fn clamp_db(db: f64) -> f64 {
    if db.is_finite() {
        db.clamp(-DB_RANGE, DB_RANGE)
    } else {
        -DB_RANGE
    }
}

fn spectrum_y(db: f32) -> f64 {
    let t = ((db - SPECTRUM_FLOOR_DB) / -SPECTRUM_FLOOR_DB).clamp(0.0, 1.0) as f64;
    -DB_RANGE + t * 2.0 * DB_RANGE
}

/// Column in `plot` for a frequency. Mirrors the canvas's braille mapping
/// (2×4 dots per cell) so labels line up with the grid lines it draws.
fn column_of(plot: Rect, hz: f32) -> u16 {
    let t = (x_of(hz) - x_of(MIN_DISPLAY_HZ)) / (x_of(MAX_DISPLAY_HZ) - x_of(MIN_DISPLAY_HZ));
    let dots = (plot.width as f64 * 2.0 - 1.0).max(0.0);
    plot.x + ((t * dots) as u16 / 2).min(plot.width.saturating_sub(1))
}

fn row_of(plot: Rect, db: f64) -> u16 {
    let t = (DB_RANGE - db) / (2.0 * DB_RANGE);
    let dots = (plot.height as f64 * 4.0 - 1.0).max(0.0);
    plot.y + ((t * dots) as u16 / 4).min(plot.height.saturating_sub(1))
}

fn draw_db_labels(buf: &mut Buffer, col: Rect, plot: Rect) {
    for db in GRID_DB {
        let label = if db == 0.0 {
            "0".to_owned()
        } else {
            format!("{db:+.0}")
        };
        let y = row_of(plot, db);
        let x = col.x + col.width.saturating_sub(label.len() as u16 + 1);
        buf.set_string(x, y, label, theme::muted());
    }
}

fn draw_freq_labels(buf: &mut Buffer, row: Rect, plot: Rect) {
    let mut next_free = row.x;
    for (hz, label) in GRID_HZ {
        let center = column_of(plot, hz);
        let x = center.saturating_sub(label.len() as u16 / 2);
        if x < next_free || x + label.len() as u16 > row.right() {
            continue;
        }
        buf.set_string(x, row.y, label, theme::muted());
        next_free = x + label.len() as u16 + 1;
    }
    let hz = "Hz";
    if row.width > 4 {
        buf.set_string(row.right() - hz.len() as u16, row.y, hz, theme::dim());
    }
}
