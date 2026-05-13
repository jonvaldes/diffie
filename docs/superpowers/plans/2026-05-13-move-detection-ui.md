# Move Detection UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render move-tagged hunks in the 2-way diff view with peach backgrounds, a peach distance-faded bezier ribbon between paired halves, a jump-to-pair button on the hover overlay, and a brief arrival flash.

**Architecture:** Pure helpers in `src/app/diff_view/common.rs` (`move_color`, `move_ribbon_alpha`, `hunk_move_id`, `find_paired_hunk`). The existing `Row` struct gains a `moved: bool` flag set once at row-build time. Three call sites in `src/app/diff_view/render.rs` change: the row-background block, the ribbon paint loop, and the hover overlay panel. Jump and flash use two new transient fields on `AppState`.

**Tech Stack:** Rust, `imgui-rs` + `wgpu` + `winit`, Catppuccin-style palette in `src/app/theme.rs`. Fast core tests run via `cargo test --no-default-features --lib`; GUI tests via `cargo test --lib`.

Reference spec: `docs/superpowers/specs/2026-05-13-move-detection-ui-design.md`.

---

## File Structure

**Modified:**

- `src/app/diff_view/common.rs` — adds four helpers and a `#[cfg(test)] mod tests` for them. Extends `Row` with `moved: bool` and sets it in the existing row-builder.
- `src/app/diff_view/render.rs` — three small call-site edits: row-background, ribbon paint, hover overlay panel.
- `src/app/mod.rs` (`AppState`) — adds `pending_jump: Option<PendingJump>` and `flash: Option<MoveFlash>` transient fields. Adds a render-loop block that consumes `pending_jump`, computes the target scroll offset, and queues `SetNextWindowScroll` on the opposite pane.
- `src/app/diff_view/tests.rs` — one integration test that drives the public session API with the histogram engine + a known move, snapshots the hunks list, and asserts `find_paired_hunk` pairs them correctly.

**Not touched:** `merge_view.rs`, `recompute_three_way`, the Preferences modal, the engine bar (`engine_bar.rs`), session.rs, anything under `src/diff/`.

---

## Task 1: Helpers in `common.rs`

**Files:**
- Modify: `src/app/diff_view/common.rs` — add four helpers near the existing `ribbon_color` function (around line 507), plus a `#[cfg(test)] mod tests` at the bottom of the file.

- [ ] **Step 1: Add the helper signatures and a stub test module**

In `src/app/diff_view/common.rs`, after the existing `ribbon_color` function (currently ending around line 513), append:

```rust
/// Background tint and ribbon base color for moved hunks. Returns the
/// project theme's `PEACH` so individual call sites do not hard-code a
/// palette pick.
pub(super) fn move_color() -> [f32; 4] {
    super::super::theme::PEACH
}

/// Ribbon alpha for a moved hunk, faded by the vertical distance
/// between the two paired halves on screen.
///
/// Near pairs render at `RIBBON_ALPHA_NEAR` (same alpha as the
/// existing change ribbon). Pairs separated by `RIBBON_FADE_RANGE_PX`
/// or more clamp to `RIBBON_ALPHA_FAR`. Linear interpolation in
/// between; negative dy is treated as |dy|.
pub(super) fn move_ribbon_alpha(dy_px: f32) -> f32 {
    const RIBBON_ALPHA_NEAR: f32 = 0.30;
    const RIBBON_ALPHA_FAR: f32 = 0.08;
    const RIBBON_FADE_RANGE_PX: f32 = 800.0;
    let t = (dy_px.abs() / RIBBON_FADE_RANGE_PX).clamp(0.0, 1.0);
    RIBBON_ALPHA_NEAR + (RIBBON_ALPHA_FAR - RIBBON_ALPHA_NEAR) * t
}

/// Return the `move_id` of the first non-Equal op in the hunk, or
/// `None` for pure-Equal hunks and change hunks whose ops are
/// untagged.
pub(super) fn hunk_move_id(hunk: &crate::diff::Hunk) -> Option<u32> {
    use crate::diff::DiffOp;
    for op in &hunk.ops {
        match op {
            DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => {
                return *move_id;
            }
            DiffOp::Equal { .. } => continue,
        }
    }
    None
}

/// Find the hunk on the opposite side that carries the same `move_id`.
/// `my_side` is the side of the caller's hunk; the returned hunk is
/// the one on the OTHER side. Returns `None` if no pair exists in the
/// list.
///
/// Determining "which side" a hunk belongs to: a hunk's `a_range` is
/// `(0, 0)` for an Insert-only hunk (nothing on side A) and its
/// `b_range` is `(0, 0)` for a Delete-only hunk. Pairs cross sides:
/// Delete-only on A pairs with Insert-only on B.
pub(super) fn find_paired_hunk(
    hunks: &[crate::diff::Hunk],
    move_id: u32,
    my_side: Side,
) -> Option<&crate::diff::Hunk> {
    let opposite_is_delete_only = matches!(my_side, Side::Right);
    hunks.iter().find(|h| {
        if hunk_move_id(h) != Some(move_id) {
            return false;
        }
        let is_delete_only = h.b_range == (0, 0);
        let is_insert_only = h.a_range == (0, 0);
        if opposite_is_delete_only { is_delete_only } else { is_insert_only }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffOp, Hunk};

    fn equal_hunk(id: u32) -> Hunk {
        Hunk {
            id,
            a_range: (1, 1),
            b_range: (1, 1),
            ops: vec![DiffOp::Equal { a: 1, b: 1, text: "x".into() }],
        }
    }

    fn delete_hunk(id: u32, move_id: Option<u32>) -> Hunk {
        let op = DiffOp::Delete {
            a: 1, text: "d".into(), spans: None, move_id,
        };
        Hunk { id, a_range: (1, 1), b_range: (0, 0), ops: vec![op] }
    }

    fn insert_hunk(id: u32, move_id: Option<u32>) -> Hunk {
        let op = DiffOp::Insert {
            b: 1, text: "i".into(), spans: None, move_id,
        };
        Hunk { id, a_range: (0, 0), b_range: (1, 1), ops: vec![op] }
    }

    #[test]
    fn move_ribbon_alpha_at_zero_returns_near() {
        assert!((move_ribbon_alpha(0.0) - 0.30).abs() < 1e-6);
    }

    #[test]
    fn move_ribbon_alpha_at_fade_range_returns_far() {
        assert!((move_ribbon_alpha(800.0) - 0.08).abs() < 1e-6);
    }

    #[test]
    fn move_ribbon_alpha_clamps_above_fade_range() {
        assert!((move_ribbon_alpha(5000.0) - 0.08).abs() < 1e-6);
    }

    #[test]
    fn move_ribbon_alpha_clamps_below_zero() {
        assert!((move_ribbon_alpha(-100.0) - move_ribbon_alpha(100.0)).abs() < 1e-6);
    }

    #[test]
    fn hunk_move_id_pure_equal_returns_none() {
        assert_eq!(hunk_move_id(&equal_hunk(0)), None);
    }

    #[test]
    fn hunk_move_id_untagged_change_returns_none() {
        assert_eq!(hunk_move_id(&delete_hunk(0, None)), None);
    }

    #[test]
    fn hunk_move_id_reads_first_change_op() {
        assert_eq!(hunk_move_id(&delete_hunk(0, Some(7))), Some(7));
        assert_eq!(hunk_move_id(&insert_hunk(0, Some(7))), Some(7));
    }

    #[test]
    fn find_paired_hunk_returns_opposite_side_match() {
        let hunks = vec![
            delete_hunk(10, Some(0)),
            insert_hunk(11, Some(0)),
        ];
        // Caller is on Side::Left (a Delete hunk) → looks for Insert-only.
        let paired = find_paired_hunk(&hunks, 0, Side::Left);
        assert_eq!(paired.map(|h| h.id), Some(11));
        // Caller is on Side::Right (an Insert hunk) → looks for Delete-only.
        let paired = find_paired_hunk(&hunks, 0, Side::Right);
        assert_eq!(paired.map(|h| h.id), Some(10));
    }

    #[test]
    fn find_paired_hunk_returns_none_for_unpaired_id() {
        let hunks = vec![delete_hunk(10, Some(0))];
        assert!(find_paired_hunk(&hunks, 0, Side::Left).is_none());
        assert!(find_paired_hunk(&hunks, 99, Side::Right).is_none());
    }
}
```

