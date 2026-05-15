//! Draw-list overlays painted on top of the per-pane `input_text_multiline`
//! widget: row backgrounds, sub-line spans, gutter dots, hover panel.

use std::cell::Cell;

use imgui::{StyleVar, Ui};

use crate::app::theme;
use crate::app::undo_stack::DiffEdit;
use crate::diff::{Anchor, DiffOp, Hunk, SubSpan, SubSpanKind};
use crate::session::{SessionId, TwoWaySide};

use super::common::{line_h, PendingJump, Side};
use crate::app::syntax::LineSpans;

/// Pure: 1-based row from a mouse y, the widget's top in screen space,
/// the widget's scroll, and the line height.
pub(super) fn mouse_y_to_line(mouse_y: f32, pane_top: f32, scroll_y: f32, lh: f32) -> u32 {
    if lh <= 0.0 {
        return 1;
    }
    let content_y = (mouse_y - pane_top) + scroll_y;
    let row0 = (content_y / lh).floor() as i64;
    let row0 = row0.max(0) as u32;
    row0 + 1
}

pub(super) fn hunk_move_id(hunk: &Hunk) -> Option<u32> {
    for op in &hunk.ops {
        match op {
            DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => {
                return *move_id;
            }
            DiffOp::Equal { .. } => continue,
        }
    }
    None
}

pub(super) fn find_paired_hunk(
    hunks: &[Hunk],
    move_id: u32,
    my_side: Side,
) -> Option<&Hunk> {
    let opposite_is_delete_only = matches!(my_side, Side::Right);
    hunks.iter().find(|h| {
        if hunk_move_id(h) != Some(move_id) {
            return false;
        }
        let is_delete_only = h.b_range == (0, 0);
        let is_insert_only = h.a_range == (0, 0);
        if opposite_is_delete_only {
            is_delete_only
        } else {
            is_insert_only
        }
    })
}

/// True if hunk has any Delete or Insert ops (i.e. not a pure equal hunk).
fn is_change_hunk(hunk: &Hunk) -> bool {
    hunk.ops.iter().any(|op| matches!(op, DiffOp::Delete { .. } | DiffOp::Insert { .. }))
}

/// Pure: compute the screen y of a 1-based line number, given the
/// widget's top-left content y, the widget's scroll_y, and line height.
pub(super) fn line_screen_y(widget_top: f32, line: u32, scroll_y: f32, lh: f32) -> f32 {
    widget_top + (line as f32 - 1.0) * lh - scroll_y
}

/// Paint syntax-highlighted text on top of the input_text_multiline widget.
/// imgui's widget paints monochrome text underneath; we overpaint the
/// colored spans on top so the colored characters cover the monochrome
/// ones. Plain (uncolored) text is left as-is so the caret stays visible.
///
/// `IMGUI_TEXT_PADDING_X` is imgui's frame-padding; ~4px empirically.
const IMGUI_TEXT_PADDING_X: f32 = 4.0;

pub(super) fn paint_syntax_text(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    highlights: &[LineSpans],
    scroll_y: f32,
) {
    if highlights.is_empty() {
        return;
    }
    // Use the foreground draw list so our colored text paints on top of
    // imgui's monochrome text. The multiline widget renders its text into
    // its own internal child-window draw list, which composites on top of
    // the parent window's draw list — so painting on the window draw list
    // puts colored text *under* the widget's text and it becomes invisible.
    // The foreground draw list is composited last, after all widgets.
    let dl = ui.get_foreground_draw_list();
    let lh = line_h();
    let widget_top = widget_rect[1];
    let widget_bottom = widget_rect[3];
    let widget_h = widget_bottom - widget_top;
    if widget_h <= 0.0 || lh <= 0.0 {
        return;
    }
    let char_w = ui.calc_text_size("m")[0].max(1.0);
    let text_x0 = widget_rect[0] + IMGUI_TEXT_PADDING_X;

    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    // Clip to the widget rect so colored text doesn't bleed outside the
    // scrollable area when content is scrolled.
    dl.with_clip_rect(
        [widget_rect[0], widget_rect[1]],
        [widget_rect[2], widget_rect[3]],
        || {
            for (line_idx, line_text) in buf.lines().enumerate() {
                let ln = line_idx as u32 + 1;
                if ln < first_line || ln > last_line {
                    continue;
                }
                let Some(line_spans) = highlights.get(line_idx) else {
                    continue;
                };
                if line_spans.is_empty() {
                    continue;
                }
                let y = line_screen_y(widget_top, ln, scroll_y, lh);
                if y + lh < widget_top || y > widget_bottom {
                    continue;
                }
                // Walk chars and pick each span's slice. start_col/end_col are
                // CHAR indices; convert to byte ranges by walking char indices.
                let chars: Vec<(usize, char)> = line_text.char_indices().collect();
                for span in line_spans {
                    if span.end_col <= span.start_col {
                        continue;
                    }
                    if span.start_col >= chars.len() {
                        continue;
                    }
                    let start_byte = chars[span.start_col].0;
                    let end_byte = if span.end_col >= chars.len() {
                        line_text.len()
                    } else {
                        chars[span.end_col].0
                    };
                    if end_byte <= start_byte {
                        continue;
                    }
                    let slice = &line_text[start_byte..end_byte];
                    let x = text_x0 + (span.start_col as f32) * char_w;
                    dl.add_text([x, y + 2.0], span.kind.color(), slice);
                }
            }
        },
    );
}

