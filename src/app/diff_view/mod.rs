//! 2-way diff view.
//!
//! Side-by-side virtualized panes, a bezier-ribbon connector strip, inline
//! per-hunk decision buttons, center-anchored scroll sync, and click-to-anchor
//! line correspondence. Pending: char-level highlights (step 9).
//!
//! Layout (post-split-into-submodule):
//! - `common.rs`  — shared types/state/helpers (DiffViewState, Side, geometry,
//!   bezier, build_pane, locate_hunk, target_scroll, ...).
//! - `render.rs`  — drawing code (draw_connector, draw_pane, draw_row,
//!   paint_row_text, draw_control_overlay, sync_scrolls). `draw_row` is kept
//!   whole here rather than split: the seam between its painting phase and
//!   its `input_text` widget construction would have required threading
//!   well over a dozen closure cells in both directions (caret_pos /
//!   caret_selection / input_active / changed / buf / was_empty plus six
//!   pre-existing out-cells). Keeping it intact preserves the canonical
//!   single-source-of-truth for the row's text and avoids accidentally
//!   altering the input ordering, per the "no behavior changes" constraint.
//! - `input.rs`   — selection / anchor input handling (update_selection,
//!   handle_anchor_clicks, build_selection_splice).
//! - `tests.rs`   — `word_bounds_tests` + `headless_tests`.

use std::cell::Cell;
use std::collections::HashSet;

use imgui::{FontId, Ui};

use super::undo_stack::DiffEdit;
use super::syntax::LineSpans;
use crate::diff::{Anchor, Hunk};
use crate::session::{SessionId, SessionStore, TwoWaySide};

mod common;
mod input;
mod render;

#[cfg(test)]
mod tests;

pub use common::{
    extract_selection_text, ordered_endpoints, select_all, DiffViewState, SelPoint, Selection,
    Side,
};

