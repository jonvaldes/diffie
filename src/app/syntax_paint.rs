//! Shared per-line text painter used by both the 2-way and 3-way diff views.
//!
//! Walks a line's syntax-highlight spans in order and emits one `add_text`
//! call per span (in the span's color) plus default-colored gaps and a tail.
//! Lines without spans render in a single default-colored `add_text` call.
//! Both view kinds suppress imgui's own text rendering and rely on this
//! helper to paint text on the foreground draw list.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use imgui::{DrawListMut, Ui};

use crate::app::syntax::LineSpans;
use crate::app::theme;

/// Global toggle for rendering whitespace characters as visible glyphs.
/// Set from `AppPreferences::show_whitespace`; read by the paint path.
static SHOW_WS: AtomicBool = AtomicBool::new(false);

pub fn set_show_whitespace(on: bool) {
    SHOW_WS.store(on, Ordering::Relaxed);
}

pub fn show_whitespace_enabled() -> bool {
    SHOW_WS.load(Ordering::Relaxed)
}

/// EOL glyph codepoint shown in the whitespace ghost layer. Updated when the
/// user switches code fonts: `⏎` (U+23CE) when the active primary covers it,
/// `¶` (U+00B6) for Noto Sans Mono, which doesn't ship U+23CE.
static EOL_CODEPOINT: AtomicU32 = AtomicU32::new(0x23ce);

pub fn set_eol_glyph(cp: u32) {
    EOL_CODEPOINT.store(cp, Ordering::Relaxed);
}

fn eol_glyph() -> char {
    char::from_u32(EOL_CODEPOINT.load(Ordering::Relaxed)).unwrap_or('\u{00b6}')
}

/// Build a "ghost" copy of `line` where spaces become `·`, tabs become `→`,
/// and every other char is replaced with a single space. A trailing `↵`
/// stands in for the line break itself so EOLs are visible too. Painted
/// underneath the real text in a dim color so whitespace stays visible while
/// real glyphs (drawn on top) still appear in their syntax colors. Works in
/// monospace because `·`/`→` occupy a single cell — the same width as the
/// space they replace. Tabs aren't perfectly aligned (their rendered width
/// varies), but the arrow lands where the tab starts, which is the useful
/// signal. Uses `¶` (U+00B6) rather than `↵` (U+21B5) because none of the
/// embedded code fonts (JetBrains Mono, Fira Code, Cascadia Code, Noto Sans
/// Mono) actually ship U+21B5 — their arrows block stops at U+2195/U+2199 —
/// so the Noto fallback can't fill the hole either. The pilcrow is in
/// Latin-1 and present in every font we ship.
fn whitespace_ghost(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 3);
    for ch in line.chars() {
        match ch {
            ' ' => out.push('\u{00b7}'),
            '\t' => out.push('\u{2192}'),
            _ => out.push(' '),
        }
    }
    out.push(eol_glyph());
    out
}

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
    let show_ws = show_whitespace_enabled();
    if line_text.is_empty() {
        if show_ws {
            let mut buf = [0u8; 4];
            dl.add_text(line_origin, theme::OVERLAY0(), eol_glyph().encode_utf8(&mut buf));
        }
        return;
    }
    let widget_left = line_origin[0] - padding_x + scroll_x;
    let text_y = line_origin[1];

    // Underlay: render whitespace glyphs in a dim color first; real text
    // (drawn afterward) covers the ghost where chars are non-whitespace.
    if show_ws {
        let ghost = whitespace_ghost(line_text);
        dl.add_text(line_origin, theme::OVERLAY0(), &ghost);
    }

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

/// Iterate the visible subset of `buf`'s lines and paint each line's text
/// via [`paint_line_with_spans`]. For each visible line the caller's
/// `per_line` closure is invoked first with `(dl, line_idx, line_text,
/// ln_1based, y_top_clipped, y_bottom_clipped)` so it can paint per-row
/// decorations (row backgrounds, hunk tints, sub-line spans, …) before the
/// text is drawn. The widget-rect clip rect is pushed once around the whole
/// loop. Shared by diff_view, merge_view, and result_pane.
#[allow(clippy::too_many_arguments)]
pub fn paint_text_lines<F>(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    highlights: &[LineSpans],
    scroll_x: f32,
    scroll_y: f32,
    padding_x: f32,
    padding_y: f32,
    lh: f32,
    mut per_line: F,
) where
    F: FnMut(&imgui::DrawListMut, usize, &str, u32, f32, f32),
{
    if widget_rect[3] <= widget_rect[1] || lh <= 0.0 {
        return;
    }
    let widget_left = widget_rect[0];
    let widget_top = widget_rect[1];
    let widget_right = widget_rect[2];
    let widget_bottom = widget_rect[3];
    let widget_h = widget_bottom - widget_top;

    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    let dl = ui.get_window_draw_list();
    dl.with_clip_rect([widget_left, widget_top], [widget_right, widget_bottom], || {
        for (line_idx, line_text) in buf.lines().enumerate() {
            let ln = (line_idx as u32) + 1;
            if ln < first_line || ln > last_line {
                continue;
            }
            let y = widget_top + padding_y + (ln as f32 - 1.0) * lh - scroll_y;
            if y + lh < widget_top || y > widget_bottom {
                continue;
            }
            let y0 = y.max(widget_top);
            let y1 = (y + lh).min(widget_bottom);
            per_line(&dl, line_idx, line_text, ln, y0, y1);
            let line_origin = [widget_left + padding_x - scroll_x, y];
            paint_line_with_spans(
                ui,
                &dl,
                line_origin,
                line_text,
                highlights.get(line_idx),
                scroll_x,
                padding_x,
            );
        }
    });
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
    dl: &imgui::DrawListMut,
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
    let target = caret_byte as usize;
    let mut byte_acc: usize = 0;
    let mut painted = false;
    // Paint the caret as a 1px-wide filled rect rather than `add_line` with
    // thickness 1.0 — that's an AA-stroked polyline, and on Windows the
    // anti-aliasing spreads a single-pixel vertical line across two columns
    // at ~50% coverage each, which can render as invisible. A filled rect
    // is pixel-aligned and reliable on every platform.
    let draw_caret = |dl: &imgui::DrawListMut, x: f32, y: f32| {
        dl.add_rect([x, y + 1.0], [x + 1.0, y + lh - 1.0], theme::TEXT())
            .filled(true)
            .build();
    };
    for (line_idx, line_text) in buf.lines().enumerate() {
        let line_end = byte_acc + line_text.len();
        if target >= byte_acc && target <= line_end {
            let local = target - byte_acc;
            let x = widget_left - scroll_x + text_x_at_byte(ui, line_text, local, padding_x);
            let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
            if y + lh >= widget_top && y <= widget_bottom && x >= widget_left && x <= widget_right {
                draw_caret(dl, x, y);
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
            draw_caret(dl, x, y);
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