// ---------------------------- bezier connector ------------------------------

fn pack_color(c: [f32; 4]) -> u32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to8(c[0]) | (to8(c[1]) << 8) | (to8(c[2]) << 16) | (to8(c[3]) << 24)
}

fn v2(x: f32, y: f32) -> imgui::sys::ImVec2 {
    imgui::sys::ImVec2 { x, y }
}

const BEZIER_SEGMENTS: usize = 24;

fn cubic_bezier(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let uu = u * u;
    let uuu = uu * u;
    let tt = t * t;
    let ttt = tt * t;
    [
        uuu * p0[0] + 3.0 * uu * t * p1[0] + 3.0 * u * tt * p2[0] + ttt * p3[0],
        uuu * p0[1] + 3.0 * uu * t * p1[1] + 3.0 * u * tt * p2[1] + ttt * p3[1],
    ]
}

fn sample_curve(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) -> Vec<[f32; 2]> {
    (0..=BEZIER_SEGMENTS)
        .map(|i| cubic_bezier(p0, p1, p2, p3, i as f32 / BEZIER_SEGMENTS as f32))
        .collect()
}

fn fill_polygon(pts: &[[f32; 2]], color: [f32; 4]) {
    if pts.len() < 3 {
        return;
    }
    let mut flat: Vec<f32> = Vec::with_capacity(pts.len() * 2);
    for p in pts {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let tris = match earcutr::earcut(&flat, &[], 2) {
        Ok(t) => t,
        Err(_) => return,
    };
    if tris.is_empty() {
        return;
    }
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        let mut uv = imgui::sys::ImVec2 { x: 0.0, y: 0.0 };
        imgui::sys::igGetFontTexUvWhitePixel(&mut uv);
        let vtx_count = pts.len() as i32;
        let idx_count = tris.len() as i32;
        imgui::sys::ImDrawList_PrimReserve(dl, idx_count, vtx_count);
        let base = (*dl)._VtxCurrentIdx;
        for p in pts {
            imgui::sys::ImDrawList_PrimWriteVtx(dl, v2(p[0], p[1]), uv, col);
        }
        for &idx in &tris {
            imgui::sys::ImDrawList_PrimWriteIdx(
                dl,
                (base + idx as u32) as imgui::sys::ImDrawIdx,
            );
        }
    }
}

fn fill_bezier_ribbon(x_l: f32, x_r: f32, a1: f32, a2: f32, b1: f32, b2: f32, color: [f32; 4]) {
    let cx = (x_l + x_r) * 0.5;
    let top = sample_curve([x_l, a1], [cx, a1], [cx, b1], [x_r, b1]);
    let bot = sample_curve([x_l, a2], [cx, a2], [cx, b2], [x_r, b2]);
    let mut outline: Vec<[f32; 2]> = top;
    outline.extend(bot.into_iter().rev());
    fill_polygon(&outline, color);
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        for p in &outline {
            imgui::sys::ImDrawList_PathLineTo(dl, v2(p[0], p[1]));
        }
        imgui::sys::ImDrawList_PathStroke(
            dl,
            col,
            imgui::sys::ImDrawFlags_Closed as i32,
            1.0,
        );
    }
}

fn stroke_bezier_curve(
    x_l: f32,
    x_r: f32,
    y1: f32,
    y2: f32,
    color: [f32; 4],
    thickness: f32,
) {
    let cx = (x_l + x_r) * 0.5;
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        imgui::sys::ImDrawList_PathLineTo(dl, v2(x_l, y1));
        imgui::sys::ImDrawList_PathBezierCubicCurveTo(
            dl,
            v2(cx, y1),
            v2(cx, y2),
            v2(x_r, y2),
            0,
        );
        imgui::sys::ImDrawList_PathStroke(dl, col, imgui::sys::ImDrawFlags_None as i32, thickness);
    }
}

