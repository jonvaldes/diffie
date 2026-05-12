//! Plain-data snapshot of per-frame input state, plus pure functions that
//! consume it. The `from_ui` adapter lives in `input_imgui.rs` so this file
//! has no imgui dependency and is testable without the `gui` feature.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseButtons {
    pub left_down: bool,
    pub left_clicked: bool,
    pub right_clicked: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_: bool,
}

/// One frame's worth of input. Everything the view logic needs to make
/// decisions, with nothing it doesn't. Coordinates are in imgui screen
/// space (pixels, origin top-left of the OS window).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InputFrame {
    pub mouse_pos: [f32; 2],
    pub mouse_buttons: MouseButtons,
    pub modifiers: Modifiers,
    /// Vertical wheel delta in line units (already normalized from pixel
    /// deltas by the winit handler in `app::mod`).
    pub wheel_v: f32,
    pub wheel_h: f32,
}

/// Read-only view of one diff pane's geometry, in imgui screen space.
/// Sides are independent: pass one of these per side when hit-testing.
#[derive(Debug, Clone, Copy)]
pub struct PaneLayout {
    pub origin: [f32; 2],
    pub width: f32,
    pub visible_height: f32,
    pub gutter_width: f32,
    pub row_height: f32,
    pub char_width: f32,
    /// Number of rendered rows in the pane this frame.
    pub row_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneHit {
    pub side: Side,
    /// 0-based row index into the pane's row list.
    pub row: u32,
    /// 0-based column in characters, clamped to `[0, line_char_count]`.
    pub col: u32,
}

/// True iff `pos` lies inside the pane's content rect.
pub fn pane_contains(layout: &PaneLayout, pos: [f32; 2]) -> bool {
    let dx = pos[0] - layout.origin[0];
    let dy = pos[1] - layout.origin[1];
    dx >= 0.0 && dx < layout.width && dy >= 0.0 && dy < layout.visible_height
}

/// Hit-test `pos` against one pane. Caller supplies `line_char_count` for
/// the row that gets hit so we can clamp the column. Returns `None` if
/// `pos` is outside the pane or hits a row that exists only as padding
/// (caller signals that by returning `None` from `line_char_count`).
pub fn hit_test_pane(
    side: Side,
    layout: &PaneLayout,
    pos: [f32; 2],
    line_char_count: impl FnOnce(u32) -> Option<u32>,
) -> Option<PaneHit> {
    if !pane_contains(layout, pos) {
        return None;
    }
    let dy = pos[1] - layout.origin[1];
    let row = (dy / layout.row_height) as u32;
    if row >= layout.row_count {
        return None;
    }
    let char_count = line_char_count(row)?;
    let text_x0 = layout.origin[0] + layout.gutter_width;
    let raw = ((pos[0] - text_x0) / layout.char_width.max(1.0)).round();
    let col = raw.clamp(0.0, char_count as f32) as u32;
    Some(PaneHit { side, row, col })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_input_frame_is_neutral() {
        let f = InputFrame::default();
        assert_eq!(f.mouse_pos, [0.0, 0.0]);
        assert!(!f.mouse_buttons.left_down);
        assert!(!f.mouse_buttons.left_clicked);
        assert!(!f.mouse_buttons.right_clicked);
        assert!(!f.modifiers.shift);
        assert_eq!(f.wheel_v, 0.0);
    }
}

#[cfg(test)]
mod hit_tests {
    use super::*;

    fn layout() -> PaneLayout {
        PaneLayout {
            origin: [100.0, 50.0],
            width: 400.0,
            visible_height: 200.0,
            gutter_width: 40.0,
            row_height: 16.0,
            char_width: 8.0,
            row_count: 10,
        }
    }

    #[test]
    fn outside_pane_returns_none() {
        let l = layout();
        assert!(hit_test_pane(Side::Left, &l, [50.0, 60.0], |_| Some(10)).is_none());
        assert!(hit_test_pane(Side::Left, &l, [600.0, 60.0], |_| Some(10)).is_none());
        assert!(hit_test_pane(Side::Left, &l, [200.0, 10.0], |_| Some(10)).is_none());
        assert!(hit_test_pane(Side::Left, &l, [200.0, 300.0], |_| Some(10)).is_none());
    }

    #[test]
    fn hit_in_first_row_first_column() {
        let l = layout();
        let hit = hit_test_pane(Side::Left, &l, [140.0, 50.0], |_| Some(20)).unwrap();
        assert_eq!(hit.row, 0);
        assert_eq!(hit.col, 0);
        assert_eq!(hit.side, Side::Left);
    }

    #[test]
    fn column_clamped_to_line_length() {
        let l = layout();
        let hit = hit_test_pane(Side::Right, &l, [490.0, 50.0], |_| Some(5)).unwrap();
        assert_eq!(hit.col, 5);
    }

    #[test]
    fn padding_row_returns_none() {
        let l = layout();
        assert!(hit_test_pane(Side::Left, &l, [200.0, 50.0 + 16.0 * 2.0], |_| None).is_none());
    }

    #[test]
    fn row_index_past_row_count_returns_none() {
        let mut l = layout();
        l.row_count = 3;
        assert!(hit_test_pane(Side::Left, &l, [200.0, 130.0], |_| Some(10)).is_none());
    }
}
