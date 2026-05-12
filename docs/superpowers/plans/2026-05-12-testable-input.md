# Testable Input Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make GUI input handling unit-testable by extracting the input-reactive logic out of per-frame imgui draw code into pure functions driven by a plain-data `InputFrame`, with the drag-selection state machine in `diff_view` as the first slice.

**Architecture:** Today, every interactive behavior in `src/app/diff_view.rs` and `src/app/merge_view.rs` is computed inline during the imgui draw pass via `ui.io()` / `ui.is_mouse_*` / `ui.is_key_*` polls. There is no seam where a test can stand in for imgui. We introduce one. A new `src/app/input.rs` module (a) defines an `InputFrame` plain-data snapshot of the imgui inputs the views consume each frame, and (b) provides hit-testing + state-machine functions that take `InputFrame` + view state + layout rects and return `Option<StateMutation>`. The imgui draw code builds an `InputFrame` once at the top of the frame via `InputFrame::from_ui(&Ui)` and feeds it to those functions; tests construct `InputFrame` values by hand. The pure layer has no `imgui` dependency, so its tests run under `--no-default-features --lib`. The first slice migrates `update_selection` (lines ~875-1010 of `diff_view.rs`): mouse-driven anchor/caret/drag selection. Once that pattern lands, hunk-button clicks, RMB anchor picking, keyboard caret movement, and merge-view interactions follow the same shape.

**Tech Stack:** Rust 2021, `imgui-rs` 0.12 (GUI), no new deps. Tests use stdlib `#[test]`. Pure layer is gated outside the `gui` feature so it builds without imgui.

---

## File Structure

- **Create** `src/app/input.rs` — `InputFrame`, `MouseButtons`, `Modifiers`, hit-testing helpers, selection state machine. Engine-agnostic Rust; no `imgui` import. Compiles under `--no-default-features`.
- **Create** `src/app/input_imgui.rs` — feature-gated adapter: `impl InputFrame { pub fn from_ui(ui: &imgui::Ui) -> Self }`. Behind `#[cfg(feature = "gui")]`. This is the only file that bridges imgui → `InputFrame`.
- **Modify** `src/app/mod.rs` — add `mod input;` (always) and `#[cfg(feature = "gui")] mod input_imgui;`. Re-export `InputFrame` for downstream files.
- **Modify** `src/app/diff_view.rs` — replace the body of `update_selection` (currently ~lines 875-1010) with a call into `input::selection_step(...)` after building an `InputFrame` at the top of `render`. The draw-side code is untouched; only the selection-mutation block changes.
- **Test** `src/app/input.rs` (inline `#[cfg(test)] mod tests`) — unit tests for hit-testing and the selection state machine.

The split keeps the imgui dependency at the adapter boundary. `input.rs` stays in the GUI submodule (it models GUI concepts) but is compiled even without the `gui` feature, so the test target can exercise it.

> **Out of scope for this plan (deliberately).** Hunk-decision buttons, RMB anchor picking, keyboard caret movement, the inline editor's input_text handling, and merge-view interactions are *not* migrated here. They follow the same pattern and will be addressed in follow-up plans once this slice has proven the shape. Picking the smallest interesting slice first is intentional — drag-selection is bug-prone enough to justify tests but small enough to land in one plan.

---

### Task 1: Stand up the pure `InputFrame` module

**Files:**
- Create: `src/app/input.rs`
- Modify: `src/app/mod.rs:1` (add module declaration near the other `mod` lines, e.g. after the existing `pub mod diff_view;`)

- [ ] **Step 1: Write the failing test**

Create `src/app/input.rs` with:

```rust
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
```

Then in `src/app/mod.rs`, add this line next to the other `mod` declarations (search for the existing `pub mod diff_view;` line and add directly below it):

```rust
pub mod input;
```

- [ ] **Step 2: Run test to verify it passes (smoke test on new module)**

Run: `cargo test --no-default-features --lib app::input::tests::default_input_frame_is_neutral`
Expected: `test result: ok. 1 passed`.