fn ribbon_color(is_change: bool) -> [f32; 4] {
    if is_change {
        theme::with_alpha(theme::BLUE, 0.28)
    } else {
        theme::with_alpha(theme::OVERLAY1, 0.10)
    }
}

fn move_ribbon_alpha(dy_px: f32) -> f32 {
    const RIBBON_ALPHA_NEAR: f32 = 0.30;
    const RIBBON_ALPHA_FAR: f32 = 0.08;
    const RIBBON_FADE_RANGE_PX: f32 = 800.0;
    let t = (dy_px.abs() / RIBBON_FADE_RANGE_PX).clamp(0.0, 1.0);
    RIBBON_ALPHA_NEAR + (RIBBON_ALPHA_FAR - RIBBON_ALPHA_NEAR) * t
}

/// Paint bezier ribbons + anchor curves in the 60px strip between the two
/// panes. `left_origin_y` / `right_origin_y` are the screen-y of content-y=0
/// for each pane (i.e. `widget_top - scroll_y`). `left_ranges` / `right_ranges`
/// are content-space hunk extents per pane.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_connector(
    ui: &Ui,
    origin: [f32; 2],
    w: f32,
    h: f32,
    left_origin_y: f32,
    right_origin_y: f32,
    left_ranges: &[(u32, f32, f32)],
    right_ranges: &[(u32, f32, f32)],
    anchors: &[Anchor],
    hunks: &[Hunk],
) {
    let dl = ui.get_window_draw_list();
    dl.with_clip_rect_intersect(origin, [origin[0] + w, origin[1] + h], || {
        let x_l = origin[0];
        let x_r = origin[0] + w;
        let band_top = origin[1];
        let band_bot = origin[1] + h;

        // Per-hunk change ribbons (and faint equal-hunk shading).
        for h_obj in hunks {
            let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let a1 = left_origin_y + lr.1;
            let a2 = left_origin_y + lr.2;
            let b1 = right_origin_y + rr.1;
            let b2 = right_origin_y + rr.2;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, ribbon_color(is_change_hunk(h_obj)));
        }

        // Moved-hunk ribbons (peach, distance-faded). Iterate only Delete-only
        // halves to avoid double-paint.
        for h_obj in hunks {
            let Some(move_id) = hunk_move_id(h_obj) else { continue };
            if h_obj.b_range != (0, 0) {
                continue;
            }
            let Some(paired) = find_paired_hunk(hunks, move_id, Side::Left) else {
                continue;
            };
            let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else { continue };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == paired.id) else { continue };
            let a1 = left_origin_y + lr.1;
            let a2 = left_origin_y + lr.2;
            let b1 = right_origin_y + rr.1;
            let b2 = right_origin_y + rr.2;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            let mid_a = (a1 + a2) * 0.5;
            let mid_b = (b1 + b2) * 0.5;
            let alpha = move_ribbon_alpha(mid_a - mid_b);
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, theme::with_alpha(theme::PEACH, alpha));
        }

        // Anchor curves: thin line from anchor.a row centre on left to
        // anchor.b row centre on right.
        let lh = line_h();
        for anc in anchors {
            let ly = left_origin_y + (anc.a as f32 - 1.0) * lh + lh * 0.5;
            let ry = right_origin_y + (anc.b as f32 - 1.0) * lh + lh * 0.5;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST, 3.0);
        }
    });
    let _ = dl;
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
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
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

    // Hover detection: is the mouse over a row inside this widget that
    // belongs to a change hunk on this side?
    let mouse_pos = ui.io().mouse_pos;
    let mx = mouse_pos[0];
    let my = mouse_pos[1];
    if mx >= widget_rect[0]
        && mx <= widget_rect[2]
        && my >= widget_top
        && my <= widget_bottom
    {
        let line = mouse_y_to_line(my, widget_top, scroll_y, lh);
        for h in hunks {
            if !is_change_hunk(h) {
                continue;
            }
            let range = match side {
                Side::Left => h.a_range,
                Side::Right => h.b_range,
            };
            if range == (0, 0) {
                continue;
            }
            if line >= range.0 && line <= range.1 {
                let anchor_y = line_screen_y(widget_top, range.0, scroll_y, lh)
                    .max(widget_top);
                hover_out.set(Some((h.id, [widget_rect[0], anchor_y])));
                break;
            }
        }
    }
}

