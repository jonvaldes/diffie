# Hover-to-Anchor UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two existing anchor-creation surfaces (gutter-click + `pending_a/b` prototype and `paint_gutter` dots) with a discoverable hover-to-anchor flow: anchor icons appear on rails inside the bezier connector strip, click → live bezier follows mouse → click an opposite-side icon to anchor or Esc/empty to cancel.

**Architecture:** A pure state-machine type (`AnchorPick`) on `DiffViewState` drives the interaction. `render` reserves two icon-wide rails inside the existing connector strip, queries hover/click via `invisible_button`, and feeds events into a pure transition function so the bulk of the logic stays unit-testable without imgui. A new overlay helper paints the rails; `draw_connector` paints the live drag bezier when `Picking`. The existing gutter no longer paints anchor dots, and the existing `pending_a/b` two-click code is removed in the same commit that adds the state-machine.

**Tech Stack:** Rust, `imgui-rs`, `wgpu`, existing core engine APIs (`session::SessionStore::add_anchor_two_way` / `remove_anchor`, `diff::Anchor`).

---

## File Structure

**Modified files:**

- `src/app/diff_view/common.rs` — add `AnchorPick` enum + new state fields; remove `pending_a` / `pending_b`. Add `RAIL_W` constant and helper `next_anchor_pick(current, click)`. (~50 LOC added, ~3 removed)
- `src/app/diff_view/overlay.rs` — add `paint_anchor_rail` and `anchor_icon_center` helpers; remove anchor-dot loop from `paint_gutter`; teach `draw_connector` to draw the live drag bezier. (~80 LOC added, ~15 removed)
- `src/app/diff_view/mod.rs` — replace `handle_anchor_click` and gutter `invisible_button` plumbing with rail strips inside the connector; drive `AnchorPick`; consume Esc. (~60 LOC changed)
- `src/app/diff_view/tests.rs` — exercise `next_anchor_pick` transitions (pure tests, no imgui). (~80 LOC added)

**No new files.** No core/library changes — `Anchor`, `SessionStore::add_anchor_two_way`, `remove_anchor` all exist.

---

## Task 1: Add `AnchorPick` state and pure transition function

**Files:**
- Modify: `src/app/diff_view/common.rs`
- Test: `src/app/diff_view/tests.rs`

This task lands the data types and the pure logic. No rendering changes yet — keeps the imgui-driven UI compiling against `pending_a/b` until Task 4 wires the new flow.

- [ ] **Step 1: Add `AnchorIconState`, `AnchorPick`, `RailClick`, `RailEvent`, and `RailAction` types**

Append to `src/app/diff_view/common.rs` (above the `DiffViewState` struct):

```rust
/// Which icon — if any — the mouse is over on a given rail, or which icon was
/// just clicked. `line` is 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RailClick {
    pub side: Side,
    pub line: crate::diff::LineNo,
    /// Whether `line` is already part of an anchor in `session.anchors`.
    pub already_anchored: bool,
    /// If `already_anchored`, the index of the anchor in `session.anchors`.
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
```

- [ ] **Step 2: Add the pure transition function**

Append to `src/app/diff_view/common.rs`:

```rust
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
        (Idle, RailEvent::Click(c)) => {
            if c.already_anchored {
                match c.anchor_idx {
                    Some(idx) => (Idle, RailAction::RemoveAnchor { idx }),
                    None => (Idle, RailAction::None),
                }
            } else {
                (Picking { side: c.side, line: c.line }, RailAction::None)
            }
        }

        // Picking + click on same-side icon: move source.
        (Picking { side, .. }, RailEvent::Click(c)) if c.side == side => {
            (Picking { side: c.side, line: c.line }, RailAction::None)
        }

        // Picking + click on opposite-side icon.
        (Picking { side: src_side, line: src_line }, RailEvent::Click(c)) => {
            if c.already_anchored {
                // Ambiguous — keep dragging. User must remove first.
                (Picking { side: src_side, line: src_line }, RailAction::None)
            } else {
                let (a, b) = match src_side {
                    Side::Left => (src_line, c.line),
                    Side::Right => (c.line, src_line),
                };
                (Idle, RailAction::AddAnchor { a, b })
            }
        }
    }
}
```

