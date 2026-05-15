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
use common::{
    build_pane_ranges, gutter_w, next_anchor_pick, rail_w, target_scroll, AnchorPick,
    PendingJump, RailAction, RailClick, RailEvent, CONNECTOR_W,
};

/// Minimum scroll change (px) to treat as intentional user input.
/// Dampens single-pixel echo oscillation when we push a new scroll value.
const ECHO_TOLERANCE: f32 = 1.0;

/// Lines of scroll per mouse-wheel tick. Matches typical text-editor feel.
const SCROLL_LINES_PER_WHEEL_TICK: f32 = 3.0;

/// Exponential easing rate for smooth scroll. ~25 gives a half-life of
/// roughly 28 ms — snappy but visibly animated. Higher = stiffer / less
/// smoothing.
const SCROLL_SMOOTH_SPEED: f32 = 25.0;
/// When displayed is within this many pixels of target, snap to avoid an
/// endless asymptotic tail.
const SCROLL_SNAP_EPSILON: f32 = 0.5;

use super::undo_stack::DiffEdit;

/// Max line pixel width across `buf` under the active imgui font.
/// Used to size the inner multiline wide enough that no internal
/// horizontal caret-tracking kicks in.
fn compute_max_line_w(ui: &Ui, buf: &str) -> f32 {
    let mut max = 0.0_f32;
    for line in buf.lines() {
        let w = ui.calc_text_size(line)[0];
        if w > max {
            max = w;
        }
    }
    max
}

/// Pixel x of the caret inside the inner multiline (= padding_x +
/// width-of-prefix on the caret's line). Walks the buffer by lines to
/// locate the caret; returns `padding_x` if the byte offset is out of
/// range.
pub(crate) fn caret_x_in_inner(buf: &str, caret_byte: usize, ui: &Ui, padding_x: f32) -> f32 {
    let mut byte_acc: usize = 0;
    for line_text in buf.lines() {
        let line_end = byte_acc + line_text.len();
        if caret_byte >= byte_acc && caret_byte <= line_end {
            let local = caret_byte - byte_acc;
            let mut snap = local.min(line_text.len());
            while snap > 0 && !line_text.is_char_boundary(snap) {
                snap -= 1;
            }
            return padding_x + ui.calc_text_size(&line_text[..snap])[0];
        }
        byte_acc = line_end + 1; // +1 for '\n'
    }
    padding_x
}

