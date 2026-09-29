use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;

use super::{panel, theme};
use crate::app::App;

const MIN_DB: f32 = -60.0;
const SCALE: [f32; 8] = [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0];
const EIGHTHS: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];
const BAR_WIDTH: u16 = 3;

const SCALE_WIDTH: u16 = 4;
/// Narrowest inner width that still draws: scale plus two 1-column bars with
/// room for integer readouts ("-51").
pub const MIN_INNER_WIDTH: u16 = SCALE_WIDTH + 2 * 4;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Levels", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < MIN_INNER_WIDTH {
        return;
    }

    // Bars, then a label row and a peak readout row.
    let [bars, labels, readout] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    // Each channel gets its own slot so labels and readouts can't collide.
    let [scale_col, in_slot, out_slot] = Layout::horizontal([
        Constraint::Length(SCALE_WIDTH),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .areas(bars);

    let m = app.engine.meters;
    let live = app.engine.state == crate::app::EngineState::Running;
    let (in_peak, in_rms, out_peak, out_rms) = if live {
        (
            m.input_peak_db,
            m.input_rms_db,
            m.output_peak_db,
            m.output_rms_db,
        )
    } else {
        (
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        )
    };

    let buf = frame.buffer_mut();
    draw_scale(buf, scale_col);
    for (slot, label, peak, rms) in [
        (in_slot, "IN", in_peak, in_rms),
        (out_slot, "OUT", out_peak, out_rms),
    ] {
        // One column of gap on the right keeps neighbours apart.
        let usable = slot.width.saturating_sub(1).max(1);
        let bar_width = BAR_WIDTH.min(usable.saturating_sub(1)).max(1);
        let bar = Rect {
            x: slot.x + (usable - bar_width) / 2,
            width: bar_width,
            ..slot
        };
        draw_bar(buf, bar, peak, rms);
        buf.set_string(slot.x, labels.y, centered(label, usable), theme::muted());
        let (text, style) = readout_text(peak, usable);
        buf.set_string(slot.x, readout.y, centered(&text, usable), style);
    }
}

fn fraction(db: f32) -> f32 {
    if db.is_finite() {
        ((db - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// dB at the centre of row `r` counted from the bottom.
fn db_at(r: u16, height: u16) -> f32 {
    MIN_DB + (r as f32 + 0.5) / height as f32 * -MIN_DB
}

fn draw_scale(buf: &mut Buffer, area: Rect) {
    let mut last_y = None;
    for db in SCALE {
        let from_bottom = (fraction(db) * (area.height - 1) as f32).round() as u16;
        let y = area.bottom() - 1 - from_bottom;
        if last_y == Some(y) {
            continue;
        }
        last_y = Some(y);
        let label = format!("{:>3}", db as i32);
        buf.set_string(area.x, y, label, theme::dim());
    }
}

fn draw_bar(buf: &mut Buffer, area: Rect, peak_db: f32, rms_db: f32) {
    let h = area.height;
    let rms_cells = fraction(rms_db) * h as f32;
    let peak_row = (fraction(peak_db) * h as f32).floor().min((h - 1) as f32) as u16;
    for r in 0..h {
        let y = area.bottom() - 1 - r;
        let zone = theme::level_color(db_at(r, h));
        let fill = rms_cells - r as f32;
        let symbol = if fill >= 1.0 {
            "█"
        } else if fill > 0.0 {
            EIGHTHS[(fill * 8.0) as usize]
        } else {
            " "
        };
        let mut style = Style::new().fg(zone).bg(theme::METER_BG);
        let mut symbol = symbol;
        if peak_db.is_finite() && peak_db > MIN_DB && r == peak_row && fill < 1.0 {
            symbol = "▔";
            style = style.fg(theme::level_color(peak_db));
        }
        for x in area.x..area.right() {
            buf[(x, y)].set_symbol(symbol).set_style(style);
        }
    }
}

/// Peak readout: one decimal when there's room, whole dB otherwise.
fn readout_text(peak_db: f32, width: u16) -> (String, Style) {
    if peak_db.is_finite() && peak_db > MIN_DB {
        let text = if width >= 5 {
            format!("{peak_db:.1}")
        } else {
            format!("{peak_db:.0}")
        };
        (text, Style::new().fg(theme::level_color(peak_db)))
    } else {
        ("-∞".to_owned(), theme::dim())
    }
}

fn centered(s: &str, width: u16) -> String {
    format!("{s:^width$}", width = width as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readout_fits_its_slot() {
        assert_eq!(readout_text(-51.14, 5).0, "-51.1");
        assert_eq!(readout_text(-51.14, 4).0, "-51");
        assert_eq!(readout_text(f32::NEG_INFINITY, 4).0, "-∞");
    }
}
