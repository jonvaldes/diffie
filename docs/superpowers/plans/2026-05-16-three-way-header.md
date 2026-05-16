# 3-Way Merge Header + Originating-Side Coloring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a P4Merge-style relationship strip above 3-way tabs, recolor per-hunk tints by originating side (Local=green, Remote=sapphire, Conflict=red), and accent each merged-result hunk with the color of the side it came from.

**Architecture:** A new self-contained `three_way_header` module owns the diagram + counts + legend rendering and a pure `count_hunks` helper. `merge.rs` gets a new `hunk_output_ranges` function (pure mirror of `apply_resolutions`' line accounting) so the result pane can locate each hunk in the merged-output line space without re-implementing the resolution walk. `merge_view.rs` and `result_pane.rs` consume those helpers; tint/ribbon palette mappings live inline in their consumers since each is a tiny `match` against `MergeHunk` kind.

**Tech Stack:** Rust, `imgui-rs`, existing Catppuccin palette in `src/app/theme.rs` (already exposes `SAPPHIRE`, `GREEN`, `RED`, `YELLOW`, `OVERLAY1`).

---

## File Structure

**New files:**
- `src/app/three_way_header.rs` — Owns `MergeCounts`, the diagram constants, `count_hunks(hunks)`, and `render(ui, counts)`. Self-contained. ~150 LOC.

**Modified files:**
- `src/merge.rs` — Add `hunk_output_ranges(hunks, resolutions) -> Vec<(u32, u32, u32)>` (~30 LOC + tests).
- `src/app/mod.rs` — `pub mod three_way_header;` declaration; insert header render + separator in the `ThreeWay` arm of `current_session_summary`; pass `hunks` + `resolutions` into `result_pane::render`.
- `src/app/merge_view.rs` — Swap the 3 tint colors in `paint_pane_text`'s `tint_for_line` closure and the 3 ribbon colors in `ribbon_color`. No structural change.
- `src/app/result_pane.rs` — Extend `render` signature with `hunks` + `resolutions`; paint per-hunk left-edge accent stripes after the multiline build.

No new dependencies.

---

## Task 1: Add `hunk_output_ranges` to `merge.rs`

**Files:**
- Modify: `src/merge.rs`

This is a pure logic helper needed by Task 5. Lands first so Task 5 has it to consume.

- [ ] **Step 1: Write the failing tests**

Append to `src/merge.rs`, inside the existing `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn hunk_output_ranges_stable_uses_text_lines() {
        let hunks = vec![MergeHunk::Stable {
            id: 0,
            base: vec!["b1".into(), "b2".into()],
            text: vec!["t1".into(), "t2".into(), "t3".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 3)]);
    }

    #[test]
    fn hunk_output_ranges_local_only_default_uses_local() {
        let hunks = vec![MergeHunk::LocalOnly {
            id: 0,
            base: vec!["b".into()],
            local: vec!["L1".into(), "L2".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 2)]);
    }

    #[test]
    fn hunk_output_ranges_local_only_base_resolution_uses_base() {
        let hunks = vec![MergeHunk::LocalOnly {
            id: 7,
            base: vec!["b1".into(), "b2".into(), "b3".into()],
            local: vec!["L".into()],
        }];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(7, Resolution::Base);
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(7, 1, 3)]);
    }

    #[test]
    fn hunk_output_ranges_conflict_unresolved_includes_markers() {
        let hunks = vec![MergeHunk::Conflict {
            id: 2,
            base: vec!["b".into()],
            local: vec!["L".into()],
            remote: vec!["R".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        // Markers: <<<LOCAL, L, |||BASE, b, ===, R, >>>REMOTE = 7 lines.
        assert_eq!(ranges, vec![(2, 1, 7)]);
    }

    #[test]
    fn hunk_output_ranges_conflict_resolved_to_local() {
        let hunks = vec![MergeHunk::Conflict {
            id: 2,
            base: vec!["b".into()],
            local: vec!["L1".into(), "L2".into()],
            remote: vec!["R".into()],
        }];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(2, Resolution::Local);
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(2, 1, 2)]);
    }

    #[test]
    fn hunk_output_ranges_skips_zero_line_hunks() {
        // A custom resolution with zero lines: hunk emits nothing, must be skipped.
        let hunks = vec![
            MergeHunk::Stable {
                id: 0,
                base: vec!["b".into()],
                text: vec!["b".into()],
            },
            MergeHunk::Conflict {
                id: 1,
                base: vec!["b".into()],
                local: vec!["L".into()],
                remote: vec!["R".into()],
            },
        ];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(1, Resolution::Custom { text: vec![] });
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 1)]); // hunk 1 skipped
    }

    #[test]
    fn hunk_output_ranges_multiple_hunks_total_matches_apply_resolutions() {
        let hunks = vec![
            MergeHunk::Stable {
                id: 0,
                base: vec!["a".into(), "b".into()],
                text: vec!["a".into(), "b".into()],
            },
            MergeHunk::LocalOnly {
                id: 1,
                base: vec!["c".into()],
                local: vec!["c'".into(), "c''".into()],
            },
            MergeHunk::Stable {
                id: 2,
                base: vec!["d".into()],
                text: vec!["d".into()],
            },
        ];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        let total: u32 = ranges.iter().map(|(_, lo, hi)| hi - lo + 1).sum();
        let out = apply_resolutions(&hunks, &resolutions);
        assert_eq!(total as usize, out.lines().count());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --no-default-features --lib hunk_output_ranges`
Expected: FAIL with `cannot find function 'hunk_output_ranges' in this scope`.

- [ ] **Step 3: Implement `hunk_output_ranges`**

Append to `src/merge.rs` just after `apply_resolutions`:

```rust
/// For each hunk, return `(hunk_id, first_line_1based, last_line_1based)` of
/// the lines the hunk will contribute to the merged output, given the current
/// resolutions. Mirrors `apply_resolutions`' line accounting exactly. Hunks
/// that resolve to zero lines are skipped.
pub fn hunk_output_ranges(
    hunks: &[MergeHunk],
    resolutions: &std::collections::HashMap<u32, Resolution>,
) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::with_capacity(hunks.len());
    let mut line_n: u32 = 1;
    for h in hunks {
        let count: u32 = match h {
            MergeHunk::Stable { id, text, .. } => match resolutions.get(id) {
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => text.len() as u32,
            },
            MergeHunk::LocalOnly { id, base, local } => match resolutions.get(id) {
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => local.len() as u32,
            },
            MergeHunk::RemoteOnly { id, base, remote } => match resolutions.get(id) {
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => remote.len() as u32,
            },
            MergeHunk::Conflict { id, base, local, remote } => match resolutions.get(id) {
                Some(Resolution::Local) => local.len() as u32,
                Some(Resolution::Remote) => remote.len() as u32,
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                None => {
                    // Markers: 4 fence lines + local + base + remote.
                    (4 + local.len() + base.len() + remote.len()) as u32
                }
            },
        };
        if count == 0 {
            continue;
        }
        let first = line_n;
        let last = line_n + count - 1;
        out.push((h.id(), first, last));
        line_n += count;
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --no-default-features --lib hunk_output_ranges`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add src/merge.rs
git commit -m "feat(merge): add hunk_output_ranges for per-hunk output-line mapping"
```

---

## Task 2: Recolor `merge_view` tints by originating side

**Files:**
- Modify: `src/app/merge_view.rs`

Two tiny color changes — five lines total. No new tests; visual.

- [ ] **Step 1: Recolor `paint_pane_text`'s row tints**

In `src/app/merge_view.rs`, locate the `tint_for_line` closure (around line 649). Replace:

```rust
                return Some(match kind_v {
                    HunkKind::LocalOnly => theme::with_alpha(theme::BLUE(), 0.22),
                    HunkKind::RemoteOnly => theme::with_alpha(theme::MAUVE(), 0.22),
                    HunkKind::Conflict => theme::with_alpha(theme::PEACH(), 0.30),
                });
```

with:

```rust
                return Some(match kind_v {
                    HunkKind::LocalOnly => theme::with_alpha(theme::GREEN(), 0.22),
                    HunkKind::RemoteOnly => theme::with_alpha(theme::SAPPHIRE(), 0.22),
                    HunkKind::Conflict => theme::with_alpha(theme::RED(), 0.30),
                });
```

- [ ] **Step 2: Recolor `ribbon_color`**

Locate `fn ribbon_color` (around line 976). Replace:

```rust
fn ribbon_color(h: &MergeHunk) -> [f32; 4] {
    match h {
        MergeHunk::Stable { .. } => theme::with_alpha(theme::OVERLAY1(), 0.10),
        MergeHunk::LocalOnly { .. } => theme::with_alpha(theme::BLUE(), 0.28),
        MergeHunk::RemoteOnly { .. } => theme::with_alpha(theme::MAUVE(), 0.28),
        MergeHunk::Conflict { .. } => theme::with_alpha(theme::PEACH(), 0.32),
    }
}
```

with:

```rust
fn ribbon_color(h: &MergeHunk) -> [f32; 4] {
    match h {
        MergeHunk::Stable { .. } => theme::with_alpha(theme::OVERLAY1(), 0.10),
        MergeHunk::LocalOnly { .. } => theme::with_alpha(theme::GREEN(), 0.28),
        MergeHunk::RemoteOnly { .. } => theme::with_alpha(theme::SAPPHIRE(), 0.28),
        MergeHunk::Conflict { .. } => theme::with_alpha(theme::RED(), 0.32),
    }
}
```

- [ ] **Step 3: Build and run tests**

Run: `cargo build`
Expected: build succeeds with no new warnings.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/app/merge_view.rs
git commit -m "feat(merge-view): recolor hunk tints + ribbons by originating side"
```

---

## Task 3: Create `three_way_header` module

**Files:**
- Create: `src/app/three_way_header.rs`
- Modify: `src/app/mod.rs` (declaration only)

This task lands the module and unit tests for `count_hunks`. Renderer is implemented but not yet wired into the main view — that happens in Task 4.

- [ ] **Step 1: Add the module declaration**

In `src/app/mod.rs`, near the other `pub mod ...` declarations at the top of the file (search for `pub mod merge_view;` and add nearby):

```rust
pub mod three_way_header;
```

- [ ] **Step 2: Write the failing test**

Create `src/app/three_way_header.rs` with the full module (header + tests). Write this content:

```rust
//! P4Merge-style relationship strip rendered at the top of 3-way tabs.
//!
//! Owns the small diagram + counts + color legend. Filename inputs continue
//! to live in `pane_header_bar`; this strip only adds the visual relationship
//! summary and per-side color key.

use imgui::Ui;

use crate::app::theme;
use crate::merge::MergeHunk;

/// Per-classification hunk counts surfaced in the header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeCounts {
    pub local_changes: u32,
    pub remote_changes: u32,
    pub conflicts: u32,
}

/// Tally hunks by classification. Stable hunks do not contribute.
pub fn count_hunks(hunks: &[MergeHunk]) -> MergeCounts {
    let mut c = MergeCounts::default();
    for h in hunks {
        match h {
            MergeHunk::Stable { .. } => {}
            MergeHunk::LocalOnly { .. } => c.local_changes += 1,
            MergeHunk::RemoteOnly { .. } => c.remote_changes += 1,
            MergeHunk::Conflict { .. } => c.conflicts += 1,
        }
    }
    c
}

// ---------------------------------------------------------------------------
// Diagram constants (relative to a 80x60 px canvas).
// ---------------------------------------------------------------------------

const CANVAS_W: f32 = 80.0;
const CANVAS_H: f32 = 60.0;
const STRIP_H: f32 = 72.0;
const MARKER_HALF: f32 = 6.0;

const BASE_POS: [f32; 2] = [40.0, 8.0];    // top center
const REMOTE_POS: [f32; 2] = [12.0, 30.0]; // left
const LOCAL_POS: [f32; 2] = [68.0, 30.0];  // right
const MERGE_POS: [f32; 2] = [40.0, 52.0];  // bottom center

// ---------------------------------------------------------------------------
// Render entry point.
// ---------------------------------------------------------------------------

/// Paint the diagram, counts, and legend at the current cursor. Advances the
/// cursor past the strip so subsequent widgets land below it.
pub fn render(ui: &Ui, counts: MergeCounts) {
    let origin = ui.cursor_screen_pos();
    let avail_w = ui.content_region_avail()[0];

    draw_diagram(ui, origin);
    draw_counts(ui, [origin[0] + CANVAS_W + 16.0, origin[1] + 6.0], counts);
    draw_legend(ui, [origin[0] + avail_w - legend_width(ui), origin[1] + 6.0]);

    ui.set_cursor_screen_pos([origin[0], origin[1] + STRIP_H]);
}

fn draw_diagram(ui: &Ui, origin: [f32; 2]) {
    let dl = ui.get_window_draw_list();
    let p = |off: [f32; 2]| [origin[0] + off[0], origin[1] + off[1]];

    let stroke = theme::OVERLAY1();
    let thickness = 1.5;

    // Curves: Base <-> Remote, Base <-> Local, Remote <-> Merge, Local <-> Merge.
    stroke_curve(&dl, p(BASE_POS), p(REMOTE_POS), stroke, thickness);
    stroke_curve(&dl, p(BASE_POS), p(LOCAL_POS), stroke, thickness);
    stroke_curve(&dl, p(REMOTE_POS), p(MERGE_POS), stroke, thickness);
    stroke_curve(&dl, p(LOCAL_POS), p(MERGE_POS), stroke, thickness);

    // Markers: Base = yellow square, Remote = sapphire diamond,
    // Local = green circle, Merge = overlay1 diamond.
    fill_square(&dl, p(BASE_POS), MARKER_HALF, theme::YELLOW());
    fill_diamond(&dl, p(REMOTE_POS), MARKER_HALF, theme::SAPPHIRE());
    fill_circle(&dl, p(LOCAL_POS), MARKER_HALF, theme::GREEN());
    fill_diamond(&dl, p(MERGE_POS), MARKER_HALF, theme::OVERLAY1());
}

fn draw_counts(ui: &Ui, top_left: [f32; 2], counts: MergeCounts) {
    let dl = ui.get_window_draw_list();
    let lh = ui.text_line_height();

    let rows: [(u8, &str, u32, [f32; 4]); 3] = [
        (0, "Remote changes:", counts.remote_changes, theme::SAPPHIRE()),
        (1, "Local changes:",  counts.local_changes,  theme::GREEN()),
        (2, "Conflicts:",      counts.conflicts,      conflict_count_color(counts.conflicts)),
    ];

    for (i, label, val, color) in rows {
        let y = top_left[1] + (i as f32) * (lh + 2.0);
        // Marker square.
        let sx = top_left[0];
        let sz = lh - 4.0;
        dl.add_rect([sx, y + 2.0], [sx + sz, y + 2.0 + sz], color)
            .filled(true)
            .build();
        // Label + value text.
        let tx = sx + sz + 6.0;
        dl.add_text([tx, y], theme::TEXT(), &format!("{label} {val}"));
    }
}

fn conflict_count_color(n: u32) -> [f32; 4] {
    if n > 0 { theme::RED() } else { theme::OVERLAY1() }
}

const LEGEND_ENTRIES: [(&str, fn() -> [f32; 4]); 4] = [
    ("Remote", theme::SAPPHIRE),
    ("Base",   theme::YELLOW),
    ("Local",  theme::GREEN),
    ("Merge",  theme::OVERLAY1),
];

fn legend_width(ui: &Ui) -> f32 {
    let mut w = 0.0_f32;
    for (label, _) in LEGEND_ENTRIES {
        w += ui.calc_text_size(label)[0] + 24.0;
    }
    w
}

fn draw_legend(ui: &Ui, top_left: [f32; 2]) {
    let dl = ui.get_window_draw_list();
    let lh = ui.text_line_height();
    let mut x = top_left[0];
    let y = top_left[1];
    for (label, color_fn) in LEGEND_ENTRIES {
        let sz = lh - 4.0;
        dl.add_rect([x, y + 2.0], [x + sz, y + 2.0 + sz], color_fn())
            .filled(true)
            .build();
        let tx = x + sz + 4.0;
        dl.add_text([tx, y], theme::TEXT(), label);
        x = tx + ui.calc_text_size(label)[0] + 12.0;
    }
}

// ---------------------------------------------------------------------------
// Primitive painters.
// ---------------------------------------------------------------------------

fn stroke_curve(
    dl: &imgui::DrawListMut,
    a: [f32; 2],
    b: [f32; 2],
    color: [f32; 4],
    thickness: f32,
) {
    // Cubic with vertically-offset control points so the curve bows softly.
    let mid_y = (a[1] + b[1]) * 0.5;
    let c1 = [a[0], mid_y];
    let c2 = [b[0], mid_y];
    dl.add_bezier_curve(a, c1, c2, b, color)
        .thickness(thickness)
        .build();
}

fn fill_square(dl: &imgui::DrawListMut, center: [f32; 2], half: f32, color: [f32; 4]) {
    dl.add_rect(
        [center[0] - half, center[1] - half],
        [center[0] + half, center[1] + half],
        color,
    )
    .filled(true)
    .build();
}

fn fill_circle(dl: &imgui::DrawListMut, center: [f32; 2], radius: f32, color: [f32; 4]) {
    dl.add_circle(center, radius, color).filled(true).build();
}

fn fill_diamond(dl: &imgui::DrawListMut, center: [f32; 2], half: f32, color: [f32; 4]) {
    let pts = [
        [center[0], center[1] - half],
        [center[0] + half, center[1]],
        [center[0], center[1] + half],
        [center[0] - half, center[1]],
    ];
    dl.add_polyline(pts.to_vec(), color)
        .filled(true)
        .thickness(0.0)
        .build();
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_hunks_empty_is_zero() {
        let c = count_hunks(&[]);
        assert_eq!(c, MergeCounts::default());
    }

    #[test]
    fn count_hunks_stable_does_not_count() {
        let hunks = vec![MergeHunk::Stable {
            id: 0,
            base: vec!["x".into()],
            text: vec!["x".into()],
        }];
        let c = count_hunks(&hunks);
        assert_eq!(c, MergeCounts::default());
    }

    #[test]
    fn count_hunks_categorizes_mixed() {
        let hunks = vec![
            MergeHunk::Stable { id: 0, base: vec![], text: vec![] },
            MergeHunk::LocalOnly { id: 1, base: vec![], local: vec!["L".into()] },
            MergeHunk::LocalOnly { id: 2, base: vec![], local: vec!["L".into()] },
            MergeHunk::RemoteOnly { id: 3, base: vec![], remote: vec!["R".into()] },
            MergeHunk::Conflict {
                id: 4, base: vec![], local: vec![], remote: vec![],
            },
        ];
        let c = count_hunks(&hunks);
        assert_eq!(c.local_changes, 2);
        assert_eq!(c.remote_changes, 1);
        assert_eq!(c.conflicts, 1);
    }

    #[test]
    fn conflict_count_color_red_when_positive() {
        assert_eq!(conflict_count_color(0), theme::OVERLAY1());
        assert_eq!(conflict_count_color(3), theme::RED());
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test --lib three_way_header`
Expected: this is a new module — tests run once it compiles. If `add_bezier_curve`, `add_polyline`, or any imgui-rs API name doesn't match the version in this crate, the build fails. If so, before proceeding, grep `Cargo.lock` (or `src/app/merge_view.rs`) for the exact draw-list API names already in use and adjust the helper signatures accordingly. The existing call sites in `merge_view.rs` use `dl.add_rect`, `dl.add_text`, and lower-level `imgui::sys::ImDrawList_PathBezierCubicCurveTo` for beziers. If `add_bezier_curve` is unavailable in this version of imgui-rs, replace `stroke_curve` with a hand-rolled `unsafe` call mirroring `stroke_bezier_curve` in `merge_view.rs:948`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib three_way_header`
Expected: 4 tests PASS.

Run: `cargo build`
Expected: build succeeds. Module compiles with no warnings beyond the expected "function unused" for `render` (it is wired up in Task 4).

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/three_way_header.rs
git commit -m "feat(app): add three_way_header module with count_hunks + diagram renderer"
```

---

## Task 4: Wire the header into the 3-way render path

**Files:**
- Modify: `src/app/mod.rs`

The renderer from Task 3 lights up here.

- [ ] **Step 1: Read the integration point**

Open `src/app/mod.rs` and locate `fn current_session_summary` (around line 1722). The `ThreeWay` arm starts around line 1830 with `anchor_bar_three_way(ui, ...)`. The new header call goes before that, after `pane_header_bar`.

- [ ] **Step 2: Add the header call**

Find this block in the `ThreeWay` arm:

```rust
        SessionMode::ThreeWay { hunks, anchors, .. } => {
            anchor_bar_three_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
```

Replace with:

```rust
        SessionMode::ThreeWay { hunks, anchors, .. } => {
            let counts = three_way_header::count_hunks(hunks);
            three_way_header::render(ui, counts);
            ui.separator();
            anchor_bar_three_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
```

- [ ] **Step 3: Build and run tests**

Run: `cargo build`
Expected: build succeeds. The previously-unused-`render` warning from Task 3 is now gone.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 4: Manual verification**

Run: `cargo run`. Open a 3-way merge tab. Visually confirm:
- The relationship diagram appears at the top of the 3-way view (above the filename inputs).
- The four colored markers are present: yellow square (Base) top-center, sapphire diamond (Remote) left, green circle (Local) right, gray diamond (Merge) bottom-center.
- Curved lines connect Base↔Remote, Base↔Local, Remote↔Merge, Local↔Merge.
- The count rows show non-zero values when hunks exist; Conflicts is red when > 0.
- The legend on the right shows the four labels with correct colors.

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs
git commit -m "feat(app): render three_way_header above 3-way merge tabs"
```

---

## Task 5: Result-pane originating-side accent stripes

**Files:**
- Modify: `src/app/result_pane.rs`
- Modify: `src/app/mod.rs` (call-site signature change)

- [ ] **Step 1: Extend `result_pane::render` signature**

Replace the existing `pub fn render` in `src/app/result_pane.rs` with the version below. The new function body:

1. Records the multiline widget's screen rect before/after the build so we know the clip area.
2. Computes per-hunk output ranges via `crate::merge::hunk_output_ranges`.
3. Maps each hunk to a stripe color via a private `stripe_color` helper.
4. Paints a 4-px-wide vertical stripe along the left edge of the widget rect for each hunk's `[first_line, last_line]` row span, clipped to the visible scroll region.

Full new `render`:

```rust
//! Editable result pane.
//!
//! Shows `SessionStore::compute_result` for the active session and lets the
//! user override it. Edits flow back through `update_manual_result`, so the
//! session's `manual_result` field is the source of truth once the user
//! types. When the user is not editing, decision/resolution changes upstream
//! recompute the result and refresh the buffer here.

use std::collections::HashMap;

use imgui::{FontId, Ui};

use crate::app::theme;
use crate::merge::{hunk_output_ranges, MergeHunk, Resolution};
use crate::session::{SessionId, SessionStore};

const STRIPE_W: f32 = 4.0;

#[derive(Default)]
pub struct ResultState {
    buffer: String,
    was_active_last_frame: bool,
    initialized: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    state: &mut ResultState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
) {
    if !state.was_active_last_frame {
        if let Ok(text) = store.compute_result(session_id) {
            if text != state.buffer {
                state.buffer = text;
            }
        }
        state.initialized = true;
    }

    if !state.initialized {
        ui.text_disabled("Computing…");
        return;
    }

    let avail = ui.content_region_avail();
    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let widget_top_left = ui.cursor_screen_pos();
    let lh = ui.text_line_height();
    let changed = ui
        .input_text_multiline("##diffie_result", &mut state.buffer, avail)
        .build();
    let active = ui.is_item_active();
    let focused = ui.is_item_focused();

    paint_origin_stripes(
        ui,
        widget_top_left,
        avail,
        lh,
        hunks,
        resolutions,
    );

    drop(_font_tok);

    if active || focused {
        *focus_request = Some(crate::app::FocusedPane::Result);
    }

    if changed {
        let _ = store.update_manual_result(session_id, state.buffer.clone());
    }
    state.was_active_last_frame = active;
}

fn paint_origin_stripes(
    ui: &Ui,
    widget_top_left: [f32; 2],
    widget_size: [f32; 2],
    lh: f32,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
) {
    if lh <= 0.0 {
        return;
    }
    let widget_top = widget_top_left[1];
    let widget_bottom = widget_top + widget_size[1];
    let widget_left = widget_top_left[0];
    let dl = ui.get_window_draw_list();
    let ranges = hunk_output_ranges(hunks, resolutions);
    let hunks_by_id: HashMap<u32, &MergeHunk> = hunks.iter().map(|h| (h.id(), h)).collect();
    dl.with_clip_rect(
        [widget_left, widget_top],
        [widget_left + widget_size[0], widget_bottom],
        || {
            for (id, first, last) in ranges {
                let Some(hunk) = hunks_by_id.get(&id) else { continue };
                let Some(color) = stripe_color(hunk, resolutions.get(&id)) else { continue };
                let y0 = widget_top + (first as f32 - 1.0) * lh;
                let y1 = widget_top + (last as f32) * lh;
                if y1 < widget_top || y0 > widget_bottom {
                    continue;
                }
                let y0 = y0.max(widget_top);
                let y1 = y1.min(widget_bottom);
                dl.add_rect(
                    [widget_left, y0],
                    [widget_left + STRIPE_W, y1],
                    color,
                )
                .filled(true)
                .build();
            }
        },
    );
}

/// Stripe color for a hunk's output region, given the current resolution.
/// Returns `None` for stable hunks (no stripe drawn).
pub(crate) fn stripe_color(hunk: &MergeHunk, resolution: Option<&Resolution>) -> Option<[f32; 4]> {
    match hunk {
        MergeHunk::Stable { .. } => None,
        MergeHunk::LocalOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::GREEN(),
        }),
        MergeHunk::RemoteOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::SAPPHIRE(),
        }),
        MergeHunk::Conflict { .. } => Some(match resolution {
            None => theme::RED(),
            Some(Resolution::Local) => theme::GREEN(),
            Some(Resolution::Remote) => theme::SAPPHIRE(),
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::MergeHunk;

    fn stable() -> MergeHunk {
        MergeHunk::Stable { id: 0, base: vec![], text: vec![] }
    }
    fn local_only() -> MergeHunk {
        MergeHunk::LocalOnly { id: 1, base: vec![], local: vec!["L".into()] }
    }
    fn remote_only() -> MergeHunk {
        MergeHunk::RemoteOnly { id: 2, base: vec![], remote: vec!["R".into()] }
    }
    fn conflict() -> MergeHunk {
        MergeHunk::Conflict {
            id: 3, base: vec![], local: vec![], remote: vec![],
        }
    }

    #[test]
    fn stable_has_no_stripe() {
        assert_eq!(stripe_color(&stable(), None), None);
    }

    #[test]
    fn local_only_defaults_to_green() {
        assert_eq!(stripe_color(&local_only(), None), Some(theme::GREEN()));
    }

    #[test]
    fn local_only_base_resolution_is_yellow() {
        assert_eq!(
            stripe_color(&local_only(), Some(&Resolution::Base)),
            Some(theme::YELLOW())
        );
    }

    #[test]
    fn remote_only_defaults_to_sapphire() {
        assert_eq!(stripe_color(&remote_only(), None), Some(theme::SAPPHIRE()));
    }

    #[test]
    fn conflict_unresolved_is_red() {
        assert_eq!(stripe_color(&conflict(), None), Some(theme::RED()));
    }

    #[test]
    fn conflict_resolutions_match_chosen_side() {
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Local)), Some(theme::GREEN()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Remote)), Some(theme::SAPPHIRE()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Base)), Some(theme::YELLOW()));
        assert_eq!(
            stripe_color(&conflict(), Some(&Resolution::Custom { text: vec![] })),
            Some(theme::OVERLAY1())
        );
    }
}
```

- [ ] **Step 2: Update the call site in `src/app/mod.rs`**

In `src/app/mod.rs`, the `result_pane::render(...)` call (around line 1878) currently passes 6 arguments. Update it to pass `hunks` and `resolutions` as well. The surrounding `ThreeWay` arm destructures `hunks` already; `resolutions` lives on the session and needs to be added to the destructure.

Locate this destructure (around line 1830):

```rust
        SessionMode::ThreeWay { hunks, anchors, .. } => {
```

Change to:

```rust
        SessionMode::ThreeWay { hunks, anchors, resolutions, .. } => {
```

Then locate the `result_pane::render` call:

```rust
                        result_pane::render(
                            ui,
                            &state.sessions,
                            id,
                            result,
                            mono,
                            &mut focus_request,
                        );
```

Replace with:

```rust
                        result_pane::render(
                            ui,
                            &state.sessions,
                            id,
                            result,
                            mono,
                            &mut focus_request,
                            hunks,
                            resolutions,
                        );
```

- [ ] **Step 3: Run tests**

Run: `cargo test --no-default-features --lib`
Expected: PASS, including the 6 new `result_pane::tests::*`.

Run: `cargo build`
Expected: builds without warnings.

- [ ] **Step 4: Manual verification**

Run: `cargo run`. Open a 3-way merge with a mix of LocalOnly / RemoteOnly / Conflict hunks. Verify:
- The merged result pane (below the merge view) shows a thin vertical accent stripe along its left edge for each non-Stable hunk.
- LocalOnly hunks → green stripe.
- RemoteOnly hunks → sapphire stripe.
- Unresolved Conflict hunks → red stripe.
- Pick a resolution from the merge view (e.g., accept Base for one hunk) and verify the stripe color updates accordingly.
- Stripes scroll with the result pane content (they're clipped to the widget rect).

- [ ] **Step 5: Commit**

```bash
git add src/app/result_pane.rs src/app/mod.rs
git commit -m "feat(result-pane): per-hunk originating-side accent stripes"
```

---

## Self-Review

**Spec coverage:**
- New header strip with diagram, counts, legend → Task 3 + Task 4.
- Diagram with Base (yellow square), Remote (sapphire diamond), Local (green circle), Merge (overlay1 diamond) + connecting curves → Task 3 step 2 (`draw_diagram`).
- Counts: Remote / Local / Conflicts, conflict count in red when > 0 → Task 3 step 2 (`draw_counts` + `conflict_count_color`).
- Color legend on the right → Task 3 step 2 (`draw_legend` + `LEGEND_ENTRIES`).
- Header rendered only for `TabMode::ThreeWay` → Task 4 step 2 (inserted in the `ThreeWay` match arm only).
- Per-hunk row tints recolored (LocalOnly=green, RemoteOnly=sapphire, Conflict=red) on all three input panes → Task 2 step 1.
- Connector ribbons recolored to same palette → Task 2 step 2.
- Result pane gets per-hunk left-edge accent stripes, colored by originating side / chosen resolution → Task 5.
- `hunk_output_ranges` helper for stripe positioning → Task 1.
- Catppuccin palette: no new theme entries (uses existing SAPPHIRE/GREEN/RED/YELLOW/OVERLAY1) → confirmed by `grep` against `src/app/theme.rs:161-169`.

**Placeholder scan:** No "TBD", "implement later", or vague handwaving. Task 3 step 3 has a contingency for imgui-rs API name mismatches but specifies *exactly* what to grep for and what to substitute (point to `merge_view.rs:948`). Each Rust code block is complete and runnable.

**Type consistency:**
- `MergeCounts { local_changes, remote_changes, conflicts }` defined in Task 3 step 2; consumed unchanged in Task 4 step 2.
- `count_hunks(hunks) -> MergeCounts` and `render(ui, counts)` signatures match across definition (Task 3) and call site (Task 4).
- `hunk_output_ranges(hunks, resolutions) -> Vec<(u32, u32, u32)>` signature consistent between Task 1 (definition) and Task 5 (call site).
- `stripe_color(hunk, resolution: Option<&Resolution>) -> Option<[f32; 4]>` defined and tested in Task 5; only consumed inside the same module.
- `result_pane::render` extended signature (`hunks: &[MergeHunk], resolutions: &HashMap<u32, Resolution>`) consistent between Task 5 step 1 (definition) and Task 5 step 2 (call site update).
- `ResultState` shape unchanged across the refactor.

All consistent.