If the test doesn't appear, the `pub mod input;` declaration is missing or in the wrong place.

- [ ] **Step 3: Verify GUI build still compiles**

Run: `cargo build`
Expected: builds cleanly (no warnings about unused module — `InputFrame` is `pub` and will be consumed in Task 4).

- [ ] **Step 4: Commit**

```bash
git add src/app/input.rs src/app/mod.rs
git commit -m "app/input: add plain-data InputFrame scaffold"
```

---

### Task 2: Pure pane hit-testing

**Files:**
- Modify: `src/app/input.rs` (append)

**Why this task:** The existing `locate` closure inside `update_selection` (around line 875 of `diff_view.rs`) maps a screen position to `(Side, line_no, col)`. Pulling its math into a pure function gives us something testable in isolation and lets the selection state machine in the next task call it without seeing imgui.

- [ ] **Step 1: Write the failing tests**

Append to `src/app/input.rs`:

```rust
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
        // origin (100,50) + gutter 40 → text starts at x=140. y=50 is row 0.
        let hit = hit_test_pane(Side::Left, &l, [140.0, 50.0], |_| Some(20)).unwrap();
        assert_eq!(hit.row, 0);
        assert_eq!(hit.col, 0);
        assert_eq!(hit.side, Side::Left);
    }

    #[test]
    fn column_clamped_to_line_length() {
        let l = layout();
        // Click far to the right: char count is 5, so col clamps to 5.
        let hit = hit_test_pane(Side::Right, &l, [490.0, 50.0], |_| Some(5)).unwrap();
        assert_eq!(hit.col, 5);
    }

    #[test]
    fn padding_row_returns_none() {
        let l = layout();
        // Row 2 exists in `row_count` but caller says no line there.
        assert!(hit_test_pane(Side::Left, &l, [200.0, 50.0 + 16.0 * 2.0], |_| None).is_none());
    }

    #[test]
    fn row_index_past_row_count_returns_none() {
        let mut l = layout();
        l.row_count = 3;
        // y=50+16*5 = 130, still inside visible_height=200 but past row_count.
        assert!(hit_test_pane(Side::Left, &l, [200.0, 130.0], |_| Some(10)).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify all 5 pass**

Run: `cargo test --no-default-features --lib app::input::hit_tests`
Expected: `test result: ok. 5 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/app/input.rs
git commit -m "app/input: pure pane hit-testing"
```

---

### Task 3: Selection state machine (pure)

**Files:**
- Modify: `src/app/input.rs` (append)

**Why this task:** This is the meat. The selection logic in `diff_view::update_selection` (around lines 875-1010) is a state machine over `(InputFrame, locate(pos), prior selection, prior drag)`. We re-express it as a pure function returning a `SelectionStep` describing the mutations to apply. The diff_view will execute those mutations next task; tests here drive synthetic InputFrames and assert the steps.

Re-read `src/app/diff_view.rs:875-1010` before writing this task. The behavior to preserve:
1. LMB click outside any pane → clear selection and drag.
2. LMB click in a pane, no shift → set selection = collapsed at hit, start drag with `threshold_passed=false`.
3. LMB click in a pane with shift, prior selection on the same side → extend caret to hit, mark drag `threshold_passed=true`.
4. LMB click in a pane with shift, no prior selection or different side → same as case 2 (no extension).
5. While LMB held with active drag → if movement from press exceeds 4 px, set `threshold_passed=true`; if threshold passed, move caret to current locate(pos).
6. LMB released with active drag → clear drag.

- [ ] **Step 1: Write the failing tests**

Append to `src/app/input.rs`:

```rust
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
    pub set_selection: Option<Option<Selection>>, // None = no change; Some(None) = clear
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
///
/// These are two callbacks because the existing diff_view behavior differs
/// between press (strict hit-test) and drag (clamped to active side).
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

    // Strict hit-test: only succeeds inside the synthetic left pane [0,200)×[0,100).
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

    // Clamped: always returns a point on the requested side, clamping pos
    // to the pane bounds. Models the existing diff_view drag-tick behavior.
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
        // Mouse at (500,500) — far outside the synthetic pane — but the
        // clamped locator pins to (199,99) so the caret tracks the last
        // row / last column reachable. This is the existing diff_view
        // behavior and the reason for the second callback.
        let prior_drag = Some(DragState {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            press_screen: [10.0, 10.0],
            threshold_passed: true,
        });
        let f = frame([500.0, 500.0], false, true, false);
        let step = selection_step(&f, None, prior_drag, locate_left, locate_clamped_left);
        let sel = step.set_selection.unwrap().unwrap();
        // y=99 → line 10, x=199 → col 24
        assert_eq!(sel.side, Side::Left);
        assert_eq!(sel.anchor, SelPoint { line_no: 1, col: 0 });
        assert_eq!(sel.caret, SelPoint { line_no: 10, col: 24 });
    }
}
```

- [ ] **Step 2: Run tests to verify all 8 pass**

Run: `cargo test --no-default-features --lib app::input::selection_tests`
Expected: `test result: ok. 8 passed`. (If any test fails, the behavior in the implementation above does not match the spec — re-read `diff_view::update_selection` and fix the implementation, not the test.)

- [ ] **Step 3: Commit**

```bash
git add src/app/input.rs
git commit -m "app/input: pure selection state machine with tests"
```

---

### Task 4: imgui adapter

**Files:**
- Create: `src/app/input_imgui.rs`
- Modify: `src/app/mod.rs` (add the cfg-gated module declaration)

- [ ] **Step 1: Create the adapter**

Create `src/app/input_imgui.rs`:

```rust
//! Adapter from imgui's per-frame input state to the engine-agnostic
//! `InputFrame`. This is the only file in `app::input*` that depends on
//! imgui.

