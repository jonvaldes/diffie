//! 2-way diff view — drawing/painting code.
//!
//! Connector ribbon, per-pane row rendering, hover control overlay, the
//! per-row paint+input widget (`draw_row` kept whole — see mod.rs note on
//! the split-seam decision), syntax-colored text paint, and the two-way
//! scroll-sync echo dampener (`sync_scrolls`).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use imgui::{FontId, ListClipper, StyleVar, Ui};

use super::super::char_diff::Segment;
use super::super::syntax::LineSpans;
use super::super::theme;
use super::super::undo_stack::DiffEdit;
use super::input::{compute_enter_split, compute_paste_split};
use crate::diff::{Anchor, Hunk};
use crate::session::{SessionId, TwoWaySide};

use super::common::{
    fill_bezier_ribbon, find_paired_hunk, gutter_w, hunk_move_id, is_change_hunk, move_color,
    move_ribbon_alpha, ordered_endpoints, ribbon_color, row_h, stroke_bezier_curve, target_scroll,
    text_x_at_byte, double_click_word_bounds, Cls, DiffViewState, MoveFlash, PendingJump, Row,
    Selection, Side, MOVE_FLASH_FRAMES, MOVE_FLASH_PEAK_ALPHA,
};

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
    left_line_ys: &HashMap<u32, f32>,
    right_line_ys: &HashMap<u32, f32>,
    anchors: &[Anchor],
    hunks: &[Hunk],
) {
    let dl = ui.get_window_draw_list();
    dl.with_clip_rect_intersect(origin, [origin[0] + w, origin[1] + h], || {
        let x_l = origin[0];
        let x_r = origin[0] + w;
        let band_top = origin[1];
        let band_bot = origin[1] + h;

        for h_obj in hunks {
            let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            // left/right_origin_y already account for scroll (captured via
            // cursor_screen_pos inside the scrolling pane), so content-y
            // maps directly to screen-y by addition.
            let a1 = left_origin_y + lr.1;
            let a2 = left_origin_y + lr.2;
            let b1 = right_origin_y + rr.1;
            let b2 = right_origin_y + rr.2;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            let color = ribbon_color(is_change_hunk(h_obj));
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, color);
        }

        // Moved-hunk ribbons. Pair Delete-only and Insert-only hunks sharing
        // a move_id; paint each pair as a distance-faded peach ribbon.
        for h_obj in hunks {
            // Only iterate Delete-only moved hunks to avoid double-painting.
            let Some(move_id) = hunk_move_id(h_obj) else { continue };
            let is_delete_only = h_obj.b_range == (0, 0);
            if !is_delete_only { continue; }
            let Some(paired) = find_paired_hunk(hunks, move_id, Side::Left) else { continue };
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
            let dy = mid_a - mid_b;
            let alpha = move_ribbon_alpha(dy);
            let color = theme::with_alpha(move_color(), alpha);
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, color);
        }

        for anc in anchors {
            let Some(ly_content) = left_line_ys.get(&anc.a) else {
                continue;
            };
            let Some(ry_content) = right_line_ys.get(&anc.b) else {
                continue;
            };
            let ly = left_origin_y + ly_content + row_h() * 0.5;
            let ry = right_origin_y + ry_content + row_h() * 0.5;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST, 3.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_pane(
    ui: &Ui,
    rows: &[Row],
    side: Side,
    session_id: SessionId,
    anchored: &HashSet<u32>,
    click_out: &Cell<Option<u32>>,
    mono_font: Option<FontId>,
    selection: Option<&Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    line_remove: &Cell<Option<DiffEdit>>,
    pending_edits: &mut Vec<DiffEdit>,
    arrow_focus: &Cell<Option<(Side, u32, usize)>>,
    caret_blink_reset: &Cell<f64>,
    input_epoch: u32,
    drag_active: Option<(Side, bool)>,
    char_w_out: &Cell<f32>,
    highlights: &[LineSpans],
    content_w: f32,
    active_selection_out: &Cell<Option<(Side, u32, usize, usize)>>,
    shift_arrow_out: &Cell<Option<(Side, u32, usize, u32)>>,
    clear_state_selection_out: &Cell<bool>,
    pin_scroll_x_request_out: &Cell<Option<(Side, f32)>>,
    caret_offset_out: &Cell<Option<(Side, f32)>>,
    hunks: &[Hunk],
    pending_jump_out: &Cell<Option<PendingJump>>,
    flash: Option<MoveFlash>,
) {
    let total = rows.len() as i32;
    if total == 0 {
        return;
    }

    // Captured before the clipper / rows render so we have an accurate
    // pane origin (post-scroll screen y of content_y=0) for auto-scroll.
    let pane_origin = ui.cursor_screen_pos();
    let visible_h = ui.content_region_avail()[1];
    let cur_scroll = ui.scroll_y();

    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([0.0, 0.0]));
    let hover: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let mut clipper = ListClipper::new(total).items_height(row_h()).begin(ui);
    while clipper.step() {
        for i in clipper.display_start()..clipper.display_end() {
            let r = &rows[i as usize];
            let line_hl = r
                .line_no
                .and_then(|ln| highlights.get((ln as usize).saturating_sub(1)))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            if let Some(clicked_line) = draw_row(
                ui,
                r,
                side,
                i,
                session_id,
                anchored,
                mono_font,
                &hover,
                selection,
                focus_event,
                line_remove,
                pending_edits,
                arrow_focus,
                caret_blink_reset,
                input_epoch,
                drag_active,
                char_w_out,
                line_hl,
                content_w,
                active_selection_out,
                shift_arrow_out,
                clear_state_selection_out,
                pin_scroll_x_request_out,
                caret_offset_out,
                flash,
            ) {
                click_out.set(Some(clicked_line));
            }
        }
    }
    drop(_spacing);

    // Drag auto-scroll: while a drag is live on this side and the mouse is
    // past the pane's visible band, scroll proportionally. The selection
    // caret advances on its own via `update_selection`, which clamps the
    // mouse to the visible band and re-computes the caret each frame.
    if drag_active.map(|(s, _)| s) == Some(side) && ui.is_mouse_down(imgui::MouseButton::Left) {
        let mouse_y = ui.io().mouse_pos[1];
        let pane_top = pane_origin[1] + cur_scroll;
        let pane_bot = pane_top + visible_h;
        let max_scroll = (rows.len() as f32 * row_h() - visible_h).max(0.0);
        let new_scroll = if mouse_y < pane_top {
            let dist = (pane_top - mouse_y).min(160.0);
            let speed = 8.0 + dist * 0.5;
            Some((cur_scroll - speed).max(0.0))
        } else if mouse_y > pane_bot {
            let dist = (mouse_y - pane_bot).min(160.0);
            let speed = 8.0 + dist * 0.5;
            Some((cur_scroll + speed).min(max_scroll))
        } else {
            None
        };
        if let Some(s) = new_scroll {
            ui.set_scroll_y(s);
        }
    }

    if let Some((hunk_id, pos)) = hover.get() {
        draw_control_overlay(
            ui,
            session_id,
            hunk_id,
            pos,
            pending_edits,
            hunks,
            side,
            pending_jump_out,
        );
    }
}

/// Floating panel with the four decision buttons, rendered on top of the
/// hovered row. Takes no space in the row layout because it sets the cursor
/// to an absolute screen position and we ignore the cursor advance afterwards.
fn draw_control_overlay(
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
    let panel_h = row_h() - 4.0;

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
    // 2-way edit mode: these copy this hunk's content from one side to the
    // other. Queued onto the undo stack so the operation is reversible.
    if ui.small_button(format!("Apply A → B##ov{hunk_id}_atob")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::B,
            old_target_lines: None,
        });
    }
    ui.same_line();
    if ui.small_button(format!("B → A##ov{hunk_id}_btoa")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::A,
            old_target_lines: None,
        });
    }
    if is_moved_with_pair {
        ui.same_line();
        if ui.small_button(format!("↕##ov{hunk_id}_jump")) {
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
        if let Some(p) = paired {
            if ui.is_item_hovered() {
                let n = match side {
                    Side::Left => p.b_range.0,
                    Side::Right => p.a_range.0,
                };
                ui.tooltip_text(format!("Jump to paired half (line {n})"));
            }
        }
    }
}

