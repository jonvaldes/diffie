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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelPoint {
    pub line_no: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub side: Side,
    pub anchor: SelPoint,
    pub caret: SelPoint,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragState {
    pub side: Side,
    pub anchor: SelPoint,
    pub press_screen: [f32; 2],
    pub threshold_passed: bool,
}

/// Outcome of one frame's selection update. The view applies these to
/// its mutable state. `focus_request` mirrors the existing diff_view
/// behavior: a click in a pane requests keyboard focus for that side.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SelectionStep {
    pub set_selection: Option<Option<Selection>>,
    pub set_drag: Option<Option<DragState>>,
    pub focus_request: Option<Side>,
}

/// Pure selection state machine.
///
/// - `locate(pos)` maps a screen position to `(Side, SelPoint)` if it falls
///   inside one of the panes. Used for the initial press.
/// - `locate_clamped(side, pos)` maps a position to a `SelPoint` on the
///   *given* side, clamping coordinates that fall outside the pane to the
///   pane edges. Used during drag so dragging off the pane still extends
///   the selection to the nearest row/col. Returns `None` only if the side
///   has no rows at all.
pub fn selection_step(
    frame: &InputFrame,
    selection: Option<Selection>,
    drag: Option<DragState>,
    locate: impl Fn([f32; 2]) -> Option<(Side, SelPoint)>,
    locate_clamped: impl Fn(Side, [f32; 2]) -> Option<SelPoint>,
) -> SelectionStep {
    const DRAG_THRESHOLD_PX: f32 = 4.0;
    let mut step = SelectionStep::default();

    if frame.mouse_buttons.left_clicked {
        let press = frame.mouse_pos;
        match locate(press) {
            Some((side, point)) => {
                let extend = frame.modifiers.shift
                    && selection.as_ref().map_or(false, |s| s.side == side);
                if extend {
                    let mut sel = selection.unwrap();
                    sel.caret = point;
                    step.set_selection = Some(Some(sel));
                    step.set_drag = Some(Some(DragState {
                        side,
                        anchor: sel.anchor,
                        press_screen: press,
                        threshold_passed: true,
                    }));
                } else {
                    step.set_selection = Some(None);
                    step.set_drag = Some(Some(DragState {
                        side,
                        anchor: point,
                        press_screen: press,
                        threshold_passed: false,
                    }));
                }
                step.focus_request = Some(side);
            }
            None => {
                step.set_selection = Some(None);
                step.set_drag = Some(None);
            }
        }
        return step;
    }

    if let Some(mut d) = drag {
        if !frame.mouse_buttons.left_down {
            step.set_drag = Some(None);
            return step;
        }
        let pos = frame.mouse_pos;
        if !d.threshold_passed {
            let dx = pos[0] - d.press_screen[0];
            let dy = pos[1] - d.press_screen[1];
            if (dx * dx + dy * dy).sqrt() >= DRAG_THRESHOLD_PX {
                d.threshold_passed = true;
            }
        }
        if d.threshold_passed {
            if let Some(point) = locate_clamped(d.side, pos) {
                step.set_selection = Some(Some(Selection {
                    side: d.side,
                    anchor: d.anchor,
                    caret: point,
                }));
            }
        }
        step.set_drag = Some(Some(d));
    }

    step
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn frame(pos: [f32; 2], clicked: bool, down: bool, shift: bool) -> InputFrame {
        InputFrame {
            mouse_pos: pos,
            mouse_buttons: MouseButtons {
                left_clicked: clicked,
                left_down: down,
                right_clicked: false,
            },
            modifiers: Modifiers { shift, ..Default::default() },
            ..Default::default()
        }
    }

    fn locate_left(p: [f32; 2]) -> Option<(Side, SelPoint)> {
        if p[0] >= 0.0 && p[0] < 200.0 && p[1] >= 0.0 && p[1] < 100.0 {
            Some((Side::Left, SelPoint {
                line_no: (p[1] as u32 / 10) + 1,
                col: (p[0] as u32 / 8),
            }))
        } else {
            None
        }
    }

    fn locate_clamped_left(side: Side, p: [f32; 2]) -> Option<SelPoint> {
        if side != Side::Left { return None; }
        let cx = p[0].clamp(0.0, 199.0);
        let cy = p[1].clamp(0.0, 99.0);
        Some(SelPoint {
            line_no: (cy as u32 / 10) + 1,
            col: (cx as u32 / 8),
        })
    }

    #[test]
    fn click_outside_pane_clears_selection_and_drag() {
        let f = frame([500.0, 500.0], true, true, false);
        let prior = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });
        let step = selection_step(&f, prior, None, locate_left, locate_clamped_left);
        assert_eq!(step.set_selection, Some(None));
        assert_eq!(step.set_drag, Some(None));
        assert!(step.focus_request.is_none());
    }

    #[test]
    fn click_in_pane_starts_drag_unset_threshold() {
        let f = frame([16.0, 10.0], true, true, false);
        let step = selection_step(&f, None, None, locate_left, locate_clamped_left);
        assert_eq!(step.set_selection, Some(None));
        assert_eq!(step.focus_request, Some(Side::Left));
        let d = step.set_drag.unwrap().unwrap();
        assert_eq!(d.side, Side::Left);
        assert_eq!(d.anchor, SelPoint { line_no: 2, col: 2 });
        assert!(!d.threshold_passed);
    }

    #[test]
    fn shift_click_with_prior_selection_same_side_extends() {
        let prior = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 0 },
        });
        let f = frame([24.0, 30.0], true, true, true);
        let step = selection_step(&f, prior, None, locate_left, locate_clamped_left);
        let sel = step.set_selection.unwrap().unwrap();
        assert_eq!(sel.anchor, SelPoint { line_no: 1, col: 0 });
        assert_eq!(sel.caret, SelPoint { line_no: 4, col: 3 });
        let d = step.set_drag.unwrap().unwrap();
        assert!(d.threshold_passed);
    }

    #[test]
    fn shift_click_without_prior_selection_acts_like_plain_click() {
        let f = frame([24.0, 30.0], true, true, true);
        let step = selection_step(&f, None, None, locate_left, locate_clamped_left);
        assert_eq!(step.set_selection, Some(None));
        let d = step.set_drag.unwrap().unwrap();
        assert!(!d.threshold_passed);
    }

    #[test]
    fn release_during_drag_clears_drag() {
        let prior_drag = Some(DragState {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            press_screen: [10.0, 10.0],
            threshold_passed: true,
        });
        let f = frame([10.0, 10.0], false, false, false);
        let step = selection_step(&f, None, prior_drag, locate_left, locate_clamped_left);
        assert_eq!(step.set_drag, Some(None));
    }

    #[test]
    fn drag_below_threshold_does_not_move_caret() {
        let prior_drag = Some(DragState {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            press_screen: [10.0, 10.0],
            threshold_passed: false,
        });
        let f = frame([12.0, 12.0], false, true, false);
        let step = selection_step(&f, None, prior_drag, locate_left, locate_clamped_left);
        assert!(step.set_selection.is_none());
        let d = step.set_drag.unwrap().unwrap();
        assert!(!d.threshold_passed);
    }

    #[test]
    fn drag_past_threshold_extends_selection() {
        let prior_drag = Some(DragState {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            press_screen: [10.0, 10.0],
            threshold_passed: false,
        });
        let f = frame([40.0, 40.0], false, true, false);
        let step = selection_step(&f, None, prior_drag, locate_left, locate_clamped_left);
        let d = step.set_drag.unwrap().unwrap();
        assert!(d.threshold_passed);
        let sel = step.set_selection.unwrap().unwrap();
        assert_eq!(sel.anchor, SelPoint { line_no: 1, col: 0 });
        assert_eq!(sel.caret, SelPoint { line_no: 5, col: 5 });
    }

    #[test]
    fn drag_outside_pane_clamps_via_clamped_locate() {
        let prior_drag = Some(DragState {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            press_screen: [10.0, 10.0],
            threshold_passed: true,
        });
        let f = frame([500.0, 500.0], false, true, false);
        let step = selection_step(&f, None, prior_drag, locate_left, locate_clamped_left);
        let sel = step.set_selection.unwrap().unwrap();
        assert_eq!(sel.side, Side::Left);
        assert_eq!(sel.anchor, SelPoint { line_no: 1, col: 0 });
        assert_eq!(sel.caret, SelPoint { line_no: 10, col: 24 });
    }
}
