//! 2-way diff view: two `input_text_multiline` widgets side-by-side
//! with a (currently empty) bezier connector strip in the middle.
//!
//! This is the post-Task-5 skeleton: text renders + edits, but no
//! per-row decorations, hover overlay, anchor gutter, or sub-line
//! spans. Those come back in tasks 6-8.

use std::cell::Cell;

use imgui::{FontId, Ui};

use crate::diff::{Anchor, Hunk};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};

mod common;
mod overlay;

#[cfg(test)]
mod tests;

pub use common::{extract_selection_text, DiffViewState, Side};
use common::{build_pane_ranges, gutter_w, line_h, target_scroll, PendingJump, CONNECTOR_W};

/// Minimum scroll change (px) to treat as intentional user input.
/// Dampens single-pixel echo oscillation when we push a new scroll value.
const ECHO_TOLERANCE: f32 = 1.0;

use super::undo_stack::DiffEdit;

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
    a_highlights: &[crate::app::syntax::LineSpans],
    b_highlights: &[crate::app::syntax::LineSpans],
) {
    // Sync buffers from session at frame start.
    let snap = match store.snapshot(session_id) {
        Ok(s) => s,
        Err(_) => return,
    };
    let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode else {
        return;
    };
    if state.a_buf != *a_text {
        state.a_buf = a_text.clone();
    }
    if state.b_buf != *b_text {
        state.b_buf = b_text.clone();
    }

    let avail = ui.content_region_avail();
    let total_w = avail[0];
    let pane_w = ((total_w - CONNECTOR_W) * 0.5).max(100.0);
    let pane_h = avail[1].max(100.0);

    let panes_top_left = ui.cursor_screen_pos();
    let left_pos = panes_top_left;
    let connector_pos = [left_pos[0] + pane_w, left_pos[1]];
    let right_pos = [connector_pos[0] + CONNECTOR_W, left_pos[1]];

    let _font_tok = mono_font.map(|f| ui.push_font(f));

    let hover_left: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let hover_right: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let pending_jump_cell: Cell<Option<PendingJump>> = Cell::new(None);

    let (left_widget_rect, left_scroll_y) = render_pane(
        ui, state, left_pos, pane_w, pane_h, Side::Left, session_id,
        pending_edits, hunks, anchors, &hover_left, status, store,
    );

    // Connector strip — empty for now, ribbons added back in a later task.
    ui.set_cursor_screen_pos(connector_pos);
    ui.invisible_button("connector_strip", [CONNECTOR_W, pane_h]);

    let (right_widget_rect, right_scroll_y) = render_pane(
        ui, state, right_pos, pane_w, pane_h, Side::Right, session_id,
        pending_edits, hunks, anchors, &hover_right, status, store,
    );

    let prev_left = state.last_left_scroll_y;
    let prev_right = state.last_right_scroll_y;
    state.last_left_scroll_y = left_scroll_y;
    state.last_right_scroll_y = right_scroll_y;
    let _ = left_widget_rect;
    let _ = right_widget_rect;

    // Scroll sync: whichever pane moved this frame drives the other.
    let lh = line_h();
    let left_changed = (left_scroll_y - prev_left).abs() > ECHO_TOLERANCE;
    let right_changed = (right_scroll_y - prev_right).abs() > ECHO_TOLERANCE;
    let left_ranges = build_pane_ranges(hunks, Side::Left, lh);
    let right_ranges = build_pane_ranges(hunks, Side::Right, lh);

    if left_changed && !right_changed {
        if let Some(target) = target_scroll(
            left_scroll_y, pane_h, pane_h, &left_ranges, &right_ranges,
        ) {
            state.pending_right_scroll = Some(target);
        }
    } else if right_changed && !left_changed {
        if let Some(target) = target_scroll(
            right_scroll_y, pane_h, pane_h, &right_ranges, &left_ranges,
        ) {
            state.pending_left_scroll = Some(target);
        }
    }

    // Draw the hover panel(s) on top, after both panes have rendered.
    if let Some((hid, pos)) = hover_left.get() {
        overlay::draw_control_overlay(
            ui, session_id, hid, pos, pending_edits, hunks, Side::Left,
            &pending_jump_cell,
        );
    }
    if let Some((hid, pos)) = hover_right.get() {
        overlay::draw_control_overlay(
            ui, session_id, hid, pos, pending_edits, hunks, Side::Right,
            &pending_jump_cell,
        );
    }
    if let Some(j) = pending_jump_cell.get() {
        state.pending_jump = Some(j);
    }

    // Reserve space so subsequent widgets land below the panes.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + pane_h]);

    let _ = focus_request;
    let _ = a_highlights;
    let _ = b_highlights;
}