/// Given the caret's pixel-x and the visible viewport `[scroll_x, scroll_x + view_w]`,
/// return the scroll_x that keeps the caret inside the viewport with `margin`
/// pixels of slack on each side. Returns `scroll_x` unchanged if the caret is
/// already comfortably inside.
pub(crate) fn track_caret_scroll_x(caret_x: f32, scroll_x: f32, view_w: f32, margin: f32) -> f32 {
    if caret_x < scroll_x + margin {
        (caret_x - margin).max(0.0)
    } else if caret_x > scroll_x + view_w - margin {
        caret_x - view_w + margin
    } else {
        scroll_x
    }
}

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
    let a_changed = state.a_buf != *a_text;
    if a_changed {
        state.a_buf = a_text.clone();
    }
    let b_changed = state.b_buf != *b_text;
    if b_changed {
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

    // Recompute max line widths under the active mono font when the
    // buffer changes. Used to size the inner multiline wide enough that
    // imgui's own horizontal caret-tracking never triggers — the outer
    // scroll wrapper handles user-facing horizontal scrolling.
    if a_changed {
        state.a_max_line_w = compute_max_line_w(ui, &state.a_buf);
    }
    if b_changed {
        state.b_max_line_w = compute_max_line_w(ui, &state.b_buf);
    }

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

    // Snapshot the previous frame's targets before render_pane mutates them,
    // so the sync detector can tell which pane the user touched this frame.
    let prev_left_target_for_sync = state.target_left_scroll;
    let prev_right_target_for_sync = state.target_right_scroll;

    let (left_widget_rect, left_scroll_y) = render_pane(
        ui, state, left_pos, pane_w, pane_h, Side::Left, session_id,
        pending_edits, hunks, anchors, &hover_left,
        a_highlights, lh,
    );

    // Connector strip: split into left rail / middle / right rail.
    // Invisible buttons capture hover/click; ribbons painted after both panes.
    let rail_w_now = rail_w();
    let left_rail_pos = connector_pos;
    let middle_pos = [connector_pos[0] + rail_w_now, connector_pos[1]];
    let middle_w = (CONNECTOR_W - 2.0 * rail_w_now).max(0.0);
    let right_rail_pos = [connector_pos[0] + CONNECTOR_W - rail_w_now, connector_pos[1]];

    ui.set_cursor_screen_pos(left_rail_pos);
    let left_rail_clicked = ui.invisible_button("anchor_rail_L", [rail_w_now, pane_h]);
    let left_rail_hovered = ui.is_item_hovered();

    ui.set_cursor_screen_pos(middle_pos);
    let middle_clicked = ui.invisible_button("connector_middle", [middle_w, pane_h]);
    let _ = middle_clicked;

    ui.set_cursor_screen_pos(right_rail_pos);
    let right_rail_clicked = ui.invisible_button("anchor_rail_R", [rail_w_now, pane_h]);
    let right_rail_hovered = ui.is_item_hovered();

    let (right_widget_rect, right_scroll_y) = render_pane(
        ui, state, right_pos, pane_w, pane_h, Side::Right, session_id,
        pending_edits, hunks, anchors, &hover_right,
        b_highlights, lh,
    );

    let prev_left_target = prev_left_target_for_sync;
    let prev_right_target = prev_right_target_for_sync;
    state.last_left_scroll_y = left_scroll_y;
    state.last_right_scroll_y = right_scroll_y;

    // Translate rail hover/click + Esc into a RailEvent, then step the
    // anchor-pick state machine. Done after both panes render so we have
    // the current frame's eased scroll values for line-mapping.
    let mouse_y = ui.io().mouse_pos[1];
    let left_hover_line = if left_rail_hovered {
        Some(overlay::mouse_y_to_line(mouse_y, left_pos[1], state.last_left_scroll_y, lh))
    } else {
        None
    };
    let right_hover_line = if right_rail_hovered {
        Some(overlay::mouse_y_to_line(mouse_y, right_pos[1], state.last_right_scroll_y, lh))
    } else {
        None
    };

    fn anchor_idx_for(anchors: &[crate::diff::Anchor], side: Side, line: u32) -> Option<usize> {
        anchors.iter().position(|a| match side {
            Side::Left => a.a == line,
            Side::Right => a.b == line,
        })
    }

    let escape_pressed = ui.is_key_pressed(imgui::Key::Escape);
    let rail_event: RailEvent = if escape_pressed {
        RailEvent::Escape
    } else if left_rail_clicked {
        let line = left_hover_line.unwrap_or(1);
        let idx = anchor_idx_for(anchors, Side::Left, line);
        RailEvent::Click(RailClick {
            side: Side::Left,
            line,
            anchor_idx: idx,
        })
    } else if right_rail_clicked {
        let line = right_hover_line.unwrap_or(1);
        let idx = anchor_idx_for(anchors, Side::Right, line);
        RailEvent::Click(RailClick {
            side: Side::Right,
            line,
            anchor_idx: idx,
        })
    } else if matches!(state.anchor_pick, AnchorPick::Picking { .. })
        && ui.is_mouse_clicked(imgui::MouseButton::Left)
        && !left_rail_hovered
        && !right_rail_hovered
    {
        // While picking, a left-click that is NOT on either rail cancels.
        // `is_mouse_clicked` fires on press while the rails' `invisible_button`
        // fires on release; suppressing this branch when a rail is hovered
        // keeps the press from cancelling the pick a frame before the release
        // completes the anchor.
        RailEvent::ClickedElsewhere
    } else {
        RailEvent::None
    };

    let (next_pick, action) = next_anchor_pick(state.anchor_pick, rail_event);
    state.anchor_pick = next_pick;
    match action {
        RailAction::None => {}
        RailAction::RemoveAnchor { idx } => {
            match store.remove_anchor(session_id, idx) {
                Ok(()) => *status = "anchor removed".to_string(),
                Err(e) => *status = format!("anchor error: {e}"),
            }
        }
        RailAction::AddAnchor { a, b } => {
            match store.add_anchor_two_way(session_id, crate::diff::Anchor { a, b }) {
                Ok(()) => *status = format!("anchor added: A:{a} <-> B:{b}"),
                Err(e) => *status = format!("anchor error: {e}"),
            }
        }
    }

    // Scroll sync: compare *targets*, not eased displayed values, so the
    // sync trigger fires once per user gesture rather than every animation
    // frame.
    let left_changed = (state.target_left_scroll - prev_left_target).abs() > ECHO_TOLERANCE;
    let right_changed = (state.target_right_scroll - prev_right_target).abs() > ECHO_TOLERANCE;
    let left_ranges = build_pane_ranges(hunks, Side::Left, lh);
    let right_ranges = build_pane_ranges(hunks, Side::Right, lh);

    if left_changed && !right_changed {
        if let Some(target) = target_scroll(
            state.target_left_scroll, pane_h, pane_h, &left_ranges, &right_ranges,
        ) {
            state.pending_right_scroll = Some(target);
        }
    } else if right_changed && !left_changed {
        if let Some(target) = target_scroll(
            state.target_right_scroll, pane_h, pane_h, &right_ranges, &left_ranges,
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
        state.anchor_pick,
        left_rail_pos[0] + rail_w_now * 0.5,
        right_rail_pos[0] + rail_w_now * 0.5,
    );

    // Paint anchor rail icons on top of the ribbons.
    let left_rail_rect = [
        left_rail_pos[0],
        left_rail_pos[1],
        left_rail_pos[0] + rail_w_now,
        left_rail_pos[1] + pane_h,
    ];
    let right_rail_rect = [
        right_rail_pos[0],
        right_rail_pos[1],
        right_rail_pos[0] + rail_w_now,
        right_rail_pos[1] + pane_h,
    ];
    overlay::paint_anchor_rail(
        ui,
        left_rail_rect,
        left_pos[1],
        left_scroll_y,
        lh,
        Side::Left,
        anchors,
        left_hover_line,
        state.anchor_pick,
    );
    overlay::paint_anchor_rail(
        ui,
        right_rail_rect,
        right_pos[1],
        right_scroll_y,
        lh,
        Side::Right,
        anchors,
        right_hover_line,
        state.anchor_pick,
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
    highlights: &[crate::app::syntax::LineSpans],
    lh: f32,
) -> ([f32; 4], f32) {
    let g_w = gutter_w();
    let widget_pos = [pane_pos[0] + g_w, pane_pos[1]];
    let widget_w = pane_w - g_w;

    // Gutter strip — display only; clicks handled by rails in the connector.
    ui.set_cursor_screen_pos(pane_pos);
    ui.dummy([g_w, pane_h]); // gutter strip — display only, clicks handled by rails
    let scroll_y_for_anchor = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };
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
    let prev_target = match side {
        Side::Left => state.target_left_scroll,
        Side::Right => state.target_right_scroll,
    };
    let prev_displayed = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };
    // Split wheel into vertical (smooth, smooth-eased into the inner
    // multiline) and horizontal (pinned onto the outer scroll child).
    // Imgui's own UpdateMouseWheel only operates on the topmost hovered
    // window — that's usually the inner multiline's child, which can't
    // scroll horizontally — so we have to drive the outer scroll
    // ourselves rather than relying on imgui to bubble the wheel.
    let hovered = ui.is_mouse_hovering_rect(
        [widget_pos[0], widget_pos[1]],
        [widget_pos[0] + widget_w, widget_pos[1] + pane_h],
    );
    let (wheel, h_wheel) = if hovered {
        let raw_v = ui.io().mouse_wheel;
        let raw_h = ui.io().mouse_wheel_h;
        if ui.io().key_shift && raw_v != 0.0 {
            (0.0, raw_h + raw_v)
        } else {
            (raw_v, raw_h)
        }
    } else {
        (0.0, 0.0)
    };
    // Compute this frame's *target*. Pending (sync / jump) overrides; otherwise
    // mouse wheel deflects from the previous target so multiple wheel ticks
    // before the easing catches up still register fully.
    let mut target = pending_scroll
        .unwrap_or_else(|| prev_target - wheel * lh * SCROLL_LINES_PER_WHEEL_TICK);
    if target < 0.0 {
        target = 0.0;
    }
    if target > max_scroll {
        target = max_scroll;
    }
    match side {
        Side::Left => state.target_left_scroll = target,
        Side::Right => state.target_right_scroll = target,
    }

    // Ease the displayed scroll toward target with an exponential decay.
    let dt = ui.io().delta_time.max(0.0).min(0.1);
    let k = 1.0 - (-dt * SCROLL_SMOOTH_SPEED).exp();
    let mut displayed = prev_displayed + (target - prev_displayed) * k;
    if (target - displayed).abs() < SCROLL_SNAP_EPSILON {
        displayed = target;
    }
    let own_scroll = displayed;

    // Wrap the input_text_multiline in our own outer child window with
    // HORIZONTAL_SCROLLBAR enabled. The inner multiline is sized to
    // the content's max line width so its own internal caret-tracking
    // scroll never kicks in; the outer child is what the user actually
    // scrolls horizontally (shift+wheel, scrollbar drag). Imgui handles
    // it natively — we just read scroll_x back for overlay alignment.
    let max_line_w = match side {
        Side::Left => state.a_max_line_w,
        Side::Right => state.b_max_line_w,
    };
    let style = ui.clone_style();
    let inner_w = (max_line_w + style.frame_padding[0] * 2.0 + 8.0).max(widget_w);

    ui.set_cursor_screen_pos(widget_pos);

    let widget_id = format!("##diffie_pane_{:?}_e{}", side, state.input_epoch);
    let outer_id = format!("##diffie_pane_outer_{:?}_e{}", side, state.input_epoch);

    let caret_byte: Cell<i32> = Cell::new(-1);
    let scroll_x_cell: Cell<f32> = Cell::new(match side {
        Side::Left => state.last_left_scroll_x,
        Side::Right => state.last_right_scroll_x,
    });
    let widget_active_cell: Cell<bool> = Cell::new(false);
    let new_buf_cell: Cell<Option<String>> = Cell::new(None);

    // Outer child uses zero window padding so the inner multiline's own
    // frame_padding is the only inset we have to account for in the
    // overlay painter.
    // Compute next horizontal scroll for the OUTER child and pin it.
    // Imgui clamps to [0, ScrollMax.x] internally; the post-clamp value
    // is read back via igGetScrollX inside the closure.
    let char_step_x = ui.calc_text_size("m")[0].max(1.0);
    let prev_scroll_x = match side {
        Side::Left => state.last_left_scroll_x,
        Side::Right => state.last_right_scroll_x,
    };
    let target_scroll_x =
        (prev_scroll_x - h_wheel * char_step_x * SCROLL_LINES_PER_WHEEL_TICK).max(0.0);
    unsafe {
        imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
            x: target_scroll_x,
            y: -1.0,
        });
    }

    let _wp = ui.push_style_var(imgui::StyleVar::WindowPadding([0.0, 0.0]));
    {
        let buf: &mut String = match side {
            Side::Left => &mut state.a_buf,
            Side::Right => &mut state.b_buf,
        };
        ui.child_window(&outer_id)
            .size([widget_w, pane_h])
            .horizontal_scrollbar(true)
            .build(|| {
                // Pin the inner multiline's vertical scroll to our eased
                // value. `-1` for x means "leave alone" — the inner's
                // scroll_x stays at 0 because the inner is sized to the
                // full content width.
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
                        x: -1.0,
                        y: own_scroll,
                    });
                }

                // Suppress imgui's own FrameBg + Text rendering. We paint
                // everything ourselves on the foreground draw list.
                let _frame_bg = ui.push_style_color(imgui::StyleColor::FrameBg, [0.0, 0.0, 0.0, 0.0]);
                let _frame_bg_hov = ui.push_style_color(imgui::StyleColor::FrameBgHovered, [0.0, 0.0, 0.0, 0.0]);
                let _frame_bg_act = ui.push_style_color(imgui::StyleColor::FrameBgActive, [0.0, 0.0, 0.0, 0.0]);
                let _text_color = ui.push_style_color(imgui::StyleColor::Text, [0.0, 0.0, 0.0, 0.0]);

                struct CaretCapture<'a> {
                    cursor: &'a Cell<i32>,
                }
                impl<'a> imgui::InputTextCallbackHandler for CaretCapture<'a> {
                    fn on_always(&mut self, data: imgui::TextCallbackData) {
                        self.cursor.set(data.cursor_pos() as i32);
                    }
                }

                let changed = ui
                    .input_text_multiline(&widget_id, buf, [inner_w, pane_h])
                    .no_undo_redo(true)
                    .callback(
                        imgui::InputTextMultilineCallback::ALWAYS,
                        CaretCapture { cursor: &caret_byte },
                    )
                    .build();
                ui.set_item_allow_overlap();
                widget_active_cell.set(ui.is_item_active());
                if changed {
                    new_buf_cell.set(Some(buf.clone()));
                }

                // Read the outer child's horizontal scroll now that the
                // multiline's internal BeginChildFrame/EndChildFrame have
                // run — the current window is the outer child again.
                unsafe {
                    scroll_x_cell.set(imgui::sys::igGetScrollX());
                }
            });
    }
    drop(_wp);

    let widget_active = widget_active_cell.get();
    let buf_clone = new_buf_cell.take();
    let scroll_y_out = own_scroll;
    let scroll_x_out = scroll_x_cell.get();

    // Caret-tracking horizontal scroll: only fire when the caret
    // actually moved this frame (typing, arrows, paste, …). If the
    // user wheel-scrolled away while the caret sat still, we'd
    // otherwise pull the view straight back to it every frame.
    let cur_caret = caret_byte.get();
    let prev_caret = match side {
        Side::Left => state.a_last_caret,
        Side::Right => state.b_last_caret,
    };
    let caret_moved = widget_active && cur_caret >= 0 && prev_caret != Some(cur_caret);
    let next_scroll_x = if caret_moved {
        let buf_ref: &str = match side {
            Side::Left => &state.a_buf,
            Side::Right => &state.b_buf,
        };
        let caret_x =
            caret_x_in_inner(buf_ref, cur_caret as usize, ui, style.frame_padding[0]);
        let char_step = ui.calc_text_size("m")[0].max(1.0);
        track_caret_scroll_x(caret_x, scroll_x_out, widget_w, char_step * 2.0)
    } else {
        scroll_x_out
    };
    let new_last_caret = if widget_active && cur_caret >= 0 {
        Some(cur_caret)
    } else {
        None
    };
    match side {
        Side::Left => {
            state.last_left_scroll_x = next_scroll_x;
            state.a_last_caret = new_last_caret;
        }
        Side::Right => {
            state.last_right_scroll_x = next_scroll_x;
            state.b_last_caret = new_last_caret;
        }
    }
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
        scroll_x_out,
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
    );

    (widget_rect, scroll_y_out)
}
