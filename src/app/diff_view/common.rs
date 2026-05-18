//! 2-way diff view — shared types, constants, and helpers.
//!
//! State, geometry, and pure utility functions used by `mod.rs` and
//! the (currently empty) `overlay.rs`.

/// Tall enough for the 1.5x Roboto Mono used in code rows at zoom=1.0.
pub(super) const ROW_H_BASE: f32 = 24.0;
/// Width of the line-number gutter, sized for ~4 digits in the code-row mono.
pub(super) const GUTTER_W_BASE: f32 = 60.0;
/// Width of each anchor rail inside the connector strip.
pub(super) const RAIL_W_BASE: f32 = 18.0;

pub(super) const CONNECTOR_W: f32 = 60.0;

pub(super) fn rail_w() -> f32 {
    RAIL_W_BASE * crate::app::code_font_zoom()
}

/// Deprecated: use `ui.text_line_height()` inside the mono font scope.
/// Kept for any callers we missed.
#[allow(dead_code)]
pub(super) fn line_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

pub(super) fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}

/// Jump-to-paired-half request, set by the `↕` button in the (future)
/// hover overlay and consumed on the next frame's pane render.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingJump {
    pub(super) session_id: crate::session::SessionId,
    pub(super) pane: Side,
    pub(super) target_line: crate::diff::LineNo,
}

/// Per-session view state that must persist across frames.
#[derive(Default)]
pub struct DiffViewState {
    /// Buffer mirror of `session.a_text`. Synced at start of every
    /// render; written-back on every `input_text_multiline` change.
    pub(super) a_buf: String,
    pub(super) b_buf: String,
    /// Last *displayed* scroll_y per pane — the eased value pushed to imgui
    /// last frame. Returned to overlay paint code so highlights/caret align
    /// with what the user sees.
    pub(super) last_left_scroll_y: f32,
    pub(super) last_right_scroll_y: f32,
    /// Where each pane is scrolling toward. Wheel input, pending sync, and
    /// jumps mutate the target; the displayed scroll eases toward it each
    /// frame. Used by the sync detector — comparing targets (not eased
    /// displayed) keeps a single sync event from being mistaken for ongoing
    /// scroll feedback.
    pub(super) target_left_scroll: f32,
    pub(super) target_right_scroll: f32,
    /// Outer-scroll-window horizontal scroll position per pane, captured
    /// each frame from imgui via `igGetScrollX`. We wrap the
    /// `input_text_multiline` in our own child window that has the
    /// horizontal scrollbar enabled; imgui handles user scrolling
    /// natively, we just mirror the value so the overlay painter can
    /// subtract it to keep highlights aligned with the rendered text.
    pub(super) last_left_scroll_x: f32,
    pub(super) last_right_scroll_x: f32,
    /// Maximum line pixel width per pane in the active mono font.
    /// Cached so we only re-measure on buffer change — the outer scroll
    /// child uses this to size the inner `input_text_multiline` wide
    /// enough that imgui's own caret-tracking horizontal scroll never
    /// triggers and the outer scrollbar gets the full content width.
    pub(super) a_max_line_w: f32,
    pub(super) b_max_line_w: f32,
    /// Last observed caret byte position per pane. Used to detect caret
    /// movement (typing, arrows, paste, click-to-new-position, …) so
    /// caret-tracking horizontal scroll only fires on actual movement,
    /// not on every frame the widget is focused — otherwise the user
    /// can't scroll away from the caret with the wheel.
    pub(super) a_last_caret: Option<i32>,
    pub(super) b_last_caret: Option<i32>,
    /// Pending scroll set by sync; consumed on next render via
    /// `igSetNextWindowScroll`.
    pub(super) pending_left_scroll: Option<f32>,
    pub(super) pending_right_scroll: Option<f32>,
    /// Line number (1-based) to focus on the first frame this view renders.
    /// Set by the open path to the first non-equal hunk's start on this side
    /// so the user lands at the first difference rather than at the top of
    /// the file. Resolved into a pending scroll inside `render_pane` once
    /// `lh` is known, then cleared.
    pub(crate) pending_initial_a_line: Option<u32>,
    pub(crate) pending_initial_b_line: Option<u32>,
    /// Live state of the hover-to-anchor interaction.
    pub(super) anchor_pick: AnchorPick,
    /// Jump-to-pair target consumed on the next render.
    pub(super) pending_jump: Option<PendingJump>,
    /// Bumped on external buffer mutations (undo/redo, Apply A->B/B->A)
    /// and mixed into the widget ID so imgui re-initialises its
    /// stb_textedit state from `buf` instead of writing stale internal
    /// bytes back.
    pub input_epoch: u32,
    /// Active drag offset for the custom vertical scrollbar thumb. `Some(off)`
    /// means the user is mid-drag — `off` is the pixel distance from the thumb
    /// top to the mouse cursor at drag start, preserved so the cursor stays
    /// glued to the same spot on the thumb. The inner multiline's own
    /// scrollbar is hidden because it sits past the horizontally-scrolling
    /// viewport's right edge; we paint our own at the outer's fixed right edge.
    pub(super) left_vbar_drag: Option<f32>,
    pub(super) right_vbar_drag: Option<f32>,
}