- [ ] **Step 3: Add fields to `DiffViewState` and remove `pending_a` / `pending_b`**

In `src/app/diff_view/common.rs`, inside `pub struct DiffViewState`:

Replace the two lines:

```rust
    pub(super) pending_a: Option<u32>,
    pub(super) pending_b: Option<u32>,
```

with:

```rust
    /// Live state of the hover-to-anchor interaction.
    pub(super) anchor_pick: AnchorPick,
```

Also add a `RAIL_W_BASE` constant near `GUTTER_W_BASE`:

```rust
/// Width of each anchor rail inside the connector strip.
pub(super) const RAIL_W_BASE: f32 = 18.0;

pub(super) fn rail_w() -> f32 {
    RAIL_W_BASE * crate::app::code_font_zoom()
}
```

- [ ] **Step 4: Write transition-table unit tests**

Append to `src/app/diff_view/tests.rs` (these tests are pure — no imgui needed, so they live below the existing imgui-driven test block but compile independently):

```rust
#[cfg(test)]
mod anchor_pick_tests {
    use super::super::common::{
        next_anchor_pick, AnchorPick, RailAction, RailClick, RailEvent, Side,
    };

    fn click(side: Side, line: u32, anchored: bool, idx: Option<usize>) -> RailClick {
        RailClick { side, line, already_anchored: anchored, anchor_idx: idx }
    }

    #[test]
    fn idle_unanchored_click_enters_picking() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Idle,
            RailEvent::Click(click(Side::Left, 3, false, None)),
        );
        assert_eq!(next, AnchorPick::Picking { side: Side::Left, line: 3 });
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn idle_anchored_click_removes() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Idle,
            RailEvent::Click(click(Side::Right, 7, true, Some(2))),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::RemoveAnchor { idx: 2 });
    }

    #[test]
    fn picking_escape_cancels() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 4 },
            RailEvent::Escape,
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_elsewhere_cancels() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Right, line: 9 },
            RailEvent::ClickedElsewhere,
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_opposite_unanchored_creates() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 5 },
            RailEvent::Click(click(Side::Right, 11, false, None)),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::AddAnchor { a: 5, b: 11 });
    }

    #[test]
    fn picking_opposite_anchored_is_noop() {
        let pick = AnchorPick::Picking { side: Side::Left, line: 5 };
        let (next, act) = next_anchor_pick(
            pick,
            RailEvent::Click(click(Side::Right, 11, true, Some(0))),
        );
        assert_eq!(next, pick);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_same_side_replaces_source() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 5 },
            RailEvent::Click(click(Side::Left, 9, false, None)),
        );
        assert_eq!(next, AnchorPick::Picking { side: Side::Left, line: 9 });
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_starts_with_right_side() {
        // Anchor mapping must put the LEFT line in `a` regardless of which
        // side the user clicked first.
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Right, line: 8 },
            RailEvent::Click(click(Side::Left, 2, false, None)),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::AddAnchor { a: 2, b: 8 });
    }

    #[test]
    fn none_event_preserves_state() {
        let s = AnchorPick::Picking { side: Side::Left, line: 1 };
        assert_eq!(next_anchor_pick(s, RailEvent::None), (s, RailAction::None));
    }
}
```

Also: the existing imgui-driven tests at the top of `tests.rs` might reference `pending_a` / `pending_b`. If so, replace those references with `anchor_pick`. If not, leave them alone.

- [ ] **Step 5: Update `handle_anchor_click` to compile against new fields**

The full `handle_anchor_click` function in `src/app/diff_view/mod.rs` (lines 260–280) reads `pending_a` / `pending_b`. To keep the codebase compiling at this point — without yet rewriting the call site — temporarily replace its body with a stub that no-ops. Task 4 deletes it.

In `src/app/diff_view/mod.rs`, replace the `handle_anchor_click` function body with:

```rust
fn handle_anchor_click(
    _state: &mut DiffViewState,
    _side: Side,
    _line: u32,
    _status: &mut String,
    _store: &SessionStore,
    _session_id: SessionId,
) {
    // Stub during refactor; rail-based anchor flow replaces this in Task 4.
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --no-default-features --lib`
Expected: PASS (including 9 new `anchor_pick_tests::*`).

Then: `cargo build` to confirm the GUI layer still compiles.
Expected: build succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/app/diff_view/common.rs src/app/diff_view/mod.rs src/app/diff_view/tests.rs
git commit -m "feat(diff-view): introduce AnchorPick state machine for hover-to-anchor"
```

---

## Task 2: Add `paint_anchor_rail` and `anchor_icon_center` helpers

**Files:**
- Modify: `src/app/diff_view/overlay.rs`

Pure rendering plumbing. Functions are unused until Task 4 calls them — that's fine; `#[allow(dead_code)]` keeps the build green for now.

- [ ] **Step 1: Add `anchor_icon_center` helper**

Insert in `src/app/diff_view/overlay.rs`, near the existing `line_screen_y`:

```rust
/// Screen-space center of the anchor icon for a given rail row.
/// `rail_left` + `rail_right` are the rail's x-extents on screen.
#[allow(dead_code)]
pub(super) fn anchor_icon_center(
    rail_left: f32,
    rail_right: f32,
    pane_top: f32,
    line: u32,
    scroll_y: f32,
    lh: f32,
) -> [f32; 2] {
    let x = (rail_left + rail_right) * 0.5;
    let y = line_screen_y(pane_top, line, scroll_y, lh) + lh * 0.5;
    [x, y]
}
```

- [ ] **Step 2: Add `paint_anchor_rail`**

Insert in `src/app/diff_view/overlay.rs`, replacing the anchor-dot loop in `paint_gutter` is handled in the next step. Add this function near `paint_gutter`:

```rust
use crate::app::diff_view::common::AnchorPick;

/// Paint anchor icons on a single rail. Outline icon on the hovered row (if any
/// and unanchored), filled icon for every row that is part of an anchor.
/// `pane_top` is the top of the *pane*, used together with `scroll_y` to map
/// content lines onto screen y.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_anchor_rail(
    ui: &Ui,
    rail_rect: [f32; 4],
    pane_top: f32,
    scroll_y: f32,
    lh: f32,
    side: Side,
    anchors: &[Anchor],
    hovered_line: Option<u32>,
    pick: AnchorPick,
) {
    let dl = ui.get_window_draw_list();
    let rail_left = rail_rect[0];
    let rail_right = rail_rect[2];
    let rail_top = rail_rect[1];
    let rail_bot = rail_rect[3];
    let glyph = "\u{f13d}"; // Nerd Font: anchor

    let filled_color = theme::LAVENDER();
    let outline_color = theme::with_alpha(theme::OVERLAY1(), 0.7);

    let line_for_anchor = |anc: &Anchor| match side {
        Side::Left => anc.a,
        Side::Right => anc.b,
    };

    // Filled icons for every anchored row.
    for anc in anchors {
        let line = line_for_anchor(anc);
        let center = anchor_icon_center(rail_left, rail_right, pane_top, line, scroll_y, lh);
        if center[1] + lh * 0.5 < rail_top || center[1] - lh * 0.5 > rail_bot {
            continue;
        }
        let size = ui.calc_text_size(glyph);
        dl.add_text(
            [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            filled_color,
            glyph,
        );
    }

    // Outline icon for the hovered row, if it isn't already filled.
    if let Some(hl) = hovered_line {
        let already_filled = anchors.iter().any(|a| line_for_anchor(a) == hl);
        if !already_filled {
            let center = anchor_icon_center(rail_left, rail_right, pane_top, hl, scroll_y, lh);
            if center[1] + lh * 0.5 >= rail_top && center[1] - lh * 0.5 <= rail_bot {
                let size = ui.calc_text_size(glyph);
                dl.add_text(
                    [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
                    outline_color,
                    glyph,
                );
            }
        }
    }

    // While Picking on this side, brighten the source icon slightly by re-stamping.
    if let AnchorPick::Picking { side: psd, line } = pick {
        if psd == side {
            let center = anchor_icon_center(rail_left, rail_right, pane_top, line, scroll_y, lh);
            let size = ui.calc_text_size(glyph);
            dl.add_text(
                [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
                theme::SAPPHIRE(),
                glyph,
            );
        }
    }
}
```