/// Render a single row. The text area is an always-live `input_text` so
/// Paint a row's text via the draw list, picking foreground colors from the
/// startup-computed `ColorTable` for the right background. Char positions
/// covered by a `seg.hl=true` segment on a Delete/Insert row use the
/// red/green table; everything else uses the normal table.
///
/// Adjacent chars that resolve to the same color are coalesced into a single
/// `add_text` call so unchanged stretches don't pay per-char overhead.
fn paint_row_text(
    ui: &Ui,
    dl: &imgui::DrawListMut<'_>,
    segments: &[Segment],
    buf: &str,
    origin: [f32; 2],
    spans: &[super::super::syntax::LineSpan],
    row_cls: Cls,
) {
    // Iterate buf by char with byte positions so chunk x can be
    // computed as `text_x_at_byte(buf, byte_at_chunk_start)`.
    let chars_with_bytes: Vec<(usize, char)> = buf.char_indices().collect();
    let n = chars_with_bytes.len();
    if n == 0 {
        return;
    }

    // Parallel hl mask, indexed by char position (matching
    // chars_with_bytes). Built from segments in the same order they
    // compose into buf.
    let mut hl_mask: Vec<bool> = Vec::with_capacity(n);
    for seg in segments {
        for _ in seg.text.chars() {
            hl_mask.push(seg.hl);
        }
    }
    debug_assert_eq!(hl_mask.len(), n);

    // Per-char syntax kind, filled from spans (non-overlapping, sorted).
    let mut kind_at: Vec<Option<super::super::syntax::SyntaxKind>> = vec![None; n];
    for s in spans {
        let start = s.start_col.min(n);
        let end = s.end_col.min(n).max(start);
        for c in start..end {
            kind_at[c] = Some(s.kind);
        }
    }

    let table_at = |c: usize| -> &'static super::super::syntax::ColorTable {
        let bg = match (row_cls, hl_mask[c]) {
            (Cls::Equal, _) => super::super::syntax::HlBg::None,
            (Cls::Delete, false) => super::super::syntax::HlBg::DeleteRow,
            (Cls::Delete, true) => super::super::syntax::HlBg::DeleteHl,
            (Cls::Insert, false) => super::super::syntax::HlBg::InsertRow,
            (Cls::Insert, true) => super::super::syntax::HlBg::InsertHl,
        };
        super::super::syntax::table_for(bg)
    };

    let pick = |c: usize| -> [f32; 4] { table_at(c).get(kind_at[c]) };

    // Coalesce contiguous same-color runs. Each chunk's position is
    // `origin + text_x_at_byte(buf, byte_at_chunk_start)` — the same
    // canonical formula caret / hl rect / drag-selection use.
    let mut run_start = 0usize;
    let mut run_color = pick(0);
    for c in 1..n {
        let color = pick(c);
        if color != run_color {
            let chunk: String = chars_with_bytes[run_start..c]
                .iter()
                .map(|(_, ch)| *ch)
                .collect();
            let chunk_byte = chars_with_bytes[run_start].0;
            let pos = [origin[0] + text_x_at_byte(ui, buf, chunk_byte), origin[1]];
            dl.add_text(pos, run_color, &chunk);
            run_start = c;
            run_color = color;
        }
    }
    let chunk: String = chars_with_bytes[run_start..]
        .iter()
        .map(|(_, ch)| *ch)
        .collect();
    let chunk_byte = chars_with_bytes[run_start].0;
    let pos = [origin[0] + text_x_at_byte(ui, buf, chunk_byte), origin[1]];
    dl.add_text(pos, run_color, &chunk);
}