/// Width of the custom vertical scrollbar painted on the right edge of each pane.
pub(crate) const VBAR_W: f32 = 12.0;
/// Minimum thumb height so the grab target stays usable on very long files.
pub(crate) const VBAR_THUMB_MIN_H: f32 = 24.0;

impl DiffViewState {
    /// True when either pane's eased scroll hasn't yet reached its target —
    /// the event loop uses this to keep redrawing while the animation runs.
    pub fn is_animating(&self) -> bool {
        const EPS: f32 = 0.5;
        (self.target_left_scroll - self.last_left_scroll_y).abs() > EPS
            || (self.target_right_scroll - self.last_right_scroll_y).abs() > EPS
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    #[allow(dead_code)]
    pub fn as_focused_pane(self) -> crate::app::FocusedPane {
        match self {
            Side::Left => crate::app::FocusedPane::TwoWayA,
            Side::Right => crate::app::FocusedPane::TwoWayB,
        }
    }
}

// ---------------------------------------------------------------------------
// Scroll-sync helpers
// ---------------------------------------------------------------------------

/// Build per-hunk (id, top_y, bot_y) ranges in content space for one pane.
pub(super) fn build_pane_ranges(
    hunks: &[crate::diff::Hunk],
    side: Side,
    lh: f32,
) -> Vec<(u32, f32, f32)> {
    hunks
        .iter()
        .filter_map(|h| {
            let (lo, hi) = match side {
                Side::Left => h.a_range,
                Side::Right => h.b_range,
            };
            if lo == 0 || hi == 0 {
                return None;
            }
            Some((h.id, (lo as f32 - 1.0) * lh, hi as f32 * lh))
        })
        .collect()
}

/// Given the source pane's current scroll, compute the scroll value the
/// destination pane should be set to so that the same hunk is centred.
pub(super) fn target_scroll(
    src_scroll: f32,
    src_view_h: f32,
    dst_view_h: f32,
    src_ranges: &[(u32, f32, f32)],
    dst_ranges: &[(u32, f32, f32)],
) -> Option<f32> {
    let center = src_scroll + src_view_h * 0.5;
    let (hunk_id, fraction) = locate_hunk(src_ranges, center)?;
    let (_id, top, bot) = dst_ranges.iter().find(|r| r.0 == hunk_id)?;
    let dst_center = top + fraction * (bot - top);
    Some((dst_center - dst_view_h * 0.5).max(0.0))
}

fn locate_hunk(ranges: &[(u32, f32, f32)], y: f32) -> Option<(u32, f32)> {
    let mut best: Option<(u32, f32, f32)> = None;
    for r in ranges {
        if r.1 <= y && y < r.2 {
            let span = (r.2 - r.1).max(1.0);
            return Some((r.0, ((y - r.1) / span).clamp(0.0, 1.0)));
        }
        if r.1 <= y && best.map_or(true, |b| r.2 > b.2) {
            best = Some(*r);
        }
    }
    best.map(|b| {
        let span = (b.2 - b.1).max(1.0);
        (b.0, ((y - b.1) / span).clamp(0.0, 1.0))
    })
}

// ---------------------------------------------------------------------------
// Hover-to-anchor state machine
// ---------------------------------------------------------------------------

/// Which icon — if any — the mouse is over on a given rail, or which icon was
/// just clicked. `line` is 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RailClick {
    pub side: Side,
    pub line: crate::diff::LineNo,
    /// If `Some(idx)`, the index of the anchor in `session.anchors` that
    /// includes this line. `None` if this line is not yet anchored.
    pub anchor_idx: Option<usize>,
}