If `theme::SAPPHIRE()` doesn't exist in `src/app/theme.rs`, use `theme::BLUE()` instead (one or the other will be present — read `theme.rs` to confirm).

- [ ] **Step 3: Remove the anchor-dot loop from `paint_gutter`**

In `src/app/diff_view/overlay.rs`, inside `paint_gutter`, delete the trailing block:

```rust
    let dot_color = theme::LAVENDER();
    for anc in anchors {
        let line = match side {
            Side::Left => anc.a,
            Side::Right => anc.b,
        };
        let y = line_screen_y(g_top, line, scroll_y, lh) + lh * 0.5;
        if y < g_top || y > g_bottom {
            continue;
        }
        dl.add_circle([g_left + g_w * 0.5, y], 3.0, dot_color)
            .filled(true)
            .build();
    }
```

The `anchors` and `side` parameters are now unused inside `paint_gutter`. Keep them on the signature for now (Task 4 removes them from the call site) and prefix unused ones with `_`:

```rust
pub(super) fn paint_gutter(
    ui: &Ui,
    gutter_rect: [f32; 4],
    _anchors: &[Anchor],
    _side: Side,
    scroll_y: f32,
    lh: f32,
    line_count: u32,
) {
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build`
Expected: build succeeds with no new warnings except for the explicitly-allowed `dead_code` on the new helpers.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/diff_view/overlay.rs
git commit -m "feat(diff-view): paint_anchor_rail helper + drop gutter anchor dots"
```

---

## Task 3: Teach `draw_connector` to paint the live drag bezier

**Files:**
- Modify: `src/app/diff_view/overlay.rs`

Existing anchored-pair curves stay; we add one more curve when `AnchorPick::Picking`, from the source icon center to the mouse position.

- [ ] **Step 1: Extend `draw_connector` signature**

In `src/app/diff_view/overlay.rs`, change `draw_connector`'s signature to accept the live pick state and the rail x-coordinates:

```rust
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_connector(
    ui: &Ui,
    origin: [f32; 2],
    w: f32,
    h: f32,
    left_origin_y: f32,
    right_origin_y: f32,
    left_ranges: &[(u32, f32, f32)],
    right_ranges: &[(u32, f32, f32)],
    anchors: &[Anchor],
    hunks: &[Hunk],
    lh: f32,
    pick: crate::app::diff_view::common::AnchorPick,
    left_rail_center_x: f32,
    right_rail_center_x: f32,
) {
```

- [ ] **Step 2: Append live-drag curve inside `draw_connector`**

Add this block at the end of the `dl.with_clip_rect_intersect(...)` closure, after the anchor-curves loop:

```rust
        // Live drag bezier while the user is picking an anchor target.
        if let crate::app::diff_view::common::AnchorPick::Picking { side, line } = pick {
            let (src_x, src_origin_y) = match side {
                Side::Left => (left_rail_center_x, left_origin_y),
                Side::Right => (right_rail_center_x, right_origin_y),
            };
            let src_y = src_origin_y + (line as f32 - 1.0) * lh + lh * 0.5;
            let [mx, my] = ui.io().mouse_pos;
            // Use the same shape as anchor curves: cubic with horizontal handles.
            // x_l is the smaller x; flip args if user picked from the right.
            let (x_l, y1, x_r, y2) = if src_x < mx {
                (src_x, src_y, mx, my)
            } else {
                (mx, my, src_x, src_y)
            };
            stroke_bezier_curve(x_l, x_r, y1, y2, theme::CRUST(), 3.0);
        }
```

- [ ] **Step 3: Update `draw_connector` call site to keep compiling**

In `src/app/diff_view/mod.rs`, the existing `overlay::draw_connector(...)` call (around line 223) is missing the three new args. Add stubs to keep this task self-contained — Task 4 wires them properly:

```rust
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
        connector_pos[0],                // placeholder; Task 4 supplies real rail centers
        connector_pos[0] + CONNECTOR_W,  // placeholder
    );
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build`
Expected: build succeeds.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/diff_view/overlay.rs src/app/diff_view/mod.rs
git commit -m "feat(diff-view): draw live drag bezier while picking an anchor"
```

