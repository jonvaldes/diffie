//! Draw-list overlays painted on top of the per-pane `input_text_multiline`
//! widget: row backgrounds, sub-line spans, gutter dots, hover panel.

use std::cell::Cell;

use imgui::{Condition, StyleVar, Ui, WindowFlags};

use crate::app::theme;
use crate::app::undo_stack::DiffEdit;
use crate::diff::{Anchor, DiffOp, Hunk, SubSpan, SubSpanKind};
use crate::session::{SessionId, TwoWaySide};

use super::common::{AnchorPick, PendingJump, Side};
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

/// Screen-space center of the anchor icon for a given rail row.
/// `rail_left` + `rail_right` are the rail's x-extents on screen.
#[allow(dead_code)]
pub(super) fn anchor_icon_center(
    rail_left: f32,
    rail_right: f32,
    pane_top: f32,
    line: u32,
    scroll_y: f32,
    lh: f32,
) -> [f32; 2] {
    let x = (rail_left + rail_right) * 0.5;
    let y = line_screen_y(pane_top, line, scroll_y, lh) + lh * 0.5;
    [x, y]
}

/// Compute the x offset of a byte position within `line`, clamped to a
/// char boundary, using imgui's font metrics (matches the multiline widget's
/// own hit-testing).
pub(super) fn text_x_at_byte(ui: &Ui, line: &str, byte_offset: usize, padding_x: f32) -> f32 {
    let clamped = byte_offset.min(line.len());
    let mut snap = clamped;
    while snap > 0 && !line.is_char_boundary(snap) {
        snap -= 1;
    }
    padding_x + ui.calc_text_size(&line[..snap])[0]
}