- [ ] **Step 2: Run the new tests, confirm they pass**

Run: `cargo test --lib diff_view::common::tests`
Expected: 9 tests pass.

If the test invocation path is wrong (`diff_view::common::tests` vs `app::diff_view::common::tests`), inspect the failure message; the test names in the output will reveal the correct prefix. The GUI feature must be enabled (`cargo test --lib` is the default with default features on).

- [ ] **Step 3: Run the full suite, confirm no regressions**

Run: `cargo test --lib`
Expected: PASS, all tests green.

Run: `cargo test --no-default-features --lib`
Expected: PASS (these helpers live behind the GUI feature, so the core-only run should be unaffected).

- [ ] **Step 4: Commit**

```bash
git add src/app/diff_view/common.rs
git commit -m "diff_view: add move-detection helpers (color, ribbon alpha, pairing)"
```

---

## Task 2: Add `moved` flag to `Row`

**Files:**
- Modify: `src/app/diff_view/common.rs` — extend the `Row` struct definition (currently around line 329) and the row-builder block (currently around lines 396–484) so `moved` is set per-row.

- [ ] **Step 1: Extend the `Row` struct**

In `src/app/diff_view/common.rs`, update the `Row` struct (currently at line 329):

```rust
#[derive(Clone)]
pub(super) struct Row {
    pub(super) line_no: Option<u32>,
    pub(super) segments: Vec<Segment>,
    pub(super) cls: Cls,
    pub(super) hunk_id: u32,
    pub(super) is_change: bool,
    pub(super) hunk_first_row: usize,
    /// True iff this row's hunk is a moved hunk (i.e.
    /// `hunk_move_id(hunk).is_some()`). All rows in a moved hunk share
    /// the same value. Equal hunks always have `moved: false`.
    pub(super) moved: bool,
}
```

- [ ] **Step 2: Compile-check; the compiler will list every Row construction site that now needs the field**

Run: `cargo build`
Expected: FAIL with E0063 errors at each `Row { ... }` construction. The error output lists every site that needs `moved: ...`. Use this output to drive Step 3.

- [ ] **Step 3: Set `moved` at every construction site**

In `src/app/diff_view/common.rs`, the row-builder block (around lines 396-484) builds rows from a hunk variable named `h`. At the very top of that block — before the `match side { Side::Left => ... Side::Right => ... } else { ... }` — compute the moved flag once:

```rust
let moved = hunk_move_id(h).is_some();
```

(The exact line is the first line of the block that operates on `h`. Search for `let segments_for` in the surrounding code; place `let moved = hunk_move_id(h).is_some();` immediately before that closure definition.)

Then in each of the ~5 `rows.push(Row { ... })` calls inside the same block, add the field. For Delete/Insert rows it reads:

```rust
rows.push(Row {
    line_no: ...,
    segments: ...,
    cls: Cls::Delete, // or Cls::Insert
    hunk_id: h.id,
    is_change: true,
    hunk_first_row,
    moved,
});
```

For Equal rows (the `else` branch starting around line 466):

```rust
rows.push(Row {
    line_no: ...,
    segments: ...,
    cls: Cls::Equal,
    hunk_id: h.id,
    is_change: false,
    hunk_first_row,
    moved: false,
});
```

Equal rows are always `moved: false` because `hunk_move_id` returns `None` for pure-Equal hunks. (A hunk containing both Equal and non-Equal ops with a non-None move_id on the change op would also set `moved: true` for its Equal rows, but the current `group_into_hunks` does not produce such hunks — Equal and change are always separated.)

- [ ] **Step 4: Compile-check, confirm clean**

Run: `cargo build`
Expected: success. If any Row construction site was missed, the compiler points at it.

- [ ] **Step 5: Add a unit test that exercises the row builder**

In `src/app/diff_view/common.rs`, inside the existing `#[cfg(test)] mod tests` added in Task 1, append:

