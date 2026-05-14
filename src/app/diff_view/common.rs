//! 2-way diff view — shared types, constants, and helpers.
//!
//! State, geometry, and pure utility functions used by `mod.rs` and
//! the (currently empty) `overlay.rs`.

/// Tall enough for the 1.5x Roboto Mono used in code rows at zoom=1.0.
pub(super) const ROW_H_BASE: f32 = 24.0;
/// Width of the line-number gutter, sized for ~4 digits in the code-row mono.
pub(super) const GUTTER_W_BASE: f32 = 60.0;

pub(super) const CONNECTOR_W: f32 = 60.0;

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

/// Brief peach flash painted on top of a hunk's rows for a few frames
/// after the user arrives via the `↕` jump button.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(super) struct MoveFlash {
    pub(super) session_id: crate::session::SessionId,
    pub(super) hunk_id: u32,
    pub(super) frames_remaining: u8,
}

#[allow(dead_code)]
pub(super) const MOVE_FLASH_FRAMES: u8 = 30;
#[allow(dead_code)]
pub(super) const MOVE_FLASH_PEAK_ALPHA: f32 = 0.20;

/// Per-session view state that must persist across frames.
#[derive(Default)]
pub struct DiffViewState {
    /// Buffer mirror of `session.a_text`. Synced at start of every
    /// render; written-back on every `input_text_multiline` change.
    pub(super) a_buf: String,
    pub(super) b_buf: String,
    /// Last scroll_y per pane (for sync math).
    pub(super) last_left_scroll_y: f32,
    pub(super) last_right_scroll_y: f32,
    /// Pending scroll set by sync; consumed on next render via
    /// `igSetNextWindowScroll`.
    pub(super) pending_left_scroll: Option<f32>,
    pub(super) pending_right_scroll: Option<f32>,
    /// Last scroll_x per pane (test harness reads these).
    pub last_left_scroll_x: f32,
    pub last_right_scroll_x: f32,
    /// Two-click anchor creation: line picked on side A awaiting partner on B.
    pub(super) pending_a: Option<u32>,
    pub(super) pending_b: Option<u32>,
    /// Jump-to-pair and arrival flash (unchanged).
    pub(super) pending_jump: Option<PendingJump>,
    pub(super) flash: Option<MoveFlash>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn as_focused_pane(self) -> crate::app::FocusedPane {
        match self {
            Side::Left => crate::app::FocusedPane::TwoWayA,
            Side::Right => crate::app::FocusedPane::TwoWayB,
        }
    }
}

/// TODO(task 11): re-implement once selection state is reintroduced on
/// the multiline widget. For now this is a no-op stub so the
/// app-level Copy plumbing still compiles.
#[allow(dead_code)]
pub fn extract_selection_text(_snap: &crate::session::DiffSession) -> String {
    String::new()
}
