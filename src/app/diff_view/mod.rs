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

pub use common::{DiffViewState, Side};
use common::{build_pane_ranges, gutter_w, target_scroll, PendingJump, CONNECTOR_W};

/// Minimum scroll change (px) to treat as intentional user input.
/// Dampens single-pixel echo oscillation when we push a new scroll value.
const ECHO_TOLERANCE: f32 = 1.0;

/// Lines of scroll per mouse-wheel tick. Matches typical text-editor feel.
const SCROLL_LINES_PER_WHEEL_TICK: f32 = 3.0;

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
    // Use imgui's actual line height (from the active/mono font metrics) for
    // all overlay positioning so our draw-list text aligns with what
    // input_text_multiline renders.
    let lh = ui.text_line_height();

    // Consume any pending jump set by last frame's hover overlay (↕ button).
    // Translates `pending_jump` into a centered scroll on the target pane.
    // Done after font push so `lh` (text_line_height) reflects the mono font.
    if let Some(jump) = state.pending_jump.take() {
        if jump.session_id == session_id {
            // Center the target line in the pane.
            let target_y = ((jump.target_line as f32 - 1.0) * lh
                - pane_h * 0.5
                + lh * 0.5)
                .max(0.0);
            match jump.pane {
                Side::Left => state.pending_left_scroll = Some(target_y),
                Side::Right => state.pending_right_scroll = Some(target_y),
            }
        }
    }

    let hover_left: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let hover_right: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let pending_jump_cell: Cell<Option<PendingJump>> = Cell::new(None);

    let (left_widget_rect, left_scroll_y) = render_pane(
        ui, state, left_pos, pane_w, pane_h, Side::Left, session_id,
        pending_edits, hunks, anchors, &hover_left, status, store,
        a_highlights, lh,
    );

    // Connector strip: reserve the area; ribbons painted after both panes
    // have rendered so we have their final widget rects + scroll_ys.
    ui.set_cursor_screen_pos(connector_pos);
    ui.invisible_button("connector_strip", [CONNECTOR_W, pane_h]);

    let (right_widget_rect, right_scroll_y) = render_pane(
        ui, state, right_pos, pane_w, pane_h, Side::Right, session_id,
        pending_edits, hunks, anchors, &hover_right, status, store,
        b_highlights, lh,
    );

    let prev_left = state.last_left_scroll_y;
    let prev_right = state.last_right_scroll_y;
    state.last_left_scroll_y = left_scroll_y;
    state.last_right_scroll_y = right_scroll_y;
    let _ = left_widget_rect;
    let _ = right_widget_rect;

    // Scroll sync: whichever pane moved this frame drives the other.
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

    // Bezier connector ribbons between the two panes, drawn after both
    // panes render so we have their final widget rects + scroll values.
    overlay::draw_connector(
        ui,
        connector_pos,
        CONNECTOR_W,
        pane_h,
        left_widget_rect[1] - left_scroll_y,
        right_widget_rect[1] - right_scroll_y,
        &left_ranges,
        &right_ranges,
        anchors,
        hunks,
        lh,
    );

    // Draw the hover panel(s) on top, after both panes have rendered.
    if let Some((hid, pos)) = hover_left.get() {
        overlay::draw_control_overlay(
            ui, session_id, hid, pos, lh, pending_edits, hunks, Side::Left,
            &pending_jump_cell,
        );
    }
    if let Some((hid, pos)) = hover_right.get() {
        overlay::draw_control_overlay(
            ui, session_id, hid, pos, lh, pending_edits, hunks, Side::Right,
            &pending_jump_cell,
        );
    }
    if let Some(j) = pending_jump_cell.get() {
        state.pending_jump = Some(j);
    }

    // Reserve space so subsequent widgets land below the panes.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + pane_h]);

    let _ = focus_request;
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
    highlights: &[crate::app::syntax::LineSpans],
    lh: f32,
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
        let line = overlay::mouse_y_to_line(mouse_y, pane_pos[1], scroll_y_for_anchor, lh);
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
        lh,
        buf_line_count_for_gutter,
    );

    // Screen-space coordinates valid for the foreground draw list.
    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + pane_h,
    ];

    // Own the scroll: imgui's CallbackAlways only fires while the widget
    // is active (focused), and imgui's child-window wheel handler scrolls
    // the internal child regardless. Reading scroll back after build is
    // unreliable, so we take it over instead.
    //
    // - Wheel over the pane → adjust our scroll.
    // - Pending sync scroll (from sister pane or ↕ jump) overrides.
    // - Push our scroll into the widget via igSetNextWindowScroll every
    //   frame so imgui's internal child stays pinned to our value.
    // Tradeoff: dragging imgui's scrollbar doesn't work in this approach;
    // mouse wheel (and our own sync path) are the supported gestures.
    let buf_for_paint_lines: u32 = {
        let buf_ref: &str = match side {
            Side::Left => &state.a_buf,
            Side::Right => &state.b_buf,
        };
        buf_ref.lines().count().max(1) as u32
    };
    let content_h = (buf_for_paint_lines as f32) * lh;
    let max_scroll = (content_h - pane_h).max(0.0);

    let pending_scroll = match side {
        Side::Left => state.pending_left_scroll.take(),
        Side::Right => state.pending_right_scroll.take(),
    };
    let prev_own_scroll = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };
    let wheel = if pending_scroll.is_none()
        && ui.is_mouse_hovering_rect(
            [widget_pos[0], widget_pos[1]],
            [widget_pos[0] + widget_w, widget_pos[1] + pane_h],
        )
    {
        ui.io().mouse_wheel
    } else {
        0.0
    };
    let mut own_scroll = pending_scroll
        .unwrap_or_else(|| prev_own_scroll - wheel * lh * SCROLL_LINES_PER_WHEEL_TICK);
    if own_scroll < 0.0 {
        own_scroll = 0.0;
    }
    if own_scroll > max_scroll {
        own_scroll = max_scroll;
    }

    // Pin the widget's internal child scroll to our value every frame so
    // the rendered text aligns with our overlay. The widget IS the next
    // window for purposes of igSetNextWindowScroll.
    unsafe {
        imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
            x: -1.0,
            y: own_scroll,
        });
    }

    ui.set_cursor_screen_pos(widget_pos);

    let widget_id = format!("##diffie_pane_{:?}_e{}", side, state.input_epoch);

    let caret_byte: Cell<i32> = Cell::new(-1);
    let (changed, buf_clone) = {
        let buf = match side {
            Side::Left => &mut state.a_buf,
            Side::Right => &mut state.b_buf,
        };

        // Suppress imgui's own FrameBg + Text rendering. We paint
        // everything ourselves on the foreground draw list. Keep
        // TextSelectedBg visible (selection rect still useful).
        let _frame_bg = ui.push_style_color(imgui::StyleColor::FrameBg, [0.0, 0.0, 0.0, 0.0]);
        let _frame_bg_hov = ui.push_style_color(imgui::StyleColor::FrameBgHovered, [0.0, 0.0, 0.0, 0.0]);
        let _frame_bg_act = ui.push_style_color(imgui::StyleColor::FrameBgActive, [0.0, 0.0, 0.0, 0.0]);
        let _text_color = ui.push_style_color(imgui::StyleColor::Text, [0.0, 0.0, 0.0, 0.0]);

        // Callback only captures the caret while the widget is active.
        // (CallbackAlways only fires when ActiveId matches the widget id.)
        struct CaretCapture<'a> {
            cursor: &'a Cell<i32>,
        }
        impl<'a> imgui::InputTextCallbackHandler for CaretCapture<'a> {
            fn on_always(&mut self, data: imgui::TextCallbackData) {
                self.cursor.set(data.cursor_pos() as i32);
            }
        }

        let changed = ui
            .input_text_multiline(&widget_id, buf, [widget_w, pane_h])
            .no_undo_redo(true)
            .callback(
                imgui::InputTextMultilineCallback::ALWAYS,
                CaretCapture { cursor: &caret_byte },
            )
            .build();
        let widget_active = ui.is_item_active();
        let clone = if changed { Some(buf.clone()) } else { None };
        (changed, (clone, widget_active))
    };
    let (buf_clone, widget_active) = buf_clone;
    let scroll_y_out = own_scroll;
    if let Some(new_text) = buf_clone {
        let side_ref = SideRef::TwoWay(match side {
            Side::Left => TwoWaySide::A,
            Side::Right => TwoWaySide::B,
        });
        pending_edits.push(DiffEdit::SetSide {
            session_id,
            side: side_ref,
            new_text,
            old_text: None,
        });
    }
    let _ = changed;

    // Paint everything (row bg, sub-line spans, syntax text, caret)
    // on the foreground draw list ourselves.
    let buf_for_paint: &str = match side {
        Side::Left => &state.a_buf,
        Side::Right => &state.b_buf,
    };
    overlay::paint_pane_text(
        ui,
        widget_rect,
        buf_for_paint,
        highlights,
        hunks,
        side,
        scroll_y_out,
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
    );

    (widget_rect, scroll_y_out)
}