```rust
#[test]
fn row_builder_sets_moved_on_moved_hunks() {
    // Two hunks: one moved Delete on side A, one moved Insert on side B,
    // both tagged with move_id 0. The builder must set Row.moved=true on
    // all change rows of the moved hunks, and Row.moved=false elsewhere.
    let hunks = vec![
        Hunk {
            id: 0,
            a_range: (1, 2),
            b_range: (0, 0),
            ops: vec![
                DiffOp::Delete { a: 1, text: "ftr1".into(), spans: None, move_id: Some(0) },
                DiffOp::Delete { a: 2, text: "ftr2".into(), spans: None, move_id: Some(0) },
            ],
        },
        Hunk {
            id: 1,
            a_range: (3, 3),
            b_range: (3, 3),
            ops: vec![DiffOp::Equal { a: 3, b: 3, text: "ctx".into() }],
        },
        Hunk {
            id: 2,
            a_range: (0, 0),
            b_range: (1, 2),
            ops: vec![
                DiffOp::Insert { b: 1, text: "ftr1".into(), spans: None, move_id: Some(0) },
                DiffOp::Insert { b: 2, text: "ftr2".into(), spans: None, move_id: Some(0) },
            ],
        },
    ];
    // build_rows takes (hunks, side, ...). Find the actual signature in
    // this file and call it appropriately. The test only needs to confirm
    // that for the Delete-only hunk on Side::Left, all rows are moved=true.
    let (left_rows, _left_line_ys) = super::build_rows_for_test(&hunks, Side::Left);
    let moved_left: Vec<bool> = left_rows.iter().filter(|r| r.is_change).map(|r| r.moved).collect();
    assert!(moved_left.iter().all(|m| *m), "all left change rows should be moved");

    let (right_rows, _right_line_ys) = super::build_rows_for_test(&hunks, Side::Right);
    let moved_right: Vec<bool> = right_rows.iter().filter(|r| r.is_change).map(|r| r.moved).collect();
    assert!(moved_right.iter().all(|m| *m), "all right change rows should be moved");

    let equal_rows: Vec<&Row> = left_rows.iter().chain(right_rows.iter())
        .filter(|r| !r.is_change).collect();
    assert!(equal_rows.iter().all(|r| !r.moved), "equal rows must not be moved");
}
```

This test references `build_rows_for_test(hunks, side)` — a test-only thin wrapper around whatever the file's actual row-building function is called. If the row-builder is currently a closure or inline block (likely), add a `#[cfg(test)] pub(super) fn build_rows_for_test(...)` adapter at the bottom of the file that wraps the production code path. This wrapper exists ONLY for tests; the implementer should keep it small (one or two delegations) and not introduce a refactor.

If the row-builder cannot be cleanly factored out as a test wrapper, replace this test with a snapshot test that exercises `build_rows_for_test`'s functionality via integration through the session API in Task 5's integration test instead, and document the deferral in the commit message. The intent of this Step 5 test — "the builder sets `moved` correctly" — must still be covered by SOMETHING that fails if the moved flag is wired wrong.

- [ ] **Step 6: Run the new test, confirm it passes**

Run: `cargo test --lib diff_view::common::tests::row_builder_sets_moved_on_moved_hunks`
Expected: PASS.

- [ ] **Step 7: Run the full suite**

Run: `cargo test --lib`
Expected: all tests pass, no regressions.

- [ ] **Step 8: Commit**

```bash
git add src/app/diff_view/common.rs
git commit -m "diff_view: thread moved flag onto Row at build time"
```

---

## Task 3: Paint peach background on moved rows

**Files:**
- Modify: `src/app/diff_view/render.rs` — the row-background `match row.cls { ... }` block, currently around lines 408–416.

- [ ] **Step 1: Locate the row-background block**

In `src/app/diff_view/render.rs`, around line 408 the existing code reads:

```rust
// ---- backgrounds: hunk color → hover tint → selection ----
let bg = match row.cls {
    Cls::Equal => None,
    Cls::Delete => Some([0.55, 0.18, 0.18, 0.30]),
    Cls::Insert => Some([0.18, 0.50, 0.22, 0.30]),
};
if let Some(bg_rgba) = bg {
    dl.add_rect(p0, p1, bg_rgba).filled(true).build();
}
```

- [ ] **Step 2: Override the background for moved rows**

Replace the block above with:

```rust
// ---- backgrounds: hunk color → hover tint → selection ----
// Moved rows replace the standard red/green tint with the move
// color (peach @ 0.30 alpha). Equal rows are never moved.
let bg = if row.moved {
    Some(theme::with_alpha(crate::app::diff_view::common::move_color(), 0.30))
} else {
    match row.cls {
        Cls::Equal => None,
        Cls::Delete => Some([0.55, 0.18, 0.18, 0.30]),
        Cls::Insert => Some([0.18, 0.50, 0.22, 0.30]),
    }
};
if let Some(bg_rgba) = bg {
    dl.add_rect(p0, p1, bg_rgba).filled(true).build();
}
```

If `crate::app::diff_view::common::move_color` is not in scope in `render.rs`, the existing `use super::common::{...}` block near the top of the file (currently around lines 20-22) needs `move_color` added to it.

- [ ] **Step 3: Compile-check**

Run: `cargo build`
Expected: success.

- [ ] **Step 4: Run the test suite**

Run: `cargo test --lib`
Expected: PASS, no regressions.

- [ ] **Step 5: Manual verification (note in commit message)**

The implementer should briefly launch the GUI (`cargo run`) with a known-moved file pair if one is at hand, OR construct a synthetic pair on the fly:
- File A: `hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n`
- File B: `hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n`
- Engine: histogram. Toggle "Detect moves" in the per-tab engine bar. The `ftr1` and `ftr2` rows on both sides should turn peach.

If the implementer cannot run the GUI, note in the commit message that manual verification is pending and proceed; later tasks gate on the same verification path.

- [ ] **Step 6: Commit**

```bash
git add src/app/diff_view/render.rs
git commit -m "diff_view: paint moved rows with peach background"
```

---

## Task 4: Paint distance-faded peach ribbon for moved hunks

**Files:**
- Modify: `src/app/diff_view/render.rs` — the ribbon-paint loop around lines 48-67.

- [ ] **Step 1: Locate the ribbon loop**

In `src/app/diff_view/render.rs`, the existing ribbon loop (around lines 48-67) reads:

```rust
for h_obj in hunks {
    let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else {
        continue;
    };
    let Some(rr) = right_ranges.iter().find(|r| r.0 == h_obj.id) else {
        continue;
    };
    let a1 = left_origin_y + lr.1;
    let a2 = left_origin_y + lr.2;
    let b1 = right_origin_y + rr.1;
    let b2 = right_origin_y + rr.2;
    if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
        continue;
    }
    let color = ribbon_color(is_change_hunk(h_obj));
    fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, color);
}
```

The crucial observation: this loop iterates `hunks`, which is the SHARED hunk list. Moved Delete hunks and moved Insert hunks are SEPARATE entries in this list. A naive iteration would not have a meaningful (left, right) pair to ribbon between — `left_ranges` only contains entries for hunks that appear on side A, `right_ranges` only for side B. For a Delete-only moved hunk on A, `right_ranges.find` returns `None` and the current code `continue`s.

This means the existing ribbon loop already silently skips moved hunks today (since they only appear on one side). We need to add a SECOND loop right after it that walks moved Delete-only hunks, finds their paired Insert-only hunk via `find_paired_hunk`, and paints a peach ribbon from the Delete's screen-y range to the Insert's.

- [ ] **Step 2: Add the moved-ribbon pass**

After the existing `for h_obj in hunks { ... }` loop and before the anchor-curve loop that starts with `for anc in anchors {`, insert:

```rust
// Moved-hunk ribbons. Pair Delete-only and Insert-only hunks sharing
// a move_id; paint each pair as a distance-faded peach ribbon.
for h_obj in hunks {
    // Only iterate Delete-only moved hunks to avoid double-painting.
    let Some(move_id) = hunk_move_id(h_obj) else { continue };
    let is_delete_only = h_obj.b_range == (0, 0);
    if !is_delete_only { continue; }
    let Some(paired) = find_paired_hunk(hunks, move_id, Side::Left) else { continue };
    let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else { continue };
    let Some(rr) = right_ranges.iter().find(|r| r.0 == paired.id) else { continue };
    let a1 = left_origin_y + lr.1;
    let a2 = left_origin_y + lr.2;
    let b1 = right_origin_y + rr.1;
    let b2 = right_origin_y + rr.2;
    if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
        continue;
    }
    let mid_a = (a1 + a2) * 0.5;
    let mid_b = (b1 + b2) * 0.5;
    let dy = mid_a - mid_b;
    let alpha = move_ribbon_alpha(dy);
    let color = theme::with_alpha(move_color(), alpha);
    fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, color);
}
```

