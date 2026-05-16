//! Shared per-line text painter used by both the 2-way and 3-way diff views.
//!
//! Walks a line's syntax-highlight spans in order and emits one `add_text`
//! call per span (in the span's color) plus default-colored gaps and a tail.
//! Lines without spans render in a single default-colored `add_text` call.
//! Both view kinds suppress imgui's own text rendering and rely on this
//! helper to paint text on the foreground draw list.

use imgui::Ui;

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

/// Paint one line of text on the current window's draw list. If `line_spans`
/// is provided and non-empty, emit one chunk per span boundary: default-color
/// gaps + span-colored ranges + default-color tail. Otherwise emit the whole
/// line in `theme::TEXT()`. `line_origin_x` is the screen-space x of byte 0 of
/// the line (i.e. `widget_left + padding_x - scroll_x`).
pub fn paint_line_with_spans(
    ui: &Ui,
    line_origin: [f32; 2],
    line_text: &str,
    line_spans: Option<&LineSpans>,
    scroll_x: f32,
    padding_x: f32,
) {
    if line_text.is_empty() {
        return;
    }
    let dl = ui.get_window_draw_list();
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
