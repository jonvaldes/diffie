//! Shared per-line text painter used by both the 2-way and 3-way diff views.
//!
//! Walks a line's syntax-highlight spans in order and emits one `add_text`
//! call per span (in the span's color) plus default-colored gaps and a tail.
//! Lines without spans render in a single default-colored `add_text` call.
//! Both view kinds suppress imgui's own text rendering and rely on this
//! helper to paint text on the foreground draw list.

use imgui::{DrawListMut, Ui};

use crate::app::syntax::LineSpans;
use crate::app::theme;

/// Snap a byte offset to the nearest preceding char boundary in `s`.
pub fn snap_to_char_boundary(s: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(s.len());
    let mut snap = clamped;
    while snap > 0 && !s.is_char_boundary(snap) {
        snap -= 1;
    }
    snap
}

/// Compute the x offset of a byte position within `line`, clamped to a
/// char boundary, using imgui's font metrics (matches the multiline widget's
/// own hit-testing).
pub(crate) fn text_x_at_byte(ui: &Ui, line: &str, byte_offset: usize, padding_x: f32) -> f32 {
    let snap = snap_to_char_boundary(line, byte_offset);
    padding_x + ui.calc_text_size(&line[..snap])[0]
}

/// Paint one line of text on a draw list. If `line_spans` is provided and
/// non-empty, emit one chunk per span boundary: default-color gaps +
/// span-colored ranges + default-color tail. Otherwise emit the whole line
/// in `theme::TEXT()`. `line_origin` is the screen-space position of byte 0
/// (i.e. `[widget_left + padding_x - scroll_x, y]`).
pub fn paint_line_with_spans(
    ui: &Ui,
    dl: &DrawListMut,
    line_origin: [f32; 2],
    line_text: &str,
    line_spans: Option<&LineSpans>,
    scroll_x: f32,
    padding_x: f32,
) {
    if line_text.is_empty() {
        return;
    }
    let widget_left = line_origin[0] - padding_x + scroll_x;
    let text_y = line_origin[1];

    let Some(line_spans) = line_spans.filter(|v| !v.is_empty()) else {
        dl.add_text(line_origin, theme::TEXT(), line_text);
        return;
    };

    let chars: Vec<(usize, char)> = line_text.char_indices().collect();
    let mut cursor_col: usize = 0;
    for span in line_spans {
        let s = span.start_col;
        let e = span.end_col.min(chars.len());
        if e <= s {
            continue;
        }
        // Default-colored gap before this span.
        if s > cursor_col {
            let gap_start_byte = chars[cursor_col].0;
            let gap_end_byte = if s >= chars.len() {
                line_text.len()
            } else {
                chars[s].0
            };
            if gap_end_byte > gap_start_byte {
                let x = widget_left - scroll_x
                    + text_x_at_byte(ui, line_text, gap_start_byte, padding_x);
                dl.add_text(
                    [x, text_y],
                    theme::TEXT(),
                    &line_text[gap_start_byte..gap_end_byte],
                );
            }
        }
        // Colored span.
        if s >= chars.len() {
            cursor_col = s;
            continue;
        }
        let span_start_byte = chars[s].0;
        let span_end_byte = if e >= chars.len() {
            line_text.len()
        } else {
            chars[e].0
        };
        if span_end_byte > span_start_byte {
            let x = widget_left - scroll_x
                + text_x_at_byte(ui, line_text, span_start_byte, padding_x);
            dl.add_text(
                [x, text_y],
                span.kind.color(),
                &line_text[span_start_byte..span_end_byte],
            );
        }
        cursor_col = e;
    }
    // Tail after the last span.
    if cursor_col < chars.len() {
        let tail_byte = chars[cursor_col].0;
        if tail_byte < line_text.len() {
            let x = widget_left - scroll_x
                + text_x_at_byte(ui, line_text, tail_byte, padding_x);
            dl.add_text([x, text_y], theme::TEXT(), &line_text[tail_byte..]);
        }
    }
}

/// Paint a blinking vertical caret line at byte offset `caret_byte` inside
/// `buf`, clipped to `widget_rect = [left, top, right, bottom]`. Walks lines
/// to locate the caret's row + column. Skips painting entirely when
/// `caret_byte < 0` or the current blink half is "off". Shared by all three
/// text panes since they each suppress imgui's native caret to render
/// their own syntax-colored text on the draw list.
#[allow(clippy::too_many_arguments)]
pub fn paint_caret(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    caret_byte: i32,
    scroll_x: f32,
    scroll_y: f32,
    padding_x: f32,
    padding_y: f32,
    lh: f32,
) {
    if caret_byte < 0 || lh <= 0.0 {
        return;
    }
    let blink_on = (ui.time() * 2.0).rem_euclid(2.0) < 1.0;
    if !blink_on {
        return;
    }
    let widget_left = widget_rect[0];
    let widget_top = widget_rect[1];
    let widget_right = widget_rect[2];
    let widget_bottom = widget_rect[3];
    let dl = ui.get_window_draw_list();
    let target = caret_byte as usize;
    let mut byte_acc: usize = 0;
    let mut painted = false;
    for (line_idx, line_text) in buf.lines().enumerate() {
        let line_end = byte_acc + line_text.len();
        if target >= byte_acc && target <= line_end {
            let local = target - byte_acc;
            let x = widget_left - scroll_x + text_x_at_byte(ui, line_text, local, padding_x);
            let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
            if y + lh >= widget_top && y <= widget_bottom && x >= widget_left && x <= widget_right {
                dl.add_line([x, y + 1.0], [x, y + lh - 1.0], theme::TEXT())
                    .thickness(1.0)
                    .build();
            }
            painted = true;
            break;
        }
        byte_acc = line_end + 1; // +1 for '\n'
    }
    if !painted && target >= byte_acc {
        let line_idx = buf.lines().count();
        let x = widget_left + padding_x - scroll_x;
        let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
        if y + lh >= widget_top && y <= widget_bottom {
            dl.add_line([x, y + 1.0], [x, y + lh - 1.0], theme::TEXT())
                .thickness(1.0)
                .build();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_boundary_is_no_op_for_ascii() {
        let s = "hello";
        for i in 0..=s.len() {
            assert_eq!(snap_to_char_boundary(s, i), i);
        }
    }

    #[test]
    fn snap_to_boundary_clamps_past_end() {
        let s = "hi";
        assert_eq!(snap_to_char_boundary(s, 99), s.len());
    }

    #[test]
    fn snap_to_boundary_snaps_mid_codepoint() {
        // Test with a string that has a 2-byte UTF-8 character at the end.
        // Use "hi\u{0301}" (combining accent) to ensure we have mid-codepoint cases.
        let s = "h\u{00e9}"; // "hé" - 'h' (1 byte) + 'é' (2 bytes)
        assert_eq!(snap_to_char_boundary(s, 0), 0); // 'h' boundary
        assert_eq!(snap_to_char_boundary(s, 1), 1); // 'é' start
        assert_eq!(snap_to_char_boundary(s, 2), 1); // mid 'é', snaps back
        assert_eq!(snap_to_char_boundary(s, 3), 3); // end of string
    }

    #[test]
    fn snap_to_boundary_empty_string() {
        assert_eq!(snap_to_char_boundary("", 0), 0);
        assert_eq!(snap_to_char_boundary("", 99), 0);
    }
}
