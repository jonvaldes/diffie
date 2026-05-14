//! 2-way diff view: two `input_text_multiline` widgets side-by-side
//! with a (currently empty) bezier connector strip in the middle.
//!
//! This is the post-Task-5 skeleton: text renders + edits, but no
//! per-row decorations, hover overlay, anchor gutter, or sub-line
//! spans. Those come back in tasks 6-8.

use imgui::{FontId, Ui};

use crate::diff::{Anchor, Hunk};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};

mod common;
mod overlay;

#[cfg(test)]
mod tests;

pub use common::{extract_selection_text, DiffViewState, Side};
use common::{gutter_w, CONNECTOR_W};

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

    let (left_widget_rect, left_scroll_y) = render_pane(
        ui, state, left_pos, pane_w, pane_h, Side::Left, session_id,
        pending_edits, hunks,
    );

    // Connector strip — empty for now, ribbons added back in task 6.
    ui.set_cursor_screen_pos(connector_pos);
    ui.invisible_button("connector_strip", [CONNECTOR_W, pane_h]);

    let (right_widget_rect, right_scroll_y) = render_pane(
        ui, state, right_pos, pane_w, pane_h, Side::Right, session_id,
        pending_edits, hunks,
    );

    state.last_left_scroll_y = left_scroll_y;
    state.last_right_scroll_y = right_scroll_y;
    let _ = left_widget_rect;
    let _ = right_widget_rect;

    // Reserve space so subsequent widgets land below the panes.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + pane_h]);

    let _ = focus_request;
    let _ = anchors;
    let _ = a_highlights;
    let _ = b_highlights;
    let _ = status;
}

fn render_pane(
    ui: &Ui,
    state: &mut DiffViewState,
    pane_pos: [f32; 2],
    pane_w: f32,
    pane_h: f32,
    side: Side,
    session_id: SessionId,
    pending_edits: &mut Vec<DiffEdit>,
    _hunks: &[Hunk],
) -> ([f32; 4], f32) {
    let g_w = gutter_w();
    let widget_pos = [pane_pos[0] + g_w, pane_pos[1]];
    let widget_w = pane_w - g_w;

    // Gutter strip — drawn in overlay task; for now reserve via invisible_button.
    ui.set_cursor_screen_pos(pane_pos);
    ui.invisible_button(format!("gutter_{:?}", side), [g_w, pane_h]);

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
    let buf = match side {
        Side::Left => &mut state.a_buf,
        Side::Right => &mut state.b_buf,
    };
    let widget_id = format!("##diffie_pane_{:?}", side);
    let changed = ui
        .input_text_multiline(&widget_id, buf, [widget_w, pane_h])
        .no_undo_redo(true)
        .build();
    let scroll_y = 0.0; // placeholder until task 6 wires the inside-widget scroll read
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
    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + pane_h,
    ];
    (widget_rect, scroll_y)
}