fn handle_anchor_click(
    state: &mut DiffViewState,
    side: Side,
    line: u32,
    status: &mut String,
    store: &SessionStore,
    session_id: SessionId,
) {
    match side {
        Side::Left => state.pending_a = Some(line),
        Side::Right => state.pending_b = Some(line),
    }
    if let (Some(a), Some(b)) = (state.pending_a, state.pending_b) {
        match store.add_anchor_two_way(session_id, Anchor { a, b }) {
            Ok(()) => *status = format!("anchor added: A:{a} <-> B:{b}"),
            Err(e) => *status = format!("anchor error: {e}"),
        }
        state.pending_a = None;
        state.pending_b = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    ui: &Ui,
    state: &mut DiffViewState,
    pane_pos: [f32; 2],
    pane_w: f32,
    pane_h: f32,
    side: Side,
    session_id: SessionId,
    pending_edits: &mut Vec<DiffEdit>,
    hunks: &[Hunk],
    anchors: &[Anchor],
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
    status: &mut String,
    store: &SessionStore,
) -> ([f32; 4], f32) {
    let g_w = gutter_w();
    let widget_pos = [pane_pos[0] + g_w, pane_pos[1]];
    let widget_w = pane_w - g_w;

    // Gutter strip — click to pin anchors; paint dots + line numbers.
    ui.set_cursor_screen_pos(pane_pos);
    let gutter_clicked = ui.invisible_button(format!("gutter_{:?}", side), [g_w, pane_h]);
    let scroll_y_for_anchor = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };
    if gutter_clicked {
        let mouse_y = ui.io().mouse_pos[1];
        let line = overlay::mouse_y_to_line(mouse_y, pane_pos[1], scroll_y_for_anchor, line_h());
        handle_anchor_click(state, side, line, status, store, session_id);
    }
    let gutter_rect = [pane_pos[0], pane_pos[1], pane_pos[0] + g_w, pane_pos[1] + pane_h];
    let buf_line_count_for_gutter = {
        let buf_ref: &str = match side {
            Side::Left => &state.a_buf,
            Side::Right => &state.b_buf,
        };
        (buf_ref.lines().count().max(1)) as u32
    };
    overlay::paint_gutter(
        ui,
        gutter_rect,
        anchors,
        side,
        scroll_y_for_anchor,
        buf_line_count_for_gutter,
    );

    // Apply any pending scroll set last frame.
    let pending_scroll = match side {
        Side::Left => state.pending_left_scroll.take(),
        Side::Right => state.pending_right_scroll.take(),
    };
    if let Some(y) = pending_scroll {
        unsafe {
            imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x: -1.0, y });
        }
    }

    ui.set_cursor_screen_pos(widget_pos);

    // Pre-compute line count from the buffer (need it before the closure
    // takes a mutable borrow). `lines()` doesn't count a trailing empty
    // line, so add 1 if the buffer ends with '\n' (or is empty).
    let (buf_line_count, _) = {
        let buf_ref: &str = match side {
            Side::Left => &state.a_buf,
            Side::Right => &state.b_buf,
        };
        let n = buf_ref.lines().count().max(1);
        let trailing = buf_ref.is_empty() || buf_ref.ends_with('\n');
        (n + if trailing { 1 } else { 0 }, ())
    };

    let lh = crate::app::diff_view::common::line_h();
    let content_h = (buf_line_count as f32 * lh).max(pane_h);

    let widget_id = format!("##diffie_pane_{:?}", side);
    let child_id = format!("##diffie_pane_child_{:?}", side);

    let mut scroll_y_out: f32 = 0.0;
    // Screen-space coordinates valid for the child window's draw list
    // (imgui draw list positions are always in screen space).
    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + pane_h,
    ];

    ui.child_window(&child_id)
        .size([widget_w, pane_h])
        .scroll_bar(true)
        .build(|| {
            let buf = match side {
                Side::Left => &mut state.a_buf,
                Side::Right => &mut state.b_buf,
            };
            let changed = ui
                .input_text_multiline(&widget_id, buf, [widget_w, content_h])
                .no_undo_redo(true)
                .build();
            // Read scroll AFTER build() so input events (including scroll)
            // have been processed; reading before would return last frame's value.
            scroll_y_out = ui.scroll_y();
            if changed {
                let side_ref = SideRef::TwoWay(match side {
                    Side::Left => TwoWaySide::A,
                    Side::Right => TwoWaySide::B,
                });
                pending_edits.push(DiffEdit::SetSide {
                    session_id,
                    side: side_ref,
                    new_text: buf.clone(),
                    old_text: None,
                });
            }

            // Paint overlays on top of the widget.
            overlay::paint_row_overlays(ui, widget_rect, hunks, side, scroll_y_out, hover_out);
        });

    (widget_rect, scroll_y_out)
}