---

## Task 4: Wire rails into the connector layout and drive the state machine

**Files:**
- Modify: `src/app/diff_view/mod.rs`

This is the largest task; the previous three set up everything it consumes. By the end of this task the feature is functional.

- [ ] **Step 1: Remove the gutter-click anchor path**

In `src/app/diff_view/mod.rs`, inside `render_pane` (lines 304–315), the block reading:

```rust
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
```

becomes:

```rust
    ui.set_cursor_screen_pos(pane_pos);
    ui.dummy([g_w, pane_h]); // gutter strip — display only, clicks handled by rails
    let scroll_y_for_anchor = match side {
        Side::Left => state.last_left_scroll_y,
        Side::Right => state.last_right_scroll_y,
    };
```

Also update the `paint_gutter` call: the function now ignores `anchors` and `side` (Task 2 prefixed them with `_`), so the existing call still works without changes.

Delete the entire `handle_anchor_click` function (the stub from Task 1).

- [ ] **Step 2: Update imports in `src/app/diff_view/mod.rs`**

Add to the existing `use common::...` line:

```rust
use common::{
    build_pane_ranges, gutter_w, next_anchor_pick, rail_w, target_scroll, AnchorPick,
    PendingJump, RailAction, RailClick, RailEvent, CONNECTOR_W,
};
```

- [ ] **Step 3: Split the connector strip into three sub-regions**

Find the current connector block in `render` (around line 181-184):

```rust
    ui.set_cursor_screen_pos(connector_pos);
    ui.invisible_button("connector_strip", [CONNECTOR_W, pane_h]);
```

Replace it with a left-rail / middle / right-rail layout that records hover info. Insert this block in its place:

```rust
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
```

- [ ] **Step 4: Translate rail hover/click + Esc into a `RailEvent`**

Right after the right rail block above (still inside `render`, before the right pane renders), add:

```rust
    // We need each side's eased scroll-y to map mouse_y to a content line.
    // Use last frame's value — the rails sit between the panes vertically, so
    // last frame's scroll is correct for hover purposes this frame.
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

    fn anchor_idx_for(anchors: &[Anchor], side: Side, line: u32) -> Option<usize> {
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
            already_anchored: idx.is_some(),
            anchor_idx: idx,
        })
    } else if right_rail_clicked {
        let line = right_hover_line.unwrap_or(1);
        let idx = anchor_idx_for(anchors, Side::Right, line);
        RailEvent::Click(RailClick {
            side: Side::Right,
            line,
            already_anchored: idx.is_some(),
            anchor_idx: idx,
        })
    } else if matches!(state.anchor_pick, AnchorPick::Picking { .. })
        && ui.is_mouse_clicked(imgui::MouseButton::Left)
    {
        // While picking, any left-click outside the rails cancels.
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
            match store.add_anchor_two_way(session_id, Anchor { a, b }) {
                Ok(()) => *status = format!("anchor added: A:{a} <-> B:{b}"),
                Err(e) => *status = format!("anchor error: {e}"),
            }
        }
    }
```

- [ ] **Step 5: Paint the rails after both panes render**

Find the existing `overlay::draw_connector(...)` call. Right *after* it (and before the hover-overlay paint), insert:

```rust
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
```

- [ ] **Step 6: Update `draw_connector` call site with the real rail centers**

Replace the placeholder rail args from Task 3 step 3 with the real ones:

```rust
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
```

- [ ] **Step 7: Remove `dead_code` allows on the new helpers**