use common::{build_pane, gutter_w, row_h, CONNECTOR_W};
use input::{build_selection_splice, handle_anchor_clicks, update_selection};
use render::{draw_connector, draw_pane, sync_scrolls};

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[Hunk],
    anchors: &[Anchor],
    status: &mut String,
    state: &mut DiffViewState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    pending_edits: &mut Vec<DiffEdit>,
    a_highlights: &[LineSpans],
    b_highlights: &[LineSpans],
) {
    let left = build_pane(hunks, Side::Left);
    let right = build_pane(hunks, Side::Right);
    let anchored_a: HashSet<u32> = anchors.iter().map(|a| a.a).collect();
    let anchored_b: HashSet<u32> = anchors.iter().map(|a| a.b).collect();

    // Click capture: rows accumulate into these cells; we apply after the
    // pane render so we can mutate `state` and call into `store` without
    // overlapping borrows.
    let left_click: Cell<Option<u32>> = Cell::new(None);
    let right_click: Cell<Option<u32>> = Cell::new(None);

    // Pane focus event from selection mouse-down. The right pane writes
    // here too — last wins, matching imgui's standard last-clicked focus.
    let focus_event: Cell<Option<crate::app::FocusedPane>> = Cell::new(None);
    // Structural-line request from the inline editor (Backspace/Delete on an
    // empty buffer ⇒ remove that line). Drained into `pending_edits` after
    // both panes render.
    let line_remove: Cell<Option<DiffEdit>> = Cell::new(None);
    // Up/Down arrow focus handoff between row input_texts. Seeded from
    // `state.arrow_focus` so a request set last frame survives; consumed by
    // whichever row matches and re-set by an active row that sees Up/Down.
    let arrow_focus_cell: Cell<Option<(Side, u32, usize)>> = Cell::new(state.arrow_focus.take());
    // Phase-reset for the manually drawn caret blink. Rows write `ui.time()`
    // here whenever an input freshly activates (line change, click) so the
    // caret is guaranteed visible at that instant rather than possibly
    // landing in the "off" half of the cycle.
    let caret_blink_reset_cell: Cell<f64> = Cell::new(state.caret_blink_reset);

    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    let left_scroll = Cell::new(0.0_f32);
    let right_scroll = Cell::new(0.0_f32);
    let left_scroll_x = Cell::new(0.0_f32);
    let right_scroll_x = Cell::new(0.0_f32);
    let left_origin = Cell::new([0.0_f32, 0.0_f32]);
    let right_origin = Cell::new([0.0_f32, 0.0_f32]);
    let left_visible = Cell::new(avail[1]);
    let right_visible = Cell::new(avail[1]);
    // Filled by whichever row's input_text is active this frame, with the
    // imgui-internal selection bounds AFTER our CaretCapture callback ran.
    // Read at end of `render` into `state.last_active_input_selection`.
    let active_selection: Cell<Option<(Side, u32, usize, usize)>> = Cell::new(None);
    // Shift+Up / Shift+Down inside an active input_text extends the
    // cross-row selection. Filled by `draw_row` when shift is held and
    // an arrow key fires: `(side, current_line, current_col, target_line)`.
    // Applied to `state.selection` after the panes render.
    let shift_arrow_extend: Cell<Option<(Side, u32, usize, u32)>> = Cell::new(None);
    // Plain arrow key (no shift) inside an active input_text: clear the
    // cross-row selection. Standard editor behavior — the caret moves
    // and the selection collapses.
    let clear_state_selection: Cell<bool> = Cell::new(false);
    // Set by `draw_row` when an arrow keypress in an active row would
    // trigger imgui's nav-scroll (the gutter-sized horizontal drift the
    // splice path also produces). For Up/Down the requested value is
    // the pre-key scroll_x (just neutralize the snap). For Left/Right
    // the requested value is computed to keep the new caret column
    // visible — same machinery, different target — so the pin doesn't
    // block the cursor-follow scroll users expect.
    let pin_scroll_x_request: Cell<Option<(Side, f32)>> = Cell::new(None);
    // Filled by `draw_row` for the active row: `(side, caret_offset)`
    // where caret_offset is the caret's x offset from the text-start
    // column, in pixels. Read by tests to verify caret–text alignment.
    let caret_offset_cell: Cell<Option<(Side, f32)>> = Cell::new(None);

    // Read the splice-induced scroll-x pin (if any) and decrement its
    // frame counter. We re-push it via `igSetNextWindowScroll` for the
    // matching pane each frame the counter is live.
    let pin_scroll_x: Option<(Side, f32)> = state
        .pin_scroll_x_after_splice
        .as_ref()
        .map(|(s, x, _)| (*s, *x));
    if let Some((_, _, n)) = state.pin_scroll_x_after_splice.as_mut() {
        *n = n.saturating_sub(1);
        if *n == 0 {
            state.pin_scroll_x_after_splice = None;
        }
    }
    // First row's `calc_text_size("m")` under the mono font lands here; the
    // central selection handler needs it to map mouse x to a column.
    let char_w_cell: Cell<f32> = Cell::new(0.0);

    // Carries the drag's side AND whether it has crossed the movement
    // threshold. The threshold flag gates per-row imgui-selection
    // suppression: below threshold (e.g. just after a click or a
    // double-click) we let imgui's native input_text selection alone so
    // double-click word-select can survive; past threshold we collapse it
    // so our drag-selection is the only visible selection.
    let drag_active_side: Option<(Side, bool)> =
        state.drag.as_ref().map(|d| (d.side, d.threshold_passed));

    let frame_selection = state.selection.clone();
    let max_left = (left.rows.len() as f32 * row_h() - avail[1]).max(0.0);
    let max_right = (right.rows.len() as f32 * row_h() - avail[1]).max(0.0);

    // Per-side content widths — drive horizontal scrolling. Push the mono
    // font briefly so `calc_text_size` measures the same font the rows
    // render with, then walk each side's rows to find the widest line.
    // A small trailing padding keeps the caret visible past the last
    // character without immediately overflowing the scrollbar.
    let char_w_global = {
        let _tok = mono_font.map(|f| ui.push_font(f));
        ui.calc_text_size("m")[0].max(1.0)
    };
    let max_chars = |rows: &[common::Row]| -> usize {
        rows.iter()
            .map(|r| r.segments.iter().map(|s| s.text.chars().count()).sum::<usize>())
            .max()
            .unwrap_or(0)
    };
    let content_w_for = |rows: &[common::Row]| -> f32 {
        gutter_w() + (max_chars(rows) as f32) * char_w_global + 16.0
    };
    let content_w_left = content_w_for(&left.rows);
    let content_w_right = content_w_for(&right.rows);

    // Pane geometry, computed before any child renders so we can flip the
    // render order without losing the visual layout. `set_cursor_screen_pos`
    // positions each child explicitly; `connector_origin` is derived from
    // geometry rather than the cursor between the two children.
    let panes_top_left = ui.cursor_screen_pos();
    let left_pos = panes_top_left;
    let right_pos = [panes_top_left[0] + pane_w + CONNECTOR_W, panes_top_left[1]];
    let connector_origin = [panes_top_left[0] + pane_w, panes_top_left[1]];

    // Driver detection: which pane is the mouse hovering when the wheel
    // fires? We need to know up-front so we can render the driver first,
    // capture its post-wheel scroll, and push the follower's matching
    // scroll via `igSetNextWindowScroll` — all within the same frame.
    let mouse_pos = ui.io().mouse_pos;
    let wheel = ui.io().mouse_wheel;
    let in_y = mouse_pos[1] >= panes_top_left[1]
        && mouse_pos[1] < panes_top_left[1] + avail[1];
    let in_right_x =
        mouse_pos[0] >= right_pos[0] && mouse_pos[0] < right_pos[0] + pane_w;
    let right_first = wheel.abs() > 1e-3 && in_y && in_right_x;

    // --- Inline closures kept lightweight by separating "render" from the
    // sync math. Each render branch threads the same state mutations
    // (pending_X, written_X) explicitly to keep the borrow checker happy.

    if right_first {
        // --- right is driver: render right first ---
        ui.set_cursor_screen_pos(right_pos);
        {
            let y_to_apply = state.pending_right.take();
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Right)
                .map(|(_, x)| x);
            if y_to_apply.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = y_to_apply.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = y_to_apply {
                    state.written_right = Some(y);
                }
            }
        }
        ui.child_window("diffie_right")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_right, 0.0])
            .build(|| {
                right_scroll.set(ui.scroll_y());
                right_scroll_x.set(ui.scroll_x());
                right_origin.set(ui.cursor_screen_pos());
                right_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &right.rows,
                    Side::Right,
                    session_id,
                    &anchored_b,
                    &right_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    b_highlights,
                    content_w_right,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                    &pin_scroll_x_request,
                    &caret_offset_cell,
                );
            });

        // Right pane has applied its wheel-induced scroll — derive matching
        // left target same-frame.
        let cur_right_for_sync = right_scroll.get();
        let r_changed = (cur_right_for_sync - state.last_right).abs() > render::ECHO_TOLERANCE;
        let r_echo = state
            .written_right
            .map_or(false, |w| (cur_right_for_sync - w).abs() < render::ECHO_TOLERANCE);
        let left_override = if r_changed && !r_echo {
            common::target_scroll(
                cur_right_for_sync,
                avail[1],
                avail[1],
                &right.ranges,
                &left.ranges,
            )
            .map(|t| t.clamp(0.0, max_left))
        } else {
            None
        };
        // Always drain `pending_left` so a same-frame override never leaves a
        // stale value queued for the next frame (which would snap-back the
        // scroll when the wheel rate dips below threshold mid-gesture).
        let pending_consumed = state.pending_left.take();
        let apply_left = left_override.or(pending_consumed);

        ui.set_cursor_screen_pos(left_pos);
        {
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Left)
                .map(|(_, x)| x);
            if apply_left.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = apply_left.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = apply_left {
                    state.written_left = Some(y);
                }
            }
        }
        ui.child_window("diffie_left")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_left, 0.0])
            .build(|| {
                left_scroll.set(ui.scroll_y());
                left_scroll_x.set(ui.scroll_x());
                left_origin.set(ui.cursor_screen_pos());
                left_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &left.rows,
                    Side::Left,
                    session_id,
                    &anchored_a,
                    &left_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    a_highlights,
                    content_w_left,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                    &pin_scroll_x_request,
                    &caret_offset_cell,
                );
            });

        // Stamp `written_X` with the actually-rendered scroll values, not
        // the floats we requested. Imgui rounds Scroll to whole pixels, so
        // when `left_override` is fractional (e.g. 2369.5) the echoed value
        // comes back as 2369.0 — exactly `ECHO_TOLERANCE` off, which the
        // strict `<` check trips into `!l_echo` and `sync_scrolls` queues a
        // stale `pending_right`. Storing the post-render value makes the
        // echo check exact.
        state.written_left = Some(left_scroll.get());
        state.written_right = Some(cur_right_for_sync);
    } else {
        // --- left is driver (or no wheel): render left first ---
        ui.set_cursor_screen_pos(left_pos);
        {
            let y_to_apply = state.pending_left.take();
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Left)
                .map(|(_, x)| x);
            if y_to_apply.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = y_to_apply.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = y_to_apply {
                    state.written_left = Some(y);
                }
            }
        }
        ui.child_window("diffie_left")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_left, 0.0])
            .build(|| {
                left_scroll.set(ui.scroll_y());
                left_scroll_x.set(ui.scroll_x());
                left_origin.set(ui.cursor_screen_pos());
                left_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &left.rows,
                    Side::Left,
                    session_id,
                    &anchored_a,
                    &left_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    a_highlights,
                    content_w_left,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                    &pin_scroll_x_request,
                    &caret_offset_cell,
                );
            });

        let cur_left_for_sync = left_scroll.get();
        let l_changed = (cur_left_for_sync - state.last_left).abs() > render::ECHO_TOLERANCE;
        let l_echo = state
            .written_left
            .map_or(false, |w| (cur_left_for_sync - w).abs() < render::ECHO_TOLERANCE);
        let right_override = if l_changed && !l_echo {
            common::target_scroll(
                cur_left_for_sync,
                avail[1],
                avail[1],
                &left.ranges,
                &right.ranges,
            )
            .map(|t| t.clamp(0.0, max_right))
        } else {
            None
        };
        // Always drain `pending_right` — see the right_first branch for why.
        let pending_consumed = state.pending_right.take();
        let apply_right = right_override.or(pending_consumed);

        ui.set_cursor_screen_pos(right_pos);
        {
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Right)
                .map(|(_, x)| x);
            if apply_right.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = apply_right.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = apply_right {
                    state.written_right = Some(y);
                }
            }
        }
        ui.child_window("diffie_right")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_right, 0.0])
            .build(|| {
                right_scroll.set(ui.scroll_y());
                right_scroll_x.set(ui.scroll_x());
                right_origin.set(ui.cursor_screen_pos());
                right_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &right.rows,
                    Side::Right,
                    session_id,
                    &anchored_b,
                    &right_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    b_highlights,
                    content_w_right,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                    &pin_scroll_x_request,
                    &caret_offset_cell,
                );
            });

        // Mirror of the right_first branch — see comment there. Store the
        // actually-rendered scrolls so the next frame's echo check is exact.
        let cur_right_post = right_scroll.get();
        state.written_left = Some(cur_left_for_sync);
        state.written_right = Some(cur_right_post);
    }
    // Restore the cursor below the diff area for any subsequent widgets.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + avail[1]]);

    if let Some(p) = focus_event.get() {
        *focus_request = Some(p);
    }
    // Persist any unconsumed arrow-focus request for the next frame (the
    // target row may not have been visible this frame).
    state.arrow_focus = arrow_focus_cell.take();
    state.caret_blink_reset = caret_blink_reset_cell.get();
    // Apply Shift+Up/Down cross-row selection extension. Anchor is
    // preserved from any existing same-side selection; otherwise it's
    // pinned at the cursor's pre-move position. Caret moves to the
    // same column on the adjacent line.
    if let Some((side, cur_ln, cur_col, new_ln)) = shift_arrow_extend.take() {
        let anchor = match state.selection.as_ref() {
            Some(s) if s.side == side => s.anchor,
            _ => SelPoint { line_no: cur_ln, col: cur_col },
        };
        state.selection = Some(Selection {
            side,
            anchor,
            caret: SelPoint { line_no: new_ln, col: cur_col },
        });
    } else if clear_state_selection.take() {
        // Plain arrow press inside the active row — collapse any
        // existing cross-row selection. Mutually exclusive with the
        // shift-extend branch above.
        state.selection = None;
    }
    // An arrow keypress requested a scroll-x pin. `draw_row` already
    // computed the target value (pre-key scroll for Up/Down, cursor-
    // follow scroll for Left/Right). Same field + countdown as the
    // splice pin.
    if let Some((side, target)) = pin_scroll_x_request.take() {
        state.pin_scroll_x_after_splice = Some((side, target, 4));
    }
    if let Some(edit) = line_remove.take() {
        pending_edits.push(edit);
    }

    update_selection(
        ui,
        state,
        &left,
        &right,
        left_origin.get(),
        right_origin.get(),
        left_visible.get(),
        right_visible.get(),
        pane_w,
        char_w_cell.get(),
        focus_request,
    );

    // Selection + Delete/Backspace ⇒ splice the selected range out of the
    // source side. We bypass `want_capture_keyboard` so a focused row's
    // input_text doesn't swallow Delete when a multi-line selection exists.
    //
    // The focused row's input_text will have *already* processed the same
    // Backspace this frame, queuing its own `SetTwoWayLine` for line 1 (the
    // active row) into `pending_edits`. That edit refers to a line we're
    // about to splice out, so we drop it before pushing the Splice — this
    // collapses the deletion into a single undo entry whose snapshot
    // reflects the true pre-keystroke state.
    let key_pressed = ui.is_key_pressed(imgui::Key::Delete)
        || ui.is_key_pressed(imgui::Key::Backspace);
    if key_pressed {
        if let Some(sel) = state.selection.as_ref().cloned() {
            if let Ok(snap) = store.snapshot(session_id) {
                if let Some(edit) = build_selection_splice(&snap, &sel, session_id) {
                    let (lo, hi) = ordered_endpoints(&sel);
                    let sel_side: TwoWaySide = match sel.side {
                        Side::Left => TwoWaySide::A,
                        Side::Right => TwoWaySide::B,
                    };
                    pending_edits.retain(|e| match e {
                        DiffEdit::SetTwoWayLine {
                            session_id: e_sid,
                            side: e_side,
                            line_no,
                            ..
                        } => !(*e_sid == session_id
                            && *e_side == sel_side
                            && *line_no >= lo.line_no
                            && *line_no <= hi.line_no),
                        _ => true,
                    });
                    pending_edits.push(edit);
                    // Park the caret on the surviving merged line at the
                    // selection's start column, so the next frame's render
                    // refocuses that row's input_text instead of leaving
                    // nothing active after the splice tears down the old
                    // row's widget id.
                    state.arrow_focus = Some((sel.side, lo.line_no, lo.col));
                    // Pin scroll_x for next frame: `set_keyboard_focus_here`
                    // would otherwise nav-scroll the pane to bring the wide
                    // input_text into view. The cells hold this frame's
                    // pre-edit scroll position — exactly what we want to
                    // restore.
                    let cur_scroll_x = match sel.side {
                        Side::Left => left_scroll_x.get(),
                        Side::Right => right_scroll_x.get(),
                    };
                    state.pin_scroll_x_after_splice = Some((sel.side, cur_scroll_x, 4));
                    state.selection = None;
                }
            }
        }
    }

    // Any structural edit invalidates line numbers the selection refers to.
    if pending_edits.iter().any(|e| {
        matches!(
            e,
            DiffEdit::SpliceTwoWayLines { .. } | DiffEdit::ReplaceHunkSide { .. }
        )
    }) {
        state.selection = None;
    }

    handle_anchor_clicks(
        store,
        session_id,
        state,
        status,
        left_click.get(),
        right_click.get(),
    );

    let l = left_scroll.get();
    let r = right_scroll.get();
    let l_view = left_visible.get();
    let r_view = right_visible.get();
    sync_scrolls(state, l, r, l_view, r_view, &left.ranges, &right.ranges);
    state.last_left_scroll_x = left_scroll_x.get();
    state.last_right_scroll_x = right_scroll_x.get();
    state.last_active_input_selection = active_selection.get();
    state.last_active_caret_offset = caret_offset_cell.get();

    draw_connector(
        ui,
        connector_origin,
        CONNECTOR_W,
        avail[1],
        left_origin.get()[1],
        right_origin.get()[1],
        &left.ranges,
        &right.ranges,
        &left.line_ys,
        &right.line_ys,
        anchors,
        hunks,
    );
}