Add the necessary imports at the top of `render.rs`:

```rust
use super::common::{
    // existing imports retained ...
    hunk_move_id, find_paired_hunk, move_color, move_ribbon_alpha, Side,
};
```

(Inspect the existing `use super::common::{...}` block and merge these names in. The existing imports list is at the top of `render.rs` around line 20–22.)

- [ ] **Step 3: Compile-check**

Run: `cargo build`
Expected: success. If `Side` was previously used internally only with a different import path, the compiler points at it.

- [ ] **Step 4: Run the test suite**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Manual verification**

With the synthetic A/B pair from Task 3, observe a peach bezier connecting the ftr1/ftr2 rows on the left pane to the ftr1/ftr2 rows on the right pane. Scroll one pane down (or up) to increase the vertical distance between the two halves; the ribbon's alpha should visibly decrease. At full viewport separation it should be near 0.08 alpha (faint but visible).

- [ ] **Step 6: Commit**

```bash
git add src/app/diff_view/render.rs
git commit -m "diff_view: paint distance-faded peach ribbon between moved halves"
```

---

## Task 5: Jump-to-pair button, pending-jump scroll, and arrival flash

**Files:**
- Modify: `src/app/mod.rs` — add two fields to `AppState`: `pending_jump: Option<PendingJump>` and `flash: Option<MoveFlash>`. Add their type definitions near the top of the module or in a small `nav` submodule.
- Modify: `src/app/diff_view/render.rs` — extend the hover overlay panel (around lines 200-250) to add a `↕` button when the hovered hunk is moved AND has a pair. Extend the row-rendering block to overlay a flash rect when the row's `(session_id, hunk_id)` matches `state.flash`.
- Modify: `src/app/diff_view/mod.rs` — in the render loop where `igSetNextWindowScroll` is already called (around lines 218, 296, 361, 435), add a block that consumes `pending_jump`, computes the target scroll, and pushes the scroll on the opposite pane the same frame it's drawn. Decrement `flash.frames_remaining` and clear when it hits 0.
- Modify: `src/app/diff_view/tests.rs` — one integration test exercising the session pipeline + pairing logic.

- [ ] **Step 1: Define PendingJump and MoveFlash on AppState**

In `src/app/mod.rs`, find the `AppState` struct (search for `pub struct AppState`). Add two fields:

```rust
/// One-shot navigation request from the move-jump button. Consumed
/// by the render loop the same frame it's set; clears itself after.
pub(super) pending_jump: Option<PendingJump>,
/// Active arrival-flash overlay. Decremented per frame; cleared when
/// `frames_remaining` reaches 0.
pub(super) flash: Option<MoveFlash>,
```