use super::input::{InputFrame, Modifiers, MouseButtons};

impl InputFrame {
    pub fn from_ui(ui: &imgui::Ui) -> Self {
        let io = ui.io();
        InputFrame {
            mouse_pos: io.mouse_pos,
            mouse_buttons: MouseButtons {
                left_down: ui.is_mouse_down(imgui::MouseButton::Left),
                left_clicked: ui.is_mouse_clicked(imgui::MouseButton::Left),
                right_clicked: ui.is_mouse_clicked(imgui::MouseButton::Right),
            },
            modifiers: Modifiers {
                shift: io.key_shift,
                ctrl: io.key_ctrl,
                alt: io.key_alt,
                super_: io.key_super,
            },
            wheel_v: io.mouse_wheel,
            wheel_h: io.mouse_wheel_h,
        }
    }
}
```

In `src/app/mod.rs`, add directly below `pub mod input;`:

```rust
#[cfg(feature = "gui")]
mod input_imgui;
```

- [ ] **Step 2: Build with and without GUI**

Run: `cargo build --no-default-features`
Expected: builds cleanly. The adapter is not compiled, so no imgui references.

Run: `cargo build`
Expected: builds cleanly. The adapter compiles and `InputFrame::from_ui` is available.

- [ ] **Step 3: Commit**

```bash
git add src/app/input_imgui.rs src/app/mod.rs
git commit -m "app/input_imgui: from_ui adapter (gui-gated)"
```

---

### Task 5: Wire `diff_view::update_selection` to the pure layer

**Files:**
- Modify: `src/app/diff_view.rs` — replace the body of `update_selection` (starts at line 845, ends at line 989 in the version of the file at plan-writing time; locate it via `grep -n 'fn update_selection' src/app/diff_view.rs` in case it has drifted).

**Why this task:** The pure layer is useless until production uses it. We migrate `update_selection` and only `update_selection`. The rest of `diff_view.rs` keeps reading imgui directly — that's fine, this plan does not migrate everything.

The strategy: keep the existing `Selection` / `SelPoint` / `DragState` / `Side` types in `diff_view.rs` (they're consumed by lots of other code), and translate between them and the `input::*` versions at the boundary inside `update_selection`. The signature of `update_selection` and the closures `pane_bounds` / `rows_for` are preserved.

#### Reference: the current code being replaced

For reference during the rewrite, here is the current `update_selection` body (verified against `src/app/diff_view.rs` at plan-write time). Re-grep before starting in case it has changed.

```rust
fn update_selection(
    ui: &Ui,
    state: &mut DiffViewState,
    left: &Pane,
    right: &Pane,
    left_origin: [f32; 2],
    right_origin: [f32; 2],
    left_visible_h: f32,
    right_visible_h: f32,
    pane_w: f32,
    char_w: f32,
    focus_request: &mut Option<crate::app::FocusedPane>,
) {
    if char_w <= 0.0 {
        return;
    }
    let pane_bounds = |side: Side| -> ([f32; 2], f32) {
        match side {
            Side::Left => (left_origin, left_visible_h),
            Side::Right => (right_origin, right_visible_h),
        }
    };
    let rows_for = |side: Side| -> &[Row] {
        match side {
            Side::Left => &left.rows,
            Side::Right => &right.rows,
        }
    };

    let locate = |pos: [f32; 2]| -> Option<(Side, SelPoint)> {
        for side in [Side::Left, Side::Right] {
            let (origin, visible_h) = pane_bounds(side);
            if pos[0] < origin[0] || pos[0] >= origin[0] + pane_w { continue; }
            let dy = pos[1] - origin[1];
            if dy < 0.0 || dy >= visible_h { continue; }
            let rows = rows_for(side);
            let row_idx = (dy / row_h()) as usize;
            if row_idx >= rows.len() { continue; }
            let row = &rows[row_idx];
            let line_no = row.line_no?;
            let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();
            let text_x0 = origin[0] + gutter_w();
            let raw = ((pos[0] - text_x0) / char_w).round();
            let col = raw.clamp(0.0, char_count as f32) as usize;
            return Some((side, SelPoint { line_no, col }));
        }
        None
    };

    let lmb_clicked = ui.is_mouse_clicked(imgui::MouseButton::Left);
    let lmb_held = ui.is_mouse_down(imgui::MouseButton::Left);

    if lmb_clicked {
        let press = ui.io().mouse_pos;
        match locate(press) {
            Some((side, point)) => {
                let shift = ui.io().key_shift;
                let extend = shift
                    && state.selection.as_ref().map_or(false, |s| s.side == side);
                if extend {
                    let sel = state.selection.as_mut().unwrap();
                    sel.caret = point;
                    state.drag = Some(DragState {
                        side, anchor: sel.anchor, press_screen: press, threshold_passed: true,
                    });
                } else {
                    state.selection = None;
                    state.drag = Some(DragState {
                        side, anchor: point, press_screen: press, threshold_passed: false,
                    });
                }
                *focus_request = Some(side.as_focused_pane());
            }
            None => {
                state.selection = None;
                state.drag = None;
            }
        }
    }

    if let Some(drag) = state.drag.as_mut() {
        if !lmb_held {
            state.drag = None;
        } else {
            let pos = ui.io().mouse_pos;
            if !drag.threshold_passed {
                let dx = pos[0] - drag.press_screen[0];
                let dy = pos[1] - drag.press_screen[1];
                if (dx * dx + dy * dy).sqrt() >= 4.0 {
                    drag.threshold_passed = true;
                }
            }
            if drag.threshold_passed {
                let side = drag.side;
                let (origin, visible_h) = pane_bounds(side);
                let rows = rows_for(side);
                if !rows.is_empty() {
                    let clamped_x = pos[0].clamp(origin[0] + gutter_w(), origin[0] + pane_w - 1.0);
                    let clamped_y = pos[1]
                        .clamp(origin[1], origin[1] + visible_h - 1.0)
                        .max(origin[1]);
                    let row_idx = ((clamped_y - origin[1]) / row_h()) as usize;
                    let row_idx = row_idx.min(rows.len() - 1);
                    let row = &rows[row_idx];
                    if let Some(line_no) = row.line_no {
                        let char_count: usize =
                            row.segments.iter().map(|s| s.text.chars().count()).sum();
                        let raw = ((clamped_x - (origin[0] + gutter_w())) / char_w).round();
                        let col = raw.clamp(0.0, char_count as f32) as usize;
                        let caret = SelPoint { line_no, col };
                        state.selection = Some(Selection { side, anchor: drag.anchor, caret });
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 1: Confirm the function still matches**

Run: `sed -n '845,989p' src/app/diff_view.rs`
If the body diverges meaningfully from the Reference block above (a few cosmetic changes are OK; new branches are not), stop and reconcile before proceeding. The conversion below assumes the Reference body.

- [ ] **Step 2: Rewrite the function**

Replace lines 845-989 of `src/app/diff_view.rs` with exactly the following. The signature is unchanged; only the body changes.

```rust
fn update_selection(
    ui: &Ui,
    state: &mut DiffViewState,
    left: &Pane,
    right: &Pane,
    left_origin: [f32; 2],
    right_origin: [f32; 2],
    left_visible_h: f32,
    right_visible_h: f32,
    pane_w: f32,
    char_w: f32,
    focus_request: &mut Option<crate::app::FocusedPane>,
) {
    use super::input::{self, InputFrame};

    if char_w <= 0.0 {
        // No rows rendered this frame — also nothing to read from imgui yet.
        return;
    }

    let pane_bounds = |side: Side| -> ([f32; 2], f32) {
        match side {
            Side::Left => (left_origin, left_visible_h),
            Side::Right => (right_origin, right_visible_h),
        }
    };
    let rows_for = |side: Side| -> &[Row] {
        match side {
            Side::Left => &left.rows,
            Side::Right => &right.rows,
        }
    };

    // Strict hit-test for the initial press.
    let locate = |pos: [f32; 2]| -> Option<(input::Side, input::SelPoint)> {
        for side in [Side::Left, Side::Right] {
            let (origin, visible_h) = pane_bounds(side);
            if pos[0] < origin[0] || pos[0] >= origin[0] + pane_w { continue; }
            let dy = pos[1] - origin[1];
            if dy < 0.0 || dy >= visible_h { continue; }
            let rows = rows_for(side);
            let row_idx = (dy / row_h()) as usize;
            if row_idx >= rows.len() { continue; }
            let row = &rows[row_idx];
            let line_no = row.line_no?;
            let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();
            let text_x0 = origin[0] + gutter_w();
            let raw = ((pos[0] - text_x0) / char_w).round();
            let col = raw.clamp(0.0, char_count as f32) as usize;
            return Some((side_to_input(side), input::SelPoint { line_no, col: col as u32 }));
        }
        None
    };

    // Clamped locate for the drag tick — preserves the existing behavior
    // where dragging off the pane still extends to the last reachable
    // row/column on the active side.
    let locate_clamped = |side: input::Side, pos: [f32; 2]| -> Option<input::SelPoint> {
        let side = side_from_input(side);
        let (origin, visible_h) = pane_bounds(side);
        let rows = rows_for(side);
        if rows.is_empty() {
            return None;
        }
        let clamped_x = pos[0].clamp(origin[0] + gutter_w(), origin[0] + pane_w - 1.0);
        let clamped_y = pos[1]
            .clamp(origin[1], origin[1] + visible_h - 1.0)
            .max(origin[1]);
        let row_idx = ((clamped_y - origin[1]) / row_h()) as usize;
        let row_idx = row_idx.min(rows.len() - 1);
        let row = &rows[row_idx];
        let line_no = row.line_no?;
        let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();
        let raw = ((clamped_x - (origin[0] + gutter_w())) / char_w).round();
        let col = raw.clamp(0.0, char_count as f32) as usize;
        Some(input::SelPoint { line_no, col: col as u32 })
    };

    let frame = InputFrame::from_ui(ui);

    let prior_sel = state.selection.as_ref().map(|s| input::Selection {
        side: side_to_input(s.side),
        anchor: input::SelPoint { line_no: s.anchor.line_no, col: s.anchor.col as u32 },
        caret: input::SelPoint { line_no: s.caret.line_no, col: s.caret.col as u32 },
    });
    let prior_drag = state.drag.as_ref().map(|d| input::DragState {
        side: side_to_input(d.side),
        anchor: input::SelPoint { line_no: d.anchor.line_no, col: d.anchor.col as u32 },
        press_screen: d.press_screen,
        threshold_passed: d.threshold_passed,
    });

    let step = input::selection_step(&frame, prior_sel, prior_drag, locate, locate_clamped);

    if let Some(new_sel) = step.set_selection {
        state.selection = new_sel.map(|s| Selection {
            side: side_from_input(s.side),
            anchor: SelPoint { line_no: s.anchor.line_no, col: s.anchor.col as usize },
            caret: SelPoint { line_no: s.caret.line_no, col: s.caret.col as usize },
        });
    }
    if let Some(new_drag) = step.set_drag {
        state.drag = new_drag.map(|d| DragState {
            side: side_from_input(d.side),
            anchor: SelPoint { line_no: d.anchor.line_no, col: d.anchor.col as usize },
            press_screen: d.press_screen,
            threshold_passed: d.threshold_passed,
        });
    }
    if let Some(side) = step.focus_request {
        *focus_request = Some(side_from_input(side).as_focused_pane());
    }
}

fn side_to_input(s: Side) -> super::input::Side {
    match s {
        Side::Left => super::input::Side::Left,
        Side::Right => super::input::Side::Right,
    }
}

fn side_from_input(s: super::input::Side) -> Side {
    match s {
        super::input::Side::Left => Side::Left,
        super::input::Side::Right => Side::Right,
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: builds cleanly. If `Side` does not implement the variants `Left`/`Right` exactly, or `SelPoint`/`Selection`/`DragState` have different field names than the Reference block above, fix the conversion (not the pure layer).

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --lib`
Expected: all existing tests pass, plus the 8 new `app::input::selection_tests::*` and 5 `app::input::hit_tests::*`, plus the trivial `default_input_frame_is_neutral`. No regressions.

- [ ] **Step 5: Manual smoke test**

Run: `cargo run`
Open two files in a 2-way diff. Verify by hand:
1. Single LMB click in left pane places the caret, focus shifts to left pane.
2. Click + drag selects a range; releasing the mouse stops the drag.
3. Shift-click with a prior selection on the same side extends the selection.
4. Click in the gap between panes clears the selection.
5. Drag below 4 px does nothing visible; past 4 px begins extending.
6. Drag off the pane (mouse leaves the pane while LMB held) keeps extending to the clamped edge.

If any behavior differs from before this plan, the conversion in Step 2 is wrong — diff_view's pre-existing behavior is the source of truth.

- [ ] **Step 6: Commit**

```bash
git add src/app/diff_view.rs
git commit -m "diff_view: drive update_selection through pure input layer"
```

---

## Followups (not in this plan)

Once Task 5 lands and the pattern is proven, the same `InputFrame` + pure-step shape can be applied to:

- Hunk-decision button clicks (`diff_view` overlay + inline buttons) — pure function from `(InputFrame, layout rects, current decisions)` to a `Vec<HunkDecisionChange>`.
- RMB anchor picking (lines ~1475-1476).
- Keyboard caret movement inside inline-editor rows (lines ~1709-1710, 1769).
- 3-way merge view interactions in `merge_view.rs`.
- A higher-level harness that takes a script of `InputFrame`s + initial `AppState` and runs the full per-frame logic without imgui, enabling scenario tests.

Each of these is a separate plan. Do not bundle them here — this plan's value is in proving the pattern with the smallest interesting slice.