/// Live interaction state for hover-to-anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AnchorPick {
    #[default]
    Idle,
    /// User clicked an unanchored icon and is now dragging.
    Picking { side: Side, line: crate::diff::LineNo },
}

/// One frame's worth of input that can affect `AnchorPick`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailEvent {
    /// Mouse-down on a rail icon at `click`.
    Click(RailClick),
    /// Mouse-down somewhere that is NOT a rail icon (anywhere inside the
    /// diff view), and not on a hunk hover panel.
    ClickedElsewhere,
    /// Escape key pressed this frame.
    Escape,
    /// Nothing this frame.
    None,
}

/// Side-effect to perform after a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailAction {
    /// No store mutation.
    None,
    /// Remove `session.anchors[idx]`.
    RemoveAnchor { idx: usize },
    /// Insert a new `Anchor { a, b }`. `a` is the LEFT-side line, `b` the right.
    AddAnchor { a: crate::diff::LineNo, b: crate::diff::LineNo },
}

/// Pure state-machine step. Returns the next pick state and the side-effect
/// (if any) the caller must apply to `SessionStore`. Encodes the full table
/// from the spec — including the "click anchored target on opposite side
/// during Picking" no-op and the same-side source replace.
pub(super) fn next_anchor_pick(current: AnchorPick, event: RailEvent) -> (AnchorPick, RailAction) {
    use AnchorPick::*;
    match (current, event) {
        (_, RailEvent::None) => (current, RailAction::None),

        // Cancel paths.
        (Picking { .. }, RailEvent::Escape) => (Idle, RailAction::None),
        (Picking { .. }, RailEvent::ClickedElsewhere) => (Idle, RailAction::None),
        (Idle, RailEvent::Escape) => (Idle, RailAction::None),
        (Idle, RailEvent::ClickedElsewhere) => (Idle, RailAction::None),

        // Idle + click.
        (Idle, RailEvent::Click(c)) => match c.anchor_idx {
            Some(idx) => (Idle, RailAction::RemoveAnchor { idx }),
            None => (Picking { side: c.side, line: c.line }, RailAction::None),
        }

        // Picking + click on same-side icon: move source.
        (Picking { side, .. }, RailEvent::Click(c)) if c.side == side => {
            (Picking { side: c.side, line: c.line }, RailAction::None)
        }

        // Picking + click on opposite-side icon.
        (Picking { side: src_side, line: src_line }, RailEvent::Click(c)) => match c.anchor_idx {
            Some(_) => {
                // Ambiguous — keep dragging. User must remove first.
                (Picking { side: src_side, line: src_line }, RailAction::None)
            }
            None => {
                let (a, b) = match src_side {
                    Side::Left => (src_line, c.line),
                    Side::Right => (c.line, src_line),
                };
                (Idle, RailAction::AddAnchor { a, b })
            }
        }
    }
}