/// clicks place the caret directly and every keystroke commits — the diff
/// re-runs every frame the buffer changes. Mouse-driven selection transitions
/// live in `update_selection`; this function is read-only with respect to
/// `selection`. Returns `Some(line_no)` if the row was right-clicked this
/// frame (anchor pick).
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    row: &Row,
    side: Side,
    idx: i32,
    session_id: SessionId,
    anchored: &HashSet<u32>,
    mono_font: Option<FontId>,
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
    selection: Option<&Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    line_remove: &Cell<Option<DiffEdit>>,
    pending_edits: &mut Vec<DiffEdit>,
    arrow_focus: &Cell<Option<(Side, u32, usize)>>,
    caret_blink_reset: &Cell<f64>,
    input_epoch: u32,
    drag_active: Option<(Side, bool)>,
    char_w_out: &Cell<f32>,
    line_hl: &[super::super::syntax::LineSpan],
    content_w: f32,
    active_selection_out: &Cell<Option<(Side, u32, usize, usize)>>,
    shift_arrow_out: &Cell<Option<(Side, u32, usize, u32)>>,
    clear_state_selection_out: &Cell<bool>,
    pin_scroll_x_request_out: &Cell<Option<(Side, f32)>>,
    caret_offset_out: &Cell<Option<(Side, f32)>>,
    flash: Option<MoveFlash>,
) -> Option<u32> {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + row_h()];

    // Gutter rect — used only for RMB anchor picking. LMB on the gutter is
    // handled by the central click handler like any other in-pane click.
    let gutter_p1 = [p0[0] + gutter_w(), p1[1]];
    let gutter_hovered = ui.is_mouse_hovering_rect(p0, gutter_p1);
    let rmb_anchor = gutter_hovered && ui.is_mouse_clicked(imgui::MouseButton::Right);

    // Positional hover for the full row, independent of any active widget.
    let mouse_in_row = ui.is_mouse_hovering_rect(p0, p1);
    if mouse_in_row && row.is_change {
        let pane_origin_y = p0[1] - (idx as f32) * row_h();
        let pane_visible_top = pane_origin_y + ui.scroll_y();
        let first_row_y = pane_origin_y + (row.hunk_first_row as f32) * row_h();
        let anchor_y = first_row_y.max(pane_visible_top);
        hover_out.set(Some((row.hunk_id, [p0[0], anchor_y])));
    }

    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let char_w = ui.calc_text_size("m")[0].max(1.0);
    char_w_out.set(char_w);
    let text_start_x = p0[0] + gutter_w();
    let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();

    let dl = ui.get_window_draw_list();

    // Single source of truth for "this row's text" — used by the
    // drag-selection rect, per-char highlight rects, syntax text
    // rendering, the manual caret, and the input_text widget below.
    // Everything that needs an x position computes it as
    // `text_x_at_byte(ui, &buf, byte_in_buf)`.
    let buf: String = row.segments.iter().map(|s| s.text.as_str()).collect();

    // ---- backgrounds: hunk color → hover tint → selection ----
    // Moved rows replace the standard red/green tint with the move
    // color (peach @ 0.30 alpha). Equal rows are never moved.
    let bg = if row.moved {
        Some(theme::with_alpha(move_color(), 0.30))
    } else {
        match row.cls {
            Cls::Equal => None,
            Cls::Delete => Some([0.55, 0.18, 0.18, 0.30]),
            Cls::Insert => Some([0.18, 0.50, 0.22, 0.30]),
        }
    };
    if let Some(bg_rgba) = bg {
        dl.add_rect(p0, p1, bg_rgba).filled(true).build();
    }
    if mouse_in_row {
        dl.add_rect(p0, p1, theme::with_alpha(theme::TEXT, 0.04))
            .filled(true)
            .build();
    }
    if let Some(f) = flash {
        if f.session_id == session_id && f.hunk_id == row.hunk_id {
            let alpha = MOVE_FLASH_PEAK_ALPHA
                * (f.frames_remaining as f32 / MOVE_FLASH_FRAMES as f32);
            let color = theme::with_alpha(move_color(), alpha);
            dl.add_rect(p0, p1, color).filled(true).build();
        }
    }
    if let (Some(sel), Some(ln)) = (selection, row.line_no) {
        if sel.side == side {
            let (lo, hi) = ordered_endpoints(sel);
            if ln >= lo.line_no && ln <= hi.line_no {
                let l_col = if ln == lo.line_no { lo.col } else { 0 };
                let r_col = if ln == hi.line_no { hi.col } else { char_count };
                let l_col = l_col.min(char_count);
                let r_col = r_col.min(char_count);
                if r_col > l_col {
                    let l_byte = buf
                        .char_indices()
                        .nth(l_col)
                        .map(|(b, _)| b)
                        .unwrap_or(buf.len());
                    let r_byte = buf
                        .char_indices()
                        .nth(r_col)
                        .map(|(b, _)| b)
                        .unwrap_or(buf.len());
                    let sel_x0 = text_start_x + text_x_at_byte(ui, &buf, l_byte);
                    let sel_x1 = text_start_x + text_x_at_byte(ui, &buf, r_byte);
                    dl.add_rect(
                        [sel_x0, p0[1]],
                        [sel_x1, p1[1]],
                        theme::with_alpha(theme::BLUE, 0.40),
                    )
                    .filled(true)
                    .build();
                }
            }
        }
    }
    let _ = focus_event;

    // ---- char-level highlight rects (red/green tint under changed chars) ----
    let hl_bg = match row.cls {
        Cls::Delete => [0.85, 0.18, 0.18, 0.20],
        Cls::Insert => [0.18, 0.70, 0.30, 0.20],
        Cls::Equal => [0.0, 0.0, 0.0, 0.0],
    };
    {
        let mut seg_byte = 0usize;
        for seg in &row.segments {
            if seg.text.is_empty() {
                continue;
            }
            let seg_byte_end = seg_byte + seg.text.len();
            if seg.hl {
                let x_start = text_start_x + text_x_at_byte(ui, &buf, seg_byte);
                let x_end = text_start_x + text_x_at_byte(ui, &buf, seg_byte_end);
                dl.add_rect(
                    [x_start, p0[1] + 2.0],
                    [x_end, p0[1] + row_h() - 2.0],
                    hl_bg,
                )
                .filled(true)
                .build();
            }
            seg_byte = seg_byte_end;
        }
    }

    // ---- gutter line number ----
    let line_text = match row.line_no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    };
    dl.add_text([p0[0] + 6.0, p0[1] + 3.0], theme::OVERLAY1, &line_text);

    // ---- anchored row marker ----
    if let Some(ln) = row.line_no {
        if anchored.contains(&ln) {
            dl.add_rect(p0, [p0[0] + 3.0, p1[1]], theme::LAVENDER)
                .filled(true)
                .build();
        }
    }

    // ---- syntax-colored text rendering ----
    //
    // We paint the row text directly via the draw list (before input_text
    // builds) so per-token colors land. The `input_text` widget that follows
    // gets its Text style color set to transparent — it still owns the
    // caret, selection-bg, and keyboard input, but doesn't draw its own
    // (un-highlighted) copy on top of ours.
    //
    // Syntax spans apply on every row regardless of hunk class; the red/green
    // *background* tints (row bg + per-char hl rects) continue to mark
    // Delete/Insert visually, but the text itself stays readable in
    // palette colors.
    let mut buf = buf;
    let was_empty = buf.is_empty();
    paint_row_text(
        ui,
        &dl,
        &row.segments,
        &buf,
        [text_start_x, p0[1] + 3.0],
        line_hl,
        row.cls,
    );
    let _frame_bg = ui.push_style_color(imgui::StyleColor::FrameBg, [0.0, 0.0, 0.0, 0.0]);
    let _frame_bg_hov = ui.push_style_color(imgui::StyleColor::FrameBgHovered, [0.0, 0.0, 0.0, 0.0]);
    let _frame_bg_act = ui.push_style_color(imgui::StyleColor::FrameBgActive, [0.0, 0.0, 0.0, 0.0]);
    // Transparent text so input_text doesn't double-draw on top of the
    // colored spans we just painted via `paint_row_text`.
    let _text_color = ui.push_style_color(imgui::StyleColor::Text, [0.0, 0.0, 0.0, 0.0]);
    let _pad = ui.push_style_var(StyleVar::FramePadding([2.0, 2.0]));
    let _border = ui.push_style_var(StyleVar::FrameBorderSize(0.0));
    ui.set_cursor_screen_pos([text_start_x, p0[1]]);
    // Match the parent window's content width so the input_text spans the
    // whole row; otherwise imgui's input_text would horizontally scroll its
    // *own* contents on long lines, fighting the parent's scroll position.
    ui.set_next_item_width((content_w - gutter_w()).max(1.0));
    let input_id = match row.line_no {
        Some(n) => format!("##rowedit_{:?}_{n}_e{input_epoch}", side),
        None => format!("##rowedit_{:?}_idx_{idx}_e{input_epoch}", side),
    };
    // If a previous frame's Up/Down arrow asked us to focus this row, claim
    // keyboard focus right before the input_text builds. imgui routes
    // SetKeyboardFocusHere through its nav-tabbing system, which actually
    // activates the widget on the *next* frame — so we keep the request
    // alive until `is_item_activated` confirms the input took focus, and
    // the callback below clears the imgui-inserted select-all whenever the
    // request is still live for this row.
    let arrow_match_target: Option<usize> = match (arrow_focus.get(), row.line_no) {
        (Some((req_side, req_ln, tcol)), Some(ln)) if req_side == side && req_ln == ln => {
            Some(tcol)
        }
        _ => None,
    };
    let arrow_match = arrow_match_target.is_some();
    if arrow_match {
        ui.set_keyboard_focus_here();
    }
    // Convert the requested target column (chars) into a byte offset within
    // this row's buffer, clamped to its length. -1 means "don't seed".
    let seed_byte: i32 = match arrow_match_target {
        Some(tcol) => {
            let take = tcol.min(buf.chars().count());
            buf.chars().take(take).map(|c| c.len_utf8()).sum::<usize>() as i32
        }
        None => -1,
    };
    // Detect a double-click that lands inside this row's input_text and
    // pre-compute the desired byte range. ImGui's native double-click
    // selects from the previous space to the next space — too greedy for
    // punctuation. Our override narrows to the standard text-editor
    // word-class run; the CaretCapture callback applies it after imgui's
    // word-select has run.
    let dbl_click_override: Option<(usize, usize)> = if ui
        .is_mouse_double_clicked(imgui::MouseButton::Left)
    {
        let click_pos = ui.io().mouse_pos;
        let widget_x0 = text_start_x;
        let widget_x1 = text_start_x + (content_w - gutter_w()).max(1.0);
        if click_pos[0] >= widget_x0
            && click_pos[0] < widget_x1
            && click_pos[1] >= p0[1]
            && click_pos[1] < p1[1]
        {
            let raw_col = ((click_pos[0] - widget_x0) / char_w).floor().max(0.0);
            let char_col = raw_col as usize;
            let byte_idx = buf
                .char_indices()
                .nth(char_col)
                .map(|(b, _)| b)
                .unwrap_or(buf.len());
            Some(double_click_word_bounds(&buf, byte_idx))
        } else {
            None
        }
    } else {
        None
    };
    // Capture imgui's internal cursor position via the ALWAYS callback so we
    // can paint the caret ourselves below — imgui's own caret uses the Text
    // color, which we forced to transparent to avoid double-drawing. We also
    // use the callback to suppress the select-all that imgui does on the
    // first frame after `SetKeyboardFocusHere`, which otherwise highlights
    // the whole row when arrow keys jump between lines, and to seed the
    // caret at the column the user had on the previous line.
    // While a cross-row drag selection is live on this side, the row where
    // mouse-down landed will *also* drag-select its imgui input_text
    // contents — that's the extra horizontal highlight tracking the
    // pointer. Suppress it by collapsing imgui's selection to the cursor
    // every frame the drag is live on our side.
    // Only suppress imgui's native input_text selection once our drag has
    // crossed the movement threshold. Pre-threshold we let imgui's
    // selection survive so double-click word-select (which sets a multi-
    // char selection that our state.selection doesn't track) doesn't get
    // immediately collapsed by this callback.
    let drag_on_this_side_past_threshold = drag_active
        .map(|(s, past)| s == side && past)
        .unwrap_or(false);
    // Also suppress when our cross-row `state.selection` exists on this
    // side and spans multiple rows. Without this, Ctrl+C would route
    // through the focused row's `input_text` widget — which only knows
    // about a single-line slice of our selection — and overwrite the
    // multi-line text we wrote to the clipboard. Single-row selections
    // (collapsed click point, same-row drag) leave imgui's selection
    // alone so double-click word-select still works.
    let multi_row_selection_on_this_side = selection.map_or(false, |s| {
        s.side == side && s.anchor.line_no != s.caret.line_no
    });
    let caret_pos: Cell<i32> = Cell::new(-1);
    // Filled after the callback with imgui's post-mutation selection bounds
    // (start_byte, end_byte). Read after `build()` so we know whether
    // imgui's input_text ended up with a selection this frame.
    let caret_selection: Cell<Option<(usize, usize)>> = Cell::new(None);
    struct CaretCapture<'a> {
        out: &'a Cell<i32>,
        selection_out: &'a Cell<Option<(usize, usize)>>,
        clear_selection: bool,
        seed_byte: i32,
        suppress_imgui_selection: bool,
        dbl_click_override: Option<(usize, usize)>,
    }
    impl<'a> imgui::InputTextCallbackHandler for CaretCapture<'a> {
        fn on_always(&mut self, mut data: imgui::TextCallbackData) {
            if self.clear_selection {
                if self.seed_byte >= 0 {
                    data.set_cursor_pos(self.seed_byte as usize);
                }
                let pos = data.cursor_pos() as i32;
                *data.selection_start_mut() = pos;
                *data.selection_end_mut() = pos;
            } else if self.suppress_imgui_selection {
                let pos = data.cursor_pos() as i32;
                *data.selection_start_mut() = pos;
                *data.selection_end_mut() = pos;
            } else if let Some((s, e)) = self.dbl_click_override {
                // Replace imgui's overly-greedy word selection.
                data.set_cursor_pos(e);
                *data.selection_start_mut() = s as i32;
                *data.selection_end_mut() = e as i32;
            }
            self.out.set(data.cursor_pos() as i32);
            // Capture the post-mutation selection so tests can observe
            // double-click word-select and similar behaviors.
            let sel = data.selection();
            self.selection_out.set(Some((sel.start, sel.end)));
        }
    }
    // Capture clipboard text BEFORE the widget builds. Imgui's
    // input_text is single-line, so Ctrl+V strips newlines from the
    // clipboard and inserts the rest. If the user pasted multi-line
    // text we'll detect that after the build and emit a multi-line
    // splice ourselves instead of letting the stripped insert stand.
    let ctrl_v_active = ui.io().key_ctrl && ui.is_key_pressed(imgui::Key::V);
    let pending_paste: Option<String> = if ctrl_v_active {
        ui.clipboard_text().filter(|s| s.contains('\n'))
    } else {
        None
    };
    let changed = ui
        .input_text(input_id, &mut buf)
        // imgui's input_text has its own per-char undo stack on Ctrl+Z. If
        // it ran alongside our app-level stack, a Ctrl+Z that pops a
        // selection-driven `Splice` would *also* make imgui re-insert a
        // char in the focused row, the input fires `changed`, we push a
        // stale `SetTwoWayLine`, and `record.edit` truncates the redo
        // history — the Splice is gone for good. We own undo at the diff
        // level, so disable imgui's.
        .no_undo_redo(true)
        .callback(
            imgui::InputTextCallback::ALWAYS,
            CaretCapture {
                out: &caret_pos,
                selection_out: &caret_selection,
                clear_selection: arrow_match,
                seed_byte,
                suppress_imgui_selection: drag_on_this_side_past_threshold
                    || multi_row_selection_on_this_side,
                dbl_click_override,
            },
        )
        .build();
    let input_active = ui.is_item_active();
    // Export the active row's imgui selection (post-callback) so render's
    // caller — currently just headless tests — can observe behaviors like
    // double-click word-select. Last-active-row wins if multiple are
    // active in a frame, which doesn't happen in practice.
    if input_active {
        if let (Some(ln), Some((s, e))) = (row.line_no, caret_selection.get()) {
            if s != e {
                active_selection_out.set(Some((side, ln, s, e)));
            }
        }
    }
    // The arrow-focus request is satisfied once the input actually becomes
    // active — `is_item_activated` is true only on that single frame, after
    // which we drop the request so a normal click can select-all again.
    if ui.is_item_activated() {
        caret_blink_reset.set(ui.time());
        if arrow_match {
            arrow_focus.set(None);
        }
    }
    // Up/Down inside an active row: hand keyboard focus to the adjacent
    // source-line row on the same side. We snapshot the current caret column
    // (chars, converted from imgui's byte offset using the row's buffer) so
    // the target row can drop the caret at the same column instead of at the
    // end of the line.
    if input_active {
        if let Some(ln) = row.line_no {
            let up = ui.is_key_pressed(imgui::Key::UpArrow) && ln > 1;
            let down = ui.is_key_pressed(imgui::Key::DownArrow);
            let left = ui.is_key_pressed(imgui::Key::LeftArrow);
            let right = ui.is_key_pressed(imgui::Key::RightArrow);
            let shift = ui.io().key_shift;
            // Lateral motion within the row (Left/Right) is handled by
            // imgui's input_text internally, so `is_item_activated`
            // doesn't fire — we need an explicit blink-reset here so the
            // caret is on for the first half-cycle after the move.
            if left || right {
                caret_blink_reset.set(ui.time());
            }
            // Pin scroll_x to neutralize imgui's nav-scroll-induced
            // gutter drift:
            //   - Up/Down: arrow_focus → set_keyboard_focus_here on the
            //     adjacent row's widget snaps to gutter_w. Pin to the
            //     pre-key scroll_x (current `ui.scroll_x()`).
            //   - Left/Right: imgui's input_text doesn't manage the
            //     parent's scroll, so we compute the target ourselves
            //     to keep the new caret column visible. This both
            //     neutralizes the gutter snap AND provides cursor-
            //     follow scroll when the caret would otherwise go past
            //     the viewport edge.
            if up || down {
                pin_scroll_x_request_out.set(Some((side, ui.scroll_x())));
            } else if left || right {
                let cur_byte = caret_pos.get().max(0) as usize;
                let take = cur_byte.min(buf.len());
                let mut snap = take;
                while snap > 0 && !buf.is_char_boundary(snap) {
                    snap -= 1;
                }
                let cur_scroll = ui.scroll_x();
                // True visible content width. `window_size()` includes
                // WindowPadding (~8 px each side), and `content_region`
                // returns the explicit content size (3076 px) rather
                // than the visible width because we set `content_size`
                // on the child window. So compute it manually:
                //   visible = window_size - 2 * WindowPadding.x.
                // Without this the cursor goes ~1 char past the right
                // edge before the scroll-follow triggers.
                let style_pad_x = unsafe { ui.style() }.window_padding[0];
                let viewport_w = (ui.window_size()[0] - 2.0 * style_pad_x).max(1.0);
                // Use calc_text_size so the scroll target matches the
                // actual rendered position of the new caret (works for
                // proportional fonts too).
                let cursor_content_x = gutter_w() + text_x_at_byte(ui, &buf, snap);
                // Two-character margin: scroll when the cursor gets
                // within 2 chars of either edge, and land it 2 chars
                // from the edge after the scroll. Standard "soft edge"
                // behavior so the user has visual context around the
                // caret instead of it pressing against the viewport wall.
                let pad = 2.0 * char_w;
                let target = if cursor_content_x < cur_scroll + pad {
                    (cursor_content_x - pad).max(0.0)
                } else if cursor_content_x > cur_scroll + viewport_w - pad {
                    cursor_content_x - viewport_w + pad
                } else {
                    cur_scroll
                };
                pin_scroll_x_request_out.set(Some((side, target)));
            }
            if up || down {
                let cur_byte = caret_pos.get().max(0) as usize;
                let take = cur_byte.min(buf.len());
                let cur_col = buf
                    .get(..take)
                    .map(|s| s.chars().count())
                    .unwrap_or_else(|| buf.chars().count());
                let new_ln = if up { ln - 1 } else { ln + 1 };
                arrow_focus.set(Some((side, new_ln, cur_col)));
                if shift {
                    shift_arrow_out.set(Some((side, ln, cur_col, new_ln)));
                }
            }
            // Plain arrow navigation (any direction, no shift) collapses
            // the cross-row selection. The caret continues moving via
            // imgui's own handling (Left/Right within the row) or our
            // arrow_focus (Up/Down between rows).
            if !shift && (up || down || left || right) {
                clear_state_selection_out.set(true);
            }
        }
    }
    drop(_pad);
    drop(_border);
    drop(_text_color);
    drop(_frame_bg_act);
    drop(_frame_bg_hov);
    drop(_frame_bg);

    // Manual caret: imgui draws its own caret with `ImGuiCol_Text`, which we
    // forced to transparent so it wouldn't overpaint the syntax-colored
    // spans. We replay the caret here at the position the callback reported,
    // blinking on a ~1s cycle to roughly match imgui's default.
    if input_active && caret_pos.get() >= 0 {
        let byte_pos = caret_pos.get().max(0) as usize;
        let caret_offset = text_x_at_byte(ui, &buf, byte_pos);
        // Phase the blink off the most recent activation so the caret is on
        // for the first half-cycle after a line jump or click.
        let since = (ui.time() - caret_blink_reset.get()).max(0.0);
        let blink_on = (since % 1.06) < 0.53;
        if blink_on {
            let cx = text_start_x + caret_offset;
            let cy0 = p0[1] + 2.0;
            let cy1 = p0[1] + row_h() - 2.0;
            dl.add_line([cx, cy0], [cx, cy1], theme::TEXT)
                .thickness(1.0)
                .build();
        }
        // Expose the caret's x offset within the text area so tests can
        // verify it tracks the rendered characters. The offset is the
        // caret's distance from `text_start_x`; for ASCII text this
        // equals `byte_pos * char_w`, but for UTF-8 it equals
        // `char_col * char_w` (the correct value).
        caret_offset_out.set(Some((side, caret_offset)));
    }

    // Enter / multi-line paste: imgui's input_text is single-line, so
    // Enter is a no-op and Ctrl+V strips newlines. Detect both and
    // emit a `SpliceTwoWayLines` that produces the correct multi-line
    // result. The standard `SetTwoWayLine` emit below is skipped when
    // a splice fires.
    // Enter on a single-line `input_text` deactivates the widget the
    // same frame it's pressed, so `input_active` is already false here.
    // `is_item_deactivated()` catches the just-deactivated row.
    let just_deactivated = ui.is_item_deactivated();
    let enter_pressed = ui.is_key_pressed(imgui::Key::Enter)
        && (input_active || just_deactivated);
    // Only the row whose input_text actually received the key event
    // emits a splice; otherwise BOTH panes' rows for this line would
    // compete for `line_remove` and the wrong-side splice could win.
    let paste_target = input_active.then_some(pending_paste).flatten();
    let mut emit_splice: Option<Vec<String>> = None;
    if enter_pressed {
        // Imgui ignored Enter — `buf` and `caret_pos` are unchanged.
        let caret_byte = (caret_pos.get().max(0) as usize).min(buf.len());
        let caret_char = buf[..caret_byte].chars().count();
        emit_splice = Some(compute_enter_split(&buf, caret_char));
    } else if let Some(paste) = paste_target {
        // Imgui already inserted the newline-stripped paste at the caret.
        // Reconstruct the original (pre-paste) line by removing the
        // inserted slice, then re-split with the un-stripped clipboard.
        let caret_byte = (caret_pos.get().max(0) as usize).min(buf.len());
        let caret_char_end = buf[..caret_byte].chars().count();
        let stripped_chars = paste.chars().filter(|c| *c != '\n').count();
        let paste_start_char = caret_char_end.saturating_sub(stripped_chars);
        let prefix: String = buf.chars().take(paste_start_char).collect();
        let suffix: String = buf.chars().skip(caret_char_end).collect();
        let original = format!("{prefix}{suffix}");
        emit_splice = Some(compute_paste_split(&original, paste_start_char, &paste));
    }
    if let (Some(replacement), Some(ln)) = (emit_splice, row.line_no) {
        let two_way_side = match side {
            Side::Left => TwoWaySide::A,
            Side::Right => TwoWaySide::B,
        };
        let line_idx = (ln as usize).saturating_sub(1);
        line_remove.set(Some(DiffEdit::SpliceTwoWayLines {
            session_id,
            side: two_way_side,
            start: line_idx,
            end: line_idx + 1,
            replacement,
            old_target_lines: None,
        }));
    // Live commit: any change pushes a `SetTwoWayLine` onto the undo stack,
    // and the next frame's diff reflects it. Equivalent edits on the same
    // line coalesce via `DiffEdit::merge` so the undo stack stays compact.
    } else if changed {
        if let Some(ln) = row.line_no {
            let two_way_side = match side {
                Side::Left => TwoWaySide::A,
                Side::Right => TwoWaySide::B,
            };
            pending_edits.push(DiffEdit::SetTwoWayLine {
                session_id,
                side: two_way_side,
                line_no: ln,
                new_text: buf,
                old_text: None,
            });
        }
    } else if input_active
        && was_empty
        && (ui.is_key_pressed(imgui::Key::Backspace) || ui.is_key_pressed(imgui::Key::Delete))
    {
        // Backspace/Delete on an already-empty input: remove the underlying
        // source line. (Single-char + Backspace deletes the char only; the
        // `was_empty` guard prevents the same keystroke from also removing
        // the line.)
        if let Some(ln) = row.line_no {
            let two_way_side = match side {
                Side::Left => TwoWaySide::A,
                Side::Right => TwoWaySide::B,
            };
            let line_idx = (ln as usize).saturating_sub(1);
            line_remove.set(Some(DiffEdit::SpliceTwoWayLines {
                session_id,
                side: two_way_side,
                start: line_idx,
                end: line_idx + 1,
                replacement: Vec::new(),
                old_target_lines: None,
            }));
        }
    }

    if input_active {
        focus_event.set(Some(side.as_focused_pane()));
    }

    drop(_font_tok);

    // Pin layout cursor exactly one row_h() down, regardless of input_text
    // height jitter, so the connector's content-y model stays accurate.
    ui.set_cursor_screen_pos([p0[0], p0[1] + row_h()]);

    if rmb_anchor {
        return row.line_no;
    }
    None
}