/// Paint EVERYTHING for one pane on the foreground draw list:
/// row backgrounds (change/move), sub-line span highlights,
/// syntax-colored text, and the caret. Also detects hover for the
/// resolution overlay panel.
///
/// Callers must suppress imgui's own text + FrameBg rendering on the
/// `input_text_multiline` widget (via transparent style colors) so this
/// is the only visible layer.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_pane_text(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    highlights: &[LineSpans],
    hunks: &[Hunk],
    side: Side,
    scroll_y: f32,
    scroll_x: f32,
    lh: f32,
    caret_byte: i32,
    widget_active: bool,
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
) {
    let widget_top = widget_rect[1];
    let widget_bottom = widget_rect[3];
    let widget_left = widget_rect[0];
    let widget_right = widget_rect[2];
    let widget_h = widget_bottom - widget_top;
    if widget_h <= 0.0 || lh <= 0.0 {
        return;
    }

    let style = ui.clone_style();
    let padding_x = style.frame_padding[0];
    let padding_y = style.frame_padding[1];

    let first_line = ((scroll_y / lh).floor() as i64).max(0) as u32 + 1;
    let last_line = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    // Map line_no (1-based) on this side -> (OpKind, Option<&Vec<SubSpan>>, move_id).
    // Walk hunks once.
    let row_info = |ln: u32| -> Option<(OpKind, Option<&Vec<SubSpan>>, Option<u32>)> {
        for h in hunks {
            let range = match side {
                Side::Left => h.a_range,
                Side::Right => h.b_range,
            };
            if range == (0, 0) || ln < range.0 || ln > range.1 {
                continue;
            }
            for op in &h.ops {
                match (side, op) {
                    (Side::Left, DiffOp::Equal { a, .. }) if *a == ln => {
                        return Some((OpKind::Equal, None, None));
                    }
                    (Side::Left, DiffOp::Delete { a, move_id, spans, .. }) if *a == ln => {
                        return Some((OpKind::Delete, spans.as_ref(), *move_id));
                    }
                    (Side::Right, DiffOp::Equal { b, .. }) if *b == ln => {
                        return Some((OpKind::Equal, None, None));
                    }
                    (Side::Right, DiffOp::Insert { b, move_id, spans, .. }) if *b == ln => {
                        return Some((OpKind::Insert, spans.as_ref(), *move_id));
                    }
                    _ => continue,
                }
            }
        }
        None
    };

    let dl = ui.get_window_draw_list();
    dl.with_clip_rect(
        [widget_left, widget_top],
        [widget_right, widget_bottom],
        || {
            // Walk lines once; paint bg, sub-line spans, then colored text.
            for (line_idx, line_text) in buf.lines().enumerate() {
                let ln = (line_idx as u32) + 1;
                if ln < first_line || ln > last_line {
                    continue;
                }
                let y = line_screen_y(widget_top, ln, scroll_y, lh) + padding_y;
                if y + lh < widget_top || y > widget_bottom {
                    continue;
                }
                let y0 = y.max(widget_top);
                let y1 = (y + lh).min(widget_bottom);

                // Per-row background.
                let info = row_info(ln);
                if let Some((op_kind, spans_opt, move_id)) = info {
                    let bg = if move_id.is_some() {
                        Some(theme::with_alpha(theme::PEACH(), 0.30))
                    } else {
                        match op_kind {
                            OpKind::Equal => None,
                            OpKind::Delete => Some([0.55, 0.18, 0.18, 0.30]),
                            OpKind::Insert => Some([0.18, 0.50, 0.22, 0.30]),
                        }
                    };
                    if let Some(color) = bg {
                        if y1 > y0 {
                            dl.add_rect([widget_left, y0], [widget_right, y1], color)
                                .filled(true)
                                .build();
                        }
                    }

                    // Sub-line spans (Changed ranges only).
                    if let Some(spans) = spans_opt {
                        let span_color = match op_kind {
                            OpKind::Delete => [0.75, 0.20, 0.20, 0.45],
                            OpKind::Insert => [0.20, 0.65, 0.25, 0.45],
                            OpKind::Equal => [0.0, 0.0, 0.0, 0.0],
                        };
                        if !matches!(op_kind, OpKind::Equal) && y1 > y0 {
                            for sp in spans {
                                if !matches!(sp.kind, SubSpanKind::Changed) {
                                    continue;
                                }
                                if sp.end <= sp.start {
                                    continue;
                                }
                                let x0 = widget_left - scroll_x
                                    + text_x_at_byte(ui, line_text, sp.start as usize, padding_x);
                                let x1 = widget_left - scroll_x
                                    + text_x_at_byte(ui, line_text, sp.end as usize, padding_x);
                                let x0c = x0.max(widget_left).min(widget_right);
                                let x1c = x1.max(widget_left).min(widget_right);
                                if x1c > x0c {
                                    dl.add_rect([x0c, y0], [x1c, y1], span_color)
                                        .filled(true)
                                        .build();
                                }
                            }
                        }
                    }
                }

                // Paint text. If there are highlight spans for this line,
                // walk the line and emit a chunk per span boundary in default
                // color + each span in its color. Otherwise emit the whole
                // line in default color.
                let text_y = y;
                let line_spans_opt = highlights.get(line_idx);
                if let Some(line_spans) = line_spans_opt.filter(|v| !v.is_empty()) {
                    // Walk char-indexed positions.
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
                } else if !line_text.is_empty() {
                    dl.add_text(
                        [widget_left + padding_x - scroll_x, text_y],
                        theme::TEXT(),
                        line_text,
                    );
                }
            }

            // Caret. Blink: ~1s period, on for first half. Only when active.
            if widget_active && caret_byte >= 0 {
                let blink_on = (ui.time() * 2.0).rem_euclid(2.0) < 1.0;
                if blink_on {
                    let target = caret_byte as usize;
                    let mut byte_acc: usize = 0;
                    let mut painted = false;
                    for (line_idx, line_text) in buf.lines().enumerate() {
                        let line_end = byte_acc + line_text.len();
                        if target >= byte_acc && target <= line_end {
                            let local = target - byte_acc;
                            let x = widget_left - scroll_x
                                + text_x_at_byte(ui, line_text, local, padding_x);
                            let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
                            if y + lh >= widget_top && y <= widget_bottom {
                                dl.add_line(
                                    [x, y + 1.0],
                                    [x, y + lh - 1.0],
                                    theme::TEXT(),
                                )
                                .thickness(1.0)
                                .build();
                            }
                            painted = true;
                            break;
                        }
                        byte_acc = line_end + 1; // +1 for '\n'
                    }
                    // Caret past the last newline (trailing empty line).
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
            }
        },
    );

    // Hover detection (outside the clip block — just sets the out cell).
    let mouse_pos = ui.io().mouse_pos;
    let mx = mouse_pos[0];
    let my = mouse_pos[1];
    if mx >= widget_left && mx <= widget_right && my >= widget_top && my <= widget_bottom {
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
                let anchor_y = line_screen_y(widget_top, range.0, scroll_y, lh).max(widget_top);
                hover_out.set(Some((h.id, [widget_right, anchor_y])));
                break;
            }
        }
    }
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
        theme::with_alpha(theme::BLUE(), 0.28)
    } else {
        theme::with_alpha(theme::OVERLAY1(), 0.10)
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
    lh: f32,
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
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, theme::with_alpha(theme::PEACH(), alpha));
        }

        // Anchor curves: thin line from anchor.a row centre on left to
        // anchor.b row centre on right.
        for anc in anchors {
            let ly = left_origin_y + (anc.a as f32 - 1.0) * lh + lh * 0.5;
            let ry = right_origin_y + (anc.b as f32 - 1.0) * lh + lh * 0.5;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST(), 3.0);
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


