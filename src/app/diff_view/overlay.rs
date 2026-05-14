//! Draw-list overlays painted on top of the per-pane `input_text_multiline`
//! widget: row backgrounds, sub-line spans. Pure functions where possible.

use imgui::Ui;

use crate::app::theme;
use crate::diff::{DiffOp, Hunk, SubSpan, SubSpanKind};

use super::common::{line_h, Side};

/// Pure: compute the screen y of a 1-based line number, given the
/// widget's top-left content y, the widget's scroll_y, and line height.
pub(super) fn line_screen_y(widget_top: f32, line: u32, scroll_y: f32, lh: f32) -> f32 {
    widget_top + (line as f32 - 1.0) * lh - scroll_y
}

#[derive(Copy, Clone)]
enum OpKind {
    Equal,
    Delete,
    Insert,
}

/// Paint per-row backgrounds (Equal / Delete / Insert / Moved) and
/// sub-line span highlights for one pane.
///
/// `widget_rect = [x0, y0, x1, y1]` is the screen-space rect of the
/// pane's text content (just the input_text_multiline, not including
/// the gutter). `scroll_y` is the pane's vertical scroll.
pub(super) fn paint_row_overlays(
    ui: &Ui,
    widget_rect: [f32; 4],
    hunks: &[Hunk],
    side: Side,
    scroll_y: f32,
) {
    let dl = ui.get_window_draw_list();
    let lh = line_h();
    let widget_top = widget_rect[1];
    let widget_bottom = widget_rect[3];
    let widget_h = widget_bottom - widget_top;
    if widget_h <= 0.0 || lh <= 0.0 {
        return;
    }

    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    // Approximate monospace char width for sub-line span x-offsets.
    let char_w = ui.calc_text_size("m")[0].max(1.0);

    for h in hunks {
        let range = match side {
            Side::Left => h.a_range,
            Side::Right => h.b_range,
        };
        if range == (0, 0) {
            continue;
        }
        if range.1 < first_line || range.0 > last_line {
            continue;
        }

        for op in &h.ops {
            let (ln, op_kind, move_id, spans): (u32, OpKind, Option<u32>, Option<&Vec<SubSpan>>) =
                match (side, op) {
                    (Side::Left, DiffOp::Equal { a, .. }) => (*a, OpKind::Equal, None, None),
                    (Side::Left, DiffOp::Delete { a, move_id, spans, .. }) => {
                        (*a, OpKind::Delete, *move_id, spans.as_ref())
                    }
                    (Side::Right, DiffOp::Equal { b, .. }) => (*b, OpKind::Equal, None, None),
                    (Side::Right, DiffOp::Insert { b, move_id, spans, .. }) => {
                        (*b, OpKind::Insert, *move_id, spans.as_ref())
                    }
                    _ => continue,
                };
            if ln < first_line || ln > last_line {
                continue;
            }
            let y = line_screen_y(widget_top, ln, scroll_y, lh);

            // Background
            let bg = if move_id.is_some() {
                Some(theme::with_alpha(theme::PEACH, 0.30))
            } else {
                match op_kind {
                    OpKind::Equal => None,
                    OpKind::Delete => Some([0.55, 0.18, 0.18, 0.30]),
                    OpKind::Insert => Some([0.18, 0.50, 0.22, 0.30]),
                }
            };
            if let Some(color) = bg {
                let y0 = y.max(widget_top);
                let y1 = (y + lh).min(widget_bottom);
                if y1 > y0 {
                    dl.add_rect(
                        [widget_rect[0], y0],
                        [widget_rect[2], y1],
                        color,
                    )
                    .filled(true)
                    .build();
                }
            }

            // Sub-line spans — paint Changed spans with a stronger tint.
            if let Some(spans) = spans {
                let span_color = match op_kind {
                    OpKind::Delete => [0.75, 0.20, 0.20, 0.45],
                    OpKind::Insert => [0.20, 0.65, 0.25, 0.45],
                    OpKind::Equal => continue,
                };
                let y0 = y.max(widget_top);
                let y1 = (y + lh).min(widget_bottom);
                if y1 <= y0 {
                    continue;
                }
                for sp in spans {
                    if !matches!(sp.kind, SubSpanKind::Changed) {
                        continue;
                    }
                    if sp.end <= sp.start {
                        continue;
                    }
                    // Approximate: monospace byte→pixel. Good enough for
                    // ASCII-heavy code; multi-byte UTF-8 will be slightly off.
                    let x0 = widget_rect[0] + char_w * sp.start as f32;
                    let x1 = widget_rect[0] + char_w * sp.end as f32;
                    let x0c = x0.max(widget_rect[0]).min(widget_rect[2]);
                    let x1c = x1.max(widget_rect[0]).min(widget_rect[2]);
                    if x1c > x0c {
                        dl.add_rect([x0c, y0], [x1c, y1], span_color)
                            .filled(true)
                            .build();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_y_at_zero_scroll() {
        assert_eq!(line_screen_y(100.0, 1, 0.0, 20.0), 100.0);
        assert_eq!(line_screen_y(100.0, 5, 0.0, 20.0), 180.0);
    }

    #[test]
    fn line_y_with_scroll() {
        // widget_top=100, line 5, scroll_y=40, line_h=20 → 100 + 80 - 40 = 140
        assert_eq!(line_screen_y(100.0, 5, 40.0, 20.0), 140.0);
    }

    #[test]
    fn line_y_for_first_line_with_scroll() {
        // widget_top=100, line 1, scroll_y=40, line_h=20 → 100 + 0 - 40 = 60
        assert_eq!(line_screen_y(100.0, 1, 40.0, 20.0), 60.0);
    }
}
