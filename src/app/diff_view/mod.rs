//! 2-way diff view: two `input_text_multiline` widgets side-by-side
//! with a (currently empty) bezier connector strip in the middle.
//!
//! This is the post-Task-5 skeleton: text renders + edits, but no
//! per-row decorations, hover overlay, anchor gutter, or sub-line
//! spans. Those come back in tasks 6-8.

use std::cell::Cell;

use imgui::{FontId, Ui};

use crate::diff::{Anchor, DiffOp, Hunk};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};

mod common;
mod overlay;

#[cfg(test)]
mod tests;

pub use common::{DiffViewState, Side};
pub(crate) use common::VBAR_W;
use common::{
    build_pane_ranges, gutter_w, next_anchor_pick, rail_w, target_scroll, AnchorPick,
    PendingJump, RailAction, RailClick, RailEvent, CONNECTOR_W, VBAR_THUMB_MIN_H,
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
/// Vertical-scrollbar thumb geometry in screen space. Returns `(thumb_top_y,
/// thumb_h)`. `content_h <= track_h` is the no-overflow case — the caller should
/// suppress painting entirely; we still return a sensible fallback for safety.
pub(crate) fn vbar_thumb_geom(track_top: f32, track_h: f32, scroll_y: f32, content_h: f32) -> (f32, f32) {
    if content_h <= track_h || track_h <= 0.0 {
        return (track_top, track_h.max(0.0));
    }
    let ratio = (track_h / content_h).clamp(0.0, 1.0);
    let h = (track_h * ratio).max(VBAR_THUMB_MIN_H).min(track_h);
    let max_scroll = (content_h - track_h).max(1.0);
    let y = track_top + (scroll_y / max_scroll).clamp(0.0, 1.0) * (track_h - h);
    (y, h)
}

/// Invert `vbar_thumb_geom` for a desired thumb top: returns the scroll_y that
/// would place the thumb there, clamped to `[0, max_scroll]`.
pub(crate) fn vbar_scroll_for_thumb_y(
    desired_thumb_top: f32,
    track_top: f32,
    track_h: f32,
    thumb_h: f32,
    content_h: f32,
) -> f32 {
    let avail = (track_h - thumb_h).max(1.0);
    let max_scroll = (content_h - track_h).max(0.0);
    let frac = ((desired_thumb_top - track_top) / avail).clamp(0.0, 1.0);
    frac * max_scroll
}

pub(crate) fn track_caret_scroll_x(caret_x: f32, scroll_x: f32, view_w: f32, margin: f32) -> f32 {
    if caret_x < scroll_x + margin {
        (caret_x - margin).max(0.0)
    } else if caret_x > scroll_x + view_w - margin {
        caret_x - view_w + margin
    } else {
        scroll_x
    }
}

/// Walk the hunk list once and return the 1-based (a_line, b_line) of the
/// first non-equal hunk. For hunks that are empty on a side (pure insert /
/// pure delete), the missing side falls back to "the line right after the
/// preceding hunk's last line" so both sides land at the same visual row
/// when the panes scroll-sync. Returns `None` when the diff is all-equal.
pub fn first_change_lines(hunks: &[Hunk]) -> Option<(u32, u32)> {
    let mut prev_a_end: u32 = 0;
    let mut prev_b_end: u32 = 0;
    for h in hunks {
        let is_change = h
            .ops
            .iter()
            .any(|op| matches!(op, DiffOp::Delete { .. } | DiffOp::Insert { .. }));
        if is_change {
            let a = if h.a_range == (0, 0) { prev_a_end + 1 } else { h.a_range.0 };
            let b = if h.b_range == (0, 0) { prev_b_end + 1 } else { h.b_range.0 };
            return Some((a, b));
        }
        prev_a_end = h.a_range.1.max(prev_a_end);
        prev_b_end = h.b_range.1.max(prev_b_end);
    }
    None
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
                Ok(()) => {
                    *status = format!("anchor added: A:{a} <-> B:{b}");
                    let center = |line: u32| {
                        ((line as f32 - 1.0) * lh - pane_h * 0.5 + lh * 0.5).max(0.0)
                    };
                    state.pending_left_scroll = Some(center(a));
                    state.pending_right_scroll = Some(center(b));
                }
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
    // `paint_text_lines` offsets each row by the multiline's frame
    // padding.y; include it here so ribbons line up with the text rows
    // (and with the gutter tints) rather than sitting a few pixels above.
    let pane_text_padding_y = ui.clone_style().frame_padding[1];
    overlay::draw_connector(
        ui,
        connector_pos,
        CONNECTOR_W,
        pane_h,
        left_widget_rect[1] + pane_text_padding_y - left_scroll_y,
        right_widget_rect[1] + pane_text_padding_y - right_scroll_y,
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

    // Reserve the gutter strip in the layout. We paint its contents below
    // — *after* we've computed `own_scroll` for this frame — so the gutter
    // tracks the same scroll value the code pane uses. Painting it here
    // with `state.last_*_scroll_y` would leave the gutter one frame behind
    // whichever pane the user is wheel-scrolling.
    ui.set_cursor_screen_pos(pane_pos);
    ui.dummy([g_w, pane_h]); // gutter strip — display only, clicks handled by rails
    let gutter_rect = [pane_pos[0], pane_pos[1], pane_pos[0] + g_w, pane_pos[1] + pane_h];
    let buf_line_count_for_gutter = {
        let buf_ref: &str = match side {
            Side::Left => &state.a_buf,
            Side::Right => &state.b_buf,
        };
        (buf_ref.lines().count().max(1)) as u32
    };

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
    // First-frame focus on the opening file's first hunk: resolve the
    // stored line number to a scroll target now that `lh` is known. Wins
    // over any prior pending scroll on this side (there shouldn't be one
    // — the view was just created — but keep the intent explicit).
    let initial_line = match side {
        Side::Left => state.pending_initial_a_line.take(),
        Side::Right => state.pending_initial_b_line.take(),
    };
    let pending_scroll = if let Some(line) = initial_line {
        const TOP_MARGIN_LINES: f32 = 2.0;
        let y = ((line.max(1) - 1) as f32 - TOP_MARGIN_LINES).max(0.0) * lh;
        Some(y.min(max_scroll))
    } else {
        pending_scroll
    };
    let prev_target = match side {
        Side::Left => state.target_left_scroll,
        Side::Right => state.target_right_scroll,
    };
    let prev_displayed = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };

    // ----- custom vertical scrollbar: drag handling -----
    // The inner multiline's own vertical scrollbar lives at x = inner_pos +
    // inner_w, which sits past the right edge of the outer's viewport whenever
    // inner_w > widget_w — so it gets clipped (worst-case completely hidden at
    // scroll_x = 0). We hide it (ScrollbarSize=0 around the inner build below)
    // and paint our own thumb on `widget_rect`'s right edge instead.
    //
    // Thumb position uses *previous frame*'s displayed scroll (what the user
    // saw), which is also what we'll show again this frame unless drag fires.
    let track_top = widget_pos[1];
    let track_h = pane_h;
    let (prev_thumb_y, thumb_h) = vbar_thumb_geom(track_top, track_h, prev_displayed, content_h);
    let vbar_x_r = widget_pos[0] + widget_w;
    let vbar_x_l = vbar_x_r - VBAR_W;
    let mouse = ui.io().mouse_pos;
    let in_x = mouse[0] >= vbar_x_l && mouse[0] <= vbar_x_r;
    let in_thumb = in_x && mouse[1] >= prev_thumb_y && mouse[1] <= prev_thumb_y + thumb_h;
    let in_track = in_x && mouse[1] >= track_top && mouse[1] <= track_top + track_h;
    let mouse_down = ui.is_mouse_down(imgui::MouseButton::Left);
    let mouse_clicked = ui.is_mouse_clicked(imgui::MouseButton::Left);
    let drag_slot: &mut Option<f32> = match side {
        Side::Left => &mut state.left_vbar_drag,
        Side::Right => &mut state.right_vbar_drag,
    };
    let mut drag_override: Option<f32> = None;
    if content_h > track_h {
        if let Some(off) = *drag_slot {
            if mouse_down {
                let desired_top = mouse[1] - off;
                drag_override = Some(vbar_scroll_for_thumb_y(
                    desired_top, track_top, track_h, thumb_h, content_h,
                ));
            } else {
                *drag_slot = None;
            }
        } else if mouse_clicked && in_thumb {
            *drag_slot = Some(mouse[1] - prev_thumb_y);
        } else if mouse_clicked && in_track {
            // Page-jump: center the thumb on the click and start dragging from there.
            let off = thumb_h * 0.5;
            *drag_slot = Some(off);
            drag_override = Some(vbar_scroll_for_thumb_y(
                mouse[1] - off,
                track_top,
                track_h,
                thumb_h,
                content_h,
            ));
        }
    } else {
        *drag_slot = None;
    }
    // True for any frame the user is interacting with the custom scrollbar:
    // either a fresh click just landed on the track, or an existing drag is
    // in progress. Used below to suppress the multiline's mouse input.
    let scrollbar_grabbing = drag_slot.is_some() || (mouse_clicked && in_track);
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
    // Compute this frame's *target*. Drag wins over everything else (no easing
    // — we want the thumb to feel pinned to the cursor). Otherwise pending
    // (sync / jump) overrides; otherwise mouse wheel deflects from the
    // previous target so multiple wheel ticks before the easing catches up
    // still register fully.
    let mut target = if let Some(s) = drag_override {
        s
    } else {
        pending_scroll.unwrap_or_else(|| prev_target - wheel * lh * SCROLL_LINES_PER_WHEEL_TICK)
    };
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
    // Drag skips the easing. dt is clamped to ~one frame at 30 fps (0.033s)
    // rather than the raw delta — when the idle event loop is parked and a
    // wheel tick wakes us up, `io.delta_time` reports the full sleep
    // duration, which with SCROLL_SMOOTH_SPEED=25 collapses the first step
    // to k≈0.92 (an instant jump) and ruins the easing feel on every other
    // tick. Treating the first post-idle frame as a normal frame keeps the
    // easing visible and matches what the user expects after a wheel
    // event.
    let displayed = if drag_override.is_some() {
        target
    } else {
        let dt = ui.io().delta_time.max(0.0).min(0.033);
        let k = 1.0 - (-dt * SCROLL_SMOOTH_SPEED).exp();
        let mut d = prev_displayed + (target - prev_displayed) * k;
        if (target - d).abs() < SCROLL_SNAP_EPSILON {
            d = target;
        }
        d
    };
    let own_scroll = displayed;

    // Now that this frame's scroll is final, paint the gutter using it so
    // gutter rows stay locked to the code rows even while the eased scroll
    // is mid-animation.
    overlay::paint_gutter(
        ui,
        gutter_rect,
        anchors,
        side,
        hunks,
        own_scroll,
        lh,
        buf_line_count_for_gutter,
    );

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

                // Shrink the inner multiline's own vertical scrollbar to a
                // single pixel. It would otherwise be painted at x =
                // inner_pos + inner_w, which clips out when the outer is
                // scrolled left of max_x. Setting it to exactly 0 trips an
                // imgui assertion in GetWindowScrollbarRect (scrollbar_size
                // must be > 0); 1.0 satisfies it and the 1px strip lives at
                // inner_w's right edge — outside the outer's clip rect — so
                // it never shows. We paint our own thumb at the outer's
                // fixed right edge after this child closes.
                let _sb = ui.push_style_var(imgui::StyleVar::ScrollbarSize(1.0));

                // Disable the multiline entirely while the user is dragging
                // our custom scrollbar — otherwise the click/drag bleeds
                // through and starts a text selection (the multiline always
                // wins ActiveID over later-rendered items, regardless of
                // `set_item_allow_overlap`). BeginDisabled blocks all input
                // to subsequent items; visuals are unaffected here because
                // we paint text/caret ourselves on the foreground draw list.
                unsafe { imgui::sys::igBeginDisabled(scrollbar_grabbing) };
                let changed = ui
                    .input_text_multiline(&widget_id, buf, [inner_w, pane_h])
                    .no_undo_redo(true)
                    .callback(
                        imgui::InputTextMultilineCallback::ALWAYS,
                        CaretCapture { cursor: &caret_byte },
                    )
                    .build();
                unsafe { imgui::sys::igEndDisabled() };
                drop(_sb);
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

    // Custom vertical scrollbar — painted last so it sits above text. Pinned
    // to widget_rect's right edge (which doesn't move when the user scrolls
    // horizontally), so it never disappears off-screen the way the inner
    // multiline's native scrollbar did.
    let vbar_visible = content_h > pane_h;
    let vbar_active = drag_slot.is_some();
    let bands = build_minimap_bands(hunks, side);
    paint_vbar(
        ui,
        widget_rect,
        own_scroll,
        content_h,
        vbar_active,
        &bands,
        buf_line_count_for_gutter,
    );

    // Override imgui's auto-set TextInput (I-beam) cursor with Arrow when the
    // mouse is over the scrollbar or actively dragging it. set_mouse_cursor
    // is read at end-of-frame, so a late write wins.
    if vbar_visible && (in_track || vbar_active) {
        ui.set_mouse_cursor(Some(imgui::MouseCursor::Arrow));
    }

    (widget_rect, scroll_y_out)
}

/// One colored band on the scrollbar track. Acts as a "minimap" marker
/// pointing at a hunk's location in the file. Lines are 1-based inclusive.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MinimapBand {
    pub line_lo: u32,
    pub line_hi: u32,
    pub color: [f32; 4],
}