/// Hover control panel anchored to the top-left of the hovered hunk.
pub(super) fn draw_control_overlay(
    ui: &Ui,
    session_id: SessionId,
    hunk_id: u32,
    pos: [f32; 2],
    lh: f32,
    pending_edits: &mut Vec<DiffEdit>,
    hunks: &[Hunk],
    side: Side,
    pending_jump_out: &Cell<Option<PendingJump>>,
) {
    let hunk = hunks.iter().find(|h| h.id == hunk_id);
    let move_id = hunk.and_then(hunk_move_id);
    let paired = move_id.and_then(|id| find_paired_hunk(hunks, id, side));
    let is_moved_with_pair = paired.is_some();

    // Render the panel as its own top-level imgui window so its buttons
    // sit above the input_text_multiline panes in the window stack and
    // actually receive clicks. (Widgets in the parent window can't win
    // hover/click against the multiline's child window via
    // SetItemAllowOverlap — cross-window overlap needs its own window.)
    //
    // `pos` is the pane's *right* edge for the hovered row; the panel is
    // anchored there with pivot (1, 0) so it grows leftwards and stays
    // inside the pane's right edge regardless of its auto-sized width.
    let panel_x = pos[0] - 4.0;
    let panel_y = pos[1] + 2.0;

    let _pad = ui.push_style_var(StyleVar::FramePadding([6.0, 2.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([4.0, 0.0]));
    let _win_pad = ui.push_style_var(StyleVar::WindowPadding([4.0, 3.0]));
    let _win_round = ui.push_style_var(StyleVar::WindowRounding(4.0));
    let _win_border = ui.push_style_var(StyleVar::WindowBorderSize(1.0));
    let _border_col = ui.push_style_color(imgui::StyleColor::Border, theme::BLUE());
    let _bg_col =
        ui.push_style_color(imgui::StyleColor::WindowBg, theme::with_alpha(theme::MANTLE(), 0.95));

    let _ = lh;
    let win_name = format!("##diff_overlay_{}_{}_{}", session_id, hunk_id, side_tag(side));
    let flags = WindowFlags::NO_TITLE_BAR
        | WindowFlags::NO_RESIZE
        | WindowFlags::NO_MOVE
        | WindowFlags::NO_SCROLLBAR
        | WindowFlags::NO_COLLAPSE
        | WindowFlags::ALWAYS_AUTO_RESIZE
        | WindowFlags::NO_SAVED_SETTINGS
        | WindowFlags::NO_FOCUS_ON_APPEARING
        | WindowFlags::NO_NAV;
    ui.window(&win_name)
        .position([panel_x, panel_y], Condition::Always)
        .position_pivot([1.0, 0.0])
        .flags(flags)
        .build(|| {
            // Icon-only button: an arrow pointing in the direction the
            // hunk will travel (left pane → right pane, or right → left).
            // nf-fa-long-arrow-right / -left read more clearly at small
            // button sizes than the basic arrows.
            let (label, target) = match side {
                Side::Left => (
                    format!("\u{f178}##ov{hunk_id}_atob"),
                    TwoWaySide::B,
                ),
                Side::Right => (
                    format!("\u{f177}##ov{hunk_id}_btoa"),
                    TwoWaySide::A,
                ),
            };
            if ui.small_button(label) {
                pending_edits.push(DiffEdit::ReplaceHunkSide {
                    session_id,
                    hunk_id,
                    target,
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
        });
}

fn side_tag(side: Side) -> &'static str {
    match side {
        Side::Left => "L",
        Side::Right => "R",
    }
}

/// Paint anchor icons on a single rail. Outline icon on the hovered row (if any
/// and unanchored), filled icon for every row that is part of an anchor.
/// `pane_top` is the top of the *pane*, used together with `scroll_y` to map
/// content lines onto screen y.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_anchor_rail(
    ui: &Ui,
    rail_rect: [f32; 4],
    pane_top: f32,
    scroll_y: f32,
    lh: f32,
    side: Side,
    anchors: &[Anchor],
    hovered_line: Option<u32>,
    pick: AnchorPick,
) {
    let dl = ui.get_window_draw_list();
    let rail_left = rail_rect[0];
    let rail_right = rail_rect[2];
    let rail_top = rail_rect[1];
    let rail_bot = rail_rect[3];
    let glyph = "\u{f13d}"; // Nerd Font: anchor

    let filled_color = theme::LAVENDER();
    let outline_color = theme::with_alpha(theme::OVERLAY1(), 0.7);

    let line_for_anchor = |anc: &Anchor| match side {
        Side::Left => anc.a,
        Side::Right => anc.b,
    };

    // Filled icons for every anchored row.
    for anc in anchors {
        let line = line_for_anchor(anc);
        let center = anchor_icon_center(rail_left, rail_right, pane_top, line, scroll_y, lh);
        if center[1] + lh * 0.5 < rail_top || center[1] - lh * 0.5 > rail_bot {
            continue;
        }
        let size = ui.calc_text_size(glyph);
        dl.add_text(
            [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            filled_color,
            glyph,
        );
    }

    // Outline icon for the hovered row, if it isn't already filled.
    if let Some(hl) = hovered_line {
        let already_filled = anchors.iter().any(|a| line_for_anchor(a) == hl);
        if !already_filled {
            let center = anchor_icon_center(rail_left, rail_right, pane_top, hl, scroll_y, lh);
            if center[1] + lh * 0.5 >= rail_top && center[1] - lh * 0.5 <= rail_bot {
                let size = ui.calc_text_size(glyph);
                dl.add_text(
                    [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
                    outline_color,
                    glyph,
                );
            }
        }
    }

    // While Picking on this side, brighten the source icon slightly by re-stamping.
    if let AnchorPick::Picking { side: psd, line } = pick {
        if psd == side {
            let center = anchor_icon_center(rail_left, rail_right, pane_top, line, scroll_y, lh);
            let size = ui.calc_text_size(glyph);
            dl.add_text(
                [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
                theme::SAPPHIRE(),
                glyph,
            );
        }
    }
}

/// Paint per-row line numbers + anchor dots in the gutter strip.
pub(super) fn paint_gutter(
    ui: &Ui,
    gutter_rect: [f32; 4],
    _anchors: &[Anchor],
    _side: Side,
    scroll_y: f32,
    lh: f32,
    line_count: u32,
) {
    let dl = ui.get_window_draw_list();
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

    let line_no_color = theme::OVERLAY1();
    for line in first_line..=last_line.min(line_count) {
        let y = line_screen_y(g_top, line, scroll_y, lh);
        if y + lh < g_top || y > g_bottom {
            continue;
        }
        let text = format!("{line}");
        let text_w = ui.calc_text_size(&text)[0];
        dl.add_text([g_left + g_w - 4.0 - text_w, y + 2.0], line_no_color, &text);
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