In `src/app/diff_view/overlay.rs`, delete the `#[allow(dead_code)]` attributes on `anchor_icon_center` and `paint_anchor_rail` — they're now used.

- [ ] **Step 8: Build and run tests**

Run: `cargo build`
Expected: build succeeds with no warnings about unused `anchor_icon_center` / `paint_anchor_rail`.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

Run: `cargo test --lib`
Expected: PASS (includes imgui-driven tests).

- [ ] **Step 9: Manual verification**

Run: `cargo run`
Open a 2-way diff. Verify each item from the spec's "verification checklist":

- Hover left pane → outline icon appears on left rail at the hovered row.
- Click outline icon → black bezier follows mouse.
- Click opposite-side outline icon → anchor created, both icons fill, recompute fires (hunks visibly realign).
- `Esc` mid-pick → bezier disappears, no anchor change.
- Click filled icon (idle) → anchor removed, recompute fires.
- During pick, click an anchored icon on the opposite side → no-op (still picking).
- During pick, click a same-side icon → source moves to new line.

- [ ] **Step 10: Commit**

```bash
git add src/app/diff_view/mod.rs src/app/diff_view/overlay.rs
git commit -m "feat(diff-view): hover-to-anchor UX with rail icons and drag bezier"
```

---

## Self-Review

**Spec coverage:**

- Icon placement on rails inside connector strip — Task 4 Step 3 (layout) + Task 2 Step 2 (paint).
- Icon glyph (nerd-font `\u{f13d}`) — Task 2 Step 2.
- Filled vs outline rendering rules — Task 2 Step 2 covers all four states from the spec table.
- `AnchorPick` enum on `DiffViewState` — Task 1 Steps 1 & 3.
- Removal of `pending_a/pending_b` — Task 1 Step 3.
- State machine transitions (all 8 rows of the spec's table) — Task 1 Step 2; tested in Step 4.
- Live bezier from source icon to mouse — Task 3 Step 2.
- Esc cancel — Task 4 Step 4.
- Click in empty space cancels — Task 4 Step 4 (`ClickedElsewhere` while Picking).
- Removal of `paint_gutter` anchor dots — Task 2 Step 3.
- Draw order: panes → ribbons → rails → live bezier → hover overlay — Task 3 (live bezier inside `draw_connector`) + Task 4 Step 5 (rails after connector). The ribbons happen inside `draw_connector` *before* the live bezier in the same call, so order is: panes (Task 4 step 5 is after panes), connector ribbons, anchor curves, live bezier (all inside one call), then rails painted after that call, then hover overlay last (unchanged). Verified against `render` order.

**Placeholder scan:** No "TBD", "TODO", or hand-wavy steps. Every code block is complete. Task 3 Step 3 deliberately uses temporary placeholder arguments for one task only — they're replaced in Task 4 Step 6 with named values.

**Type consistency:**

- `AnchorPick`, `RailClick`, `RailEvent`, `RailAction` defined in Task 1 Step 1; used in Task 1 Step 2 with matching shapes; used in Task 4 Step 4 against the same shape.
- `next_anchor_pick(current, event) -> (AnchorPick, RailAction)` — same signature in definition (Task 1 Step 2) and call site (Task 4 Step 4).
- `paint_anchor_rail`'s argument list matches between definition (Task 2 Step 2) and call (Task 4 Step 5): `ui, rail_rect, pane_top, scroll_y, lh, side, anchors, hovered_line, pick`.
- `draw_connector`'s new params (`pick`, `left_rail_center_x`, `right_rail_center_x`) appended in the same order in Task 3 Step 1 (definition), Task 3 Step 3 (placeholder call), and Task 4 Step 6 (real call).
- `RAIL_W_BASE`/`rail_w()` defined in Task 1 Step 3, used in Task 4 Step 3.
- `Anchor { a, b }` constructed via `RailAction::AddAnchor { a, b }` — both `a` and `b` are `LineNo` (= `u32`), matching `Anchor`'s fields. The left-line-into-`a` mapping in `next_anchor_pick` is verified by `picking_starts_with_right_side` test.

All consistent.