/// Build scrollbar-minimap bands for one side of a 2-way diff. Mirrors the
/// per-row tint convention: Left/A deletions paint red, Right/B insertions
/// paint green. Pure-equal hunks and hunks with an empty range on this side
/// (insert-only on the left, delete-only on the right) are skipped.
fn build_minimap_bands(hunks: &[Hunk], side: Side) -> Vec<MinimapBand> {
    let mut bands = Vec::new();
    for h in hunks {
        let range = match side {
            Side::Left => h.a_range,
            Side::Right => h.b_range,
        };
        if range == (0, 0) || range.1 < range.0 {
            continue;
        }
        let has_change = h
            .ops
            .iter()
            .any(|op| matches!(op, DiffOp::Delete { .. } | DiffOp::Insert { .. }));
        if !has_change {
            continue;
        }
        let color = match side {
            Side::Left => [0.55, 0.18, 0.18, 0.85],
            Side::Right => [0.18, 0.50, 0.22, 0.85],
        };
        bands.push(MinimapBand { line_lo: range.0, line_hi: range.1, color });
    }
    bands
}

pub(crate) fn paint_vbar(
    ui: &Ui,
    widget_rect: [f32; 4],
    scroll_y: f32,
    content_h: f32,
    active: bool,
    bands: &[MinimapBand],
    total_lines: u32,
) {
    let track_top = widget_rect[1];
    let track_bot = widget_rect[3];
    let track_h = track_bot - track_top;
    if content_h <= track_h || track_h <= 0.0 {
        return;
    }
    let x_r = widget_rect[2];
    let x_l = x_r - VBAR_W;
    let (ty, th) = vbar_thumb_geom(track_top, track_h, scroll_y, content_h);
    // Use the window's draw list (not the foreground one) so the scrollbar
    // participates in window stacking — modal popups like Preferences
    // correctly cover it instead of being drawn over.
    let dl = ui.get_window_draw_list();
    // Track — subtle, doesn't fight with the syntax-highlighted text behind.
    dl.add_rect_filled_multicolor(
        [x_l, track_top],
        [x_r, track_bot],
        [0.0, 0.0, 0.0, 0.18],
        [0.0, 0.0, 0.0, 0.18],
        [0.0, 0.0, 0.0, 0.18],
        [0.0, 0.0, 0.0, 0.18],
    );
    // Minimap bands. Map each band's line range onto the track. Enforce a
    // 2px minimum height so single-line hunks remain visible at any zoom.
    if total_lines > 0 {
        let lines_f = total_lines as f32;
        for b in bands {
            let lo = b.line_lo.saturating_sub(1) as f32;
            let hi = b.line_hi.min(total_lines) as f32;
            let mut y0 = track_top + (lo / lines_f) * track_h;
            let mut y1 = track_top + (hi / lines_f) * track_h;
            if y1 - y0 < 2.0 {
                let mid = 0.5 * (y0 + y1);
                y0 = mid - 1.0;
                y1 = mid + 1.0;
            }
            dl.add_rect([x_l, y0], [x_r, y1], b.color)
                .filled(true)
                .build();
        }
    }
    // Thumb — brighter when dragging.
    let thumb_a = if active { 0.85 } else { 0.55 };
    dl.add_rect([x_l + 2.0, ty + 1.0], [x_r - 2.0, ty + th - 1.0], [0.75, 0.75, 0.75, thumb_a])
        .filled(true)
        .rounding(3.0)
        .build();
}