/// Hover control panel anchored to the top-left of the hovered hunk.
pub(super) fn draw_control_overlay(
    ui: &Ui,
    session_id: SessionId,
    hunk_id: u32,
    pos: [f32; 2],
    pending_edits: &mut Vec<DiffEdit>,
    hunks: &[Hunk],
    side: Side,
    pending_jump_out: &Cell<Option<PendingJump>>,
) {
    let _pad = ui.push_style_var(StyleVar::FramePadding([6.0, 2.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([4.0, 0.0]));

    let hunk = hunks.iter().find(|h| h.id == hunk_id);
    let move_id = hunk.and_then(hunk_move_id);
    let paired = move_id.and_then(|id| find_paired_hunk(hunks, id, side));
    let is_moved_with_pair = paired.is_some();

    let panel_x = pos[0] + 4.0;
    let panel_y = pos[1] + 2.0;
    let panel_w: f32 = if is_moved_with_pair { 240.0 } else { 200.0 };
    let panel_h = line_h() - 4.0;

    let dl = ui.get_window_draw_list();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        theme::with_alpha(theme::MANTLE, 0.95),
    )
    .filled(true)
    .rounding(4.0)
    .build();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        theme::BLUE,
    )
    .rounding(4.0)
    .thickness(1.0)
    .build();

    ui.set_cursor_screen_pos([panel_x + 6.0, panel_y + 3.0]);
    if ui.small_button(format!("Apply A -> B##ov{hunk_id}_atob")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::B,
            old_target_text: None,
        });
    }
    ui.same_line();
    if ui.small_button(format!("B -> A##ov{hunk_id}_btoa")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::A,
            old_target_text: None,
        });
    }
    if is_moved_with_pair {
        ui.same_line();
        if ui.small_button(format!("v^##ov{hunk_id}_jump")) {
            if let Some(p) = paired {
                let target_line = match side {
                    Side::Left => p.b_range.0,
                    Side::Right => p.a_range.0,
                };
                let target_pane = match side {
                    Side::Left => Side::Right,
                    Side::Right => Side::Left,
                };
                pending_jump_out.set(Some(PendingJump {
                    session_id,
                    pane: target_pane,
                    target_line,
                }));
            }
        }
    }
}

/// Paint per-row line numbers + anchor dots in the gutter strip.
pub(super) fn paint_gutter(
    ui: &Ui,
    gutter_rect: [f32; 4],
    anchors: &[Anchor],
    side: Side,
    scroll_y: f32,
    line_count: u32,
) {
    let dl = ui.get_window_draw_list();
    let lh = line_h();
    let g_top = gutter_rect[1];
    let g_bottom = gutter_rect[3];
    let g_h = g_bottom - g_top;
    if g_h <= 0.0 || lh <= 0.0 {
        return;
    }
    let g_left = gutter_rect[0];
    let g_w = gutter_rect[2] - g_left;
    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + g_h) / lh).ceil() as u32 + 1;

    let line_no_color = theme::OVERLAY1;
    for line in first_line..=last_line.min(line_count) {
        let y = line_screen_y(g_top, line, scroll_y, lh);
        if y + lh < g_top || y > g_bottom {
            continue;
        }
        let text = format!("{line}");
        let text_w = ui.calc_text_size(&text)[0];
        dl.add_text([g_left + g_w - 4.0 - text_w, y + 2.0], line_no_color, &text);
    }

    let dot_color = theme::LAVENDER;
    for anc in anchors {
        let line = match side {
            Side::Left => anc.a,
            Side::Right => anc.b,
        };
        let y = line_screen_y(g_top, line, scroll_y, lh) + lh * 0.5;
        if y < g_top || y > g_bottom {
            continue;
        }
        dl.add_circle([g_left + g_w * 0.5, y], 3.0, dot_color)
            .filled(true)
            .build();
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

    #[test]
    fn anchor_click_maps_mouse_y_to_line() {
        // Formula: ((mouse_y - pane_top) + scroll_y) / lh, then +1 (1-based).
        // pane_top=100, lh=20, scroll_y=40:
        //   mouse_y=120 → content_y=60 → row0=3 → 1-based row 4
        assert_eq!(mouse_y_to_line(120.0, 100.0, 40.0, 20.0), 4);
        //   mouse_y=100 → content_y=40 → row0=2 → 1-based row 3
        assert_eq!(mouse_y_to_line(100.0, 100.0, 40.0, 20.0), 3);
        //   mouse_y=110 → content_y=50 → row0=2 → 1-based row 3 (within row)
        assert_eq!(mouse_y_to_line(110.0, 100.0, 40.0, 20.0), 3);
    }

    #[test]
    fn mouse_y_to_line_clamps_below_top() {
        // Below widget top: should clamp to row 1 (not panic / negative).
        assert_eq!(mouse_y_to_line(50.0, 100.0, 0.0, 20.0), 1);
    }
}