Add these struct definitions adjacent to `AppState` (or near other small types like `MoveFlash`'s siblings if such a pattern exists):

```rust
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingJump {
    pub(super) session_id: crate::session::SessionId,
    pub(super) pane: crate::app::diff_view::common::Side,
    pub(super) target_line: crate::diff::LineNo,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MoveFlash {
    pub(super) session_id: crate::session::SessionId,
    pub(super) hunk_id: u32,
    pub(super) frames_remaining: u8,
}

const MOVE_FLASH_FRAMES: u8 = 30;
const MOVE_FLASH_PEAK_ALPHA: f32 = 0.20;
```

Also update `AppState`'s `Default`/`new`/initializer to set both fields to `None`. Search for the existing `AppState { ... }` initializer near line 200 and add `pending_jump: None,` and `flash: None,`.

- [ ] **Step 2: Wire the `↕` button into the hover overlay panel**

In `src/app/diff_view/render.rs`, in the function that paints the hover overlay (search for `Apply A → B` — currently around line 235). The panel currently widths 200 px. When the hovered hunk is moved AND a pair exists, widen to 240 px and emit a third button.

Locate the panel's beginning (search for `let panel_w = 200.0;`). Replace it with:

```rust
let hunk = hunks.iter().find(|h| h.id == hunk_id);
let move_id = hunk.and_then(hunk_move_id);
let paired = move_id.and_then(|id| find_paired_hunk(hunks, id, side));
let is_moved_with_pair = paired.is_some();
let panel_w: f32 = if is_moved_with_pair { 240.0 } else { 200.0 };
```

Note: the overlay-paint function does not currently take `hunks` or `side` as arguments. Adding them is part of this step. The existing signature lives near line 195-205; extend it to include `hunks: &[crate::diff::Hunk]` and `side: Side`, and update the call site (search the file for where `render_hover_overlay` or similar is invoked — there is exactly one call site).

After the existing `B → A` button (around line 244), add:

```rust
if is_moved_with_pair {
    ui.same_line();
    if ui.small_button(format!("↕##ov{hunk_id}_jump")) {
        if let Some(p) = paired {
            let target_line = match side {
                Side::Left => p.b_range.0,
                Side::Right => p.a_range.0,
            };
            let target_pane = match side {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            };
            pending_jump_out.set(Some(PendingJump {
                session_id, pane: target_pane, target_line,
            }));
        }
    }
    if let Some(p) = paired {
        if ui.is_item_hovered() {
            let n = match side {
                Side::Left => p.b_range.0,
                Side::Right => p.a_range.0,
            };
            ui.tooltip_text(format!("Jump to paired half (line {n})"));
        }
    }
}
```

`pending_jump_out` is a new `&Cell<Option<PendingJump>>` parameter on the overlay function; add it to the signature and the call site, and at the call site read it back to set `state.pending_jump`. The pattern mirrors existing out-cells in this file (`line_remove`, `arrow_focus`, etc.).

- [ ] **Step 3: Consume `pending_jump` in the render loop**

In `src/app/diff_view/mod.rs`, find the existing `igSetNextWindowScroll` calls (around lines 218, 296, 361, 435). These are inside the per-pane render block. At the top of each pane's `Begin`-equivalent (right before the existing center-anchored scroll-sync code), add:

```rust
if let Some(jump) = state.pending_jump {
    if jump.session_id == session_id && jump.pane == this_pane_side {
        // Look up the destination pane's content-y for the target line.
        // `line_ys` is the pane-local map of line_no -> content_y built
        // during row construction; it is already in scope here.
        if let Some(content_y) = line_ys.get(&jump.target_line).copied() {
            let y = (content_y - visible_h * 0.5).max(0.0);
            unsafe {
                imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
                    x: -1.0, y,
                });
            }
            // Find which hunk on this pane contains the target line so
            // the flash overlay can paint over it.
            let paired_hunk_id = hunks
                .iter()
                .find(|h| {
                    let (lo, hi) = match this_pane_side {
                        Side::Left => h.a_range,
                        Side::Right => h.b_range,
                    };
                    lo != 0 && jump.target_line >= lo && jump.target_line <= hi
                })
                .map(|h| h.id);
            if let Some(hid) = paired_hunk_id {
                state.flash = Some(MoveFlash {
                    session_id,
                    hunk_id: hid,
                    frames_remaining: MOVE_FLASH_FRAMES,
                });
            }
            state.pending_jump = None;
        }
    }
}
```

The `target_scroll` helper in `common.rs:659` is a different beast (it maps a scroll-center between two panes for live-mirror sync); we do not use it here because we want a one-shot center-on-line, not a continuous mirror.

This is the conceptually simplest place to mutate `pending_jump` and `flash`, but it depends on `state` being mutably accessible at this point. If the existing code pattern in `diff_view/mod.rs` captures only immutable references, route the mutation through the existing `out` cells used by other event sinks. Read the surrounding render-loop code carefully before writing; do not invent new mutability paths.

Exact line numbers will differ from what the plan estimates. The implementer should treat the line numbers as approximate and find the actual control-flow points by reading the file.

- [ ] **Step 4: Decrement and paint the flash**

In `src/app/diff_view/render.rs`'s row paint (after the existing background and hover blocks, before selection painting — around line 415), add:

```rust
if let Some(f) = flash {
    if f.session_id == session_id && Some(f.hunk_id) == Some(row.hunk_id) {
        let alpha = MOVE_FLASH_PEAK_ALPHA * (f.frames_remaining as f32 / MOVE_FLASH_FRAMES as f32);
        let color = theme::with_alpha(crate::app::diff_view::common::move_color(), alpha);
        dl.add_rect(p0, p1, color).filled(true).build();
    }
}
```

`flash` is a new optional parameter on `draw_row`. Add it to the signature and the call site (`draw_pane` invokes `draw_row`; thread it through). Use `Option<MoveFlash>` (Copy).

In `src/app/diff_view/mod.rs`, after the render loop completes for the frame (after all panes have drawn), decrement and clear the flash:

```rust
if let Some(f) = state.flash.as_mut() {
    if f.frames_remaining > 0 {
        f.frames_remaining -= 1;
    }
    if f.frames_remaining == 0 {
        state.flash = None;
    }
}
```

Place this at the bottom of the per-frame render function for diff_view, after both panes have rendered.

- [ ] **Step 5: Integration test in `tests.rs`**

Append to `src/app/diff_view/tests.rs`:

```rust
#[test]
fn session_with_move_produces_paired_hunks_with_matching_id() {
    use crate::diff::{DiffOp, DiffOptions, Hunk};
    use crate::session::{SessionMode, SessionStore};
    use crate::app::diff_view::common::{find_paired_hunk, hunk_move_id, Side};

    let a_text = "hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n";
    let b_text = "hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n";
    let store = SessionStore::new();
    let opts = DiffOptions {
        detect_moves: true,
        move_min_lines: 2,
        ..DiffOptions::default()
    };
    let id = store
        .open_two_way_with(a_text, b_text, Some("histogram".into()), opts)
        .expect("create session");
    let snapshot = store.snapshot(id).expect("snapshot");
    let hunks: Vec<Hunk> = match snapshot.mode {
        SessionMode::TwoWay { hunks, .. } => hunks,
        _ => panic!("expected TwoWay"),
    };
    let tagged: Vec<&Hunk> = hunks.iter().filter(|h| hunk_move_id(h).is_some()).collect();
    assert_eq!(tagged.len(), 2, "exactly two hunks should be tagged");
    let id_a = hunk_move_id(tagged[0]);
    let id_b = hunk_move_id(tagged[1]);
    assert_eq!(id_a, id_b, "both halves of a move share the id");
    let move_id = id_a.unwrap();
    // From the Delete-only hunk (Side::Left as caller), find the Insert-only pair.
    let delete_hunk = tagged.iter().find(|h| h.b_range == (0, 0)).expect("delete-only present");
    let paired = find_paired_hunk(&hunks, move_id, Side::Left);
    assert_eq!(paired.map(|h| h.id), Some(tagged.iter().find(|h| h.a_range == (0, 0)).unwrap().id));
    let _ = delete_hunk;
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test --lib`
Expected: PASS. The new integration test and all unit tests pass; no regressions.

- [ ] **Step 7: Manual verification**

Launch the GUI with the synthetic A/B pair from Task 3, toggle "Detect moves", click the `↕` button on a moved hunk. The opposite pane should scroll so the paired hunk is centered; the paired rows should briefly flash a darker peach (peaking at ~0.20 alpha, decaying to 0 over ~0.5 s at 60 Hz). Clicking the `↕` on the now-flashed hunk should ping-pong back.

- [ ] **Step 8: Commit**

```bash
git add src/app/mod.rs src/app/diff_view/mod.rs src/app/diff_view/render.rs src/app/diff_view/tests.rs
git commit -m "diff_view: jump-to-pair button with arrival flash for moves"
```

---

## Verification Checklist

After all five tasks complete:

- [ ] `cargo test --lib` passes; `cargo test --no-default-features --lib` passes.
- [ ] `cargo build` succeeds with no new warnings.
- [ ] In the GUI, the synthetic `hdr/blk/ftr` file pair with histogram + "Detect moves" enabled shows:
  - Peach row backgrounds on `ftr1`, `ftr2` on both sides.
  - A peach bezier ribbon connecting the two halves; alpha visibly decreases as the halves are scrolled apart.
  - A `↕` button on the hover overlay of either moved hunk.
  - Click `↕` → opposite pane scrolls so the paired hunk is centered; paired rows briefly flash darker peach.
- [ ] With "Detect moves" disabled, no peach color appears anywhere.
- [ ] Myers and patience engines do not produce peach UI (their `supports_moves` is false, no move_id tags reach the renderer).
- [ ] No `move_id` reads in `merge_view.rs` (3-way is explicitly out of scope).