pub(super) const ECHO_TOLERANCE: f32 = 0.5;

pub(super) fn sync_scrolls(
    state: &mut DiffViewState,
    curr_left: f32,
    curr_right: f32,
    left_view_h: f32,
    right_view_h: f32,
    left_ranges: &[(u32, f32, f32)],
    right_ranges: &[(u32, f32, f32)],
) {
    let l_changed = (curr_left - state.last_left).abs() > ECHO_TOLERANCE;
    let r_changed = (curr_right - state.last_right).abs() > ECHO_TOLERANCE;
    let l_echo = state
        .written_left
        .map_or(false, |w| (curr_left - w).abs() < ECHO_TOLERANCE);
    let r_echo = state
        .written_right
        .map_or(false, |w| (curr_right - w).abs() < ECHO_TOLERANCE);

    if l_changed && !l_echo {
        if let Some(target) =
            target_scroll(curr_left, left_view_h, right_view_h, left_ranges, right_ranges)
        {
            state.pending_right = Some(target);
        }
    } else if r_changed && !r_echo {
        if let Some(target) = target_scroll(
            curr_right,
            right_view_h,
            left_view_h,
            right_ranges,
            left_ranges,
        ) {
            state.pending_left = Some(target);
        }
    }

    state.last_left = curr_left;
    state.last_right = curr_right;
    if l_echo {
        state.written_left = None;
    }
    if r_echo {
        state.written_right = None;
    }
}
