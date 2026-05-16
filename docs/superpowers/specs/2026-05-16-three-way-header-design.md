# 3-Way Merge Header + Originating-Side Coloring

Add a P4Merge-style relationship strip at the top of 3-way merge tabs, recolor the per-hunk tints in the merge view by which side originated the change, and accent the merged result pane with the same colors so each output hunk advertises where it came from.

## Background

The current 3-way view shows three editable panes (Remote | Base | Local since commit `5ebc33c`), a connector with bezier ribbons between adjacent panes, and a separate editable result pane below. Per-hunk tints currently use BLUE/MAUVE/PEACH keyed on `MergeHunk` kind. The result pane has no per-hunk visual cues.

The user has cited P4Merge as the visual reference: a small relationship diagram (Base, Remote, Local, Merge) sits at the top of the view, and each side has its own color (Base=yellow, Remote=blue, Local=green, Merge result=neutral). Hunks throughout the merge are colored by which side originated the change. This spec mirrors that vocabulary onto the existing diffie 3-way layout.

## User-facing changes

### New header strip

Rendered only for `TabMode::ThreeWay`, between `engine_bar`'s separator and `pane_header_bar`. Height: 72 px (independent of font zoom).

Three regions left-to-right:

1. **Relationship diagram** — fixed 80 × 60 px painted on the imgui draw list:
   - **Base**: yellow square at the top-center of the canvas.
   - **Remote**: blue diamond on the left.
   - **Local**: green circle on the right.
   - **Merge**: neutral-gray diamond at the bottom-center.
   - Cubic-bezier curves connect Base→Remote, Base→Local, Remote→Merge, Local→Merge, mirroring the P4Merge reference. Stroke uses `theme::OVERLAY1()` at 1.5 px.
2. **Counts** — three labelled values, vertically stacked, left-aligned within the region:
   - `■ Remote changes: N` (square glyph + Sapphire)
   - `■ Local changes: N` (Green)
   - `■ Conflicts: N` (Red when N > 0, OVERLAY1 when N = 0)
   - Each count is derived by a single pass over `hunks: &[MergeHunk]`.
3. **Color legend** — right-aligned, four small color swatches with role labels:
   - 🟦 Remote
   - 🟨 Base
   - 🟩 Local
   - ⬛ Merge

Below the strip, a thin separator. The existing `pane_header_bar` (with editable filename inputs and the browse / save buttons) continues to render unchanged.

### Per-hunk tints, recolored by originating side

`merge_view.rs` currently applies these tints per hunk kind:

| Kind | Current | New |
| --- | --- | --- |
| `Stable` | none | none |
| `LocalOnly` | BLUE (Sapphire) | **GREEN** |
| `RemoteOnly` | MAUVE | **SAPPHIRE** |
| `Conflict` | PEACH | **RED** |

Alpha values remain at the current ~0.22 for row tints and ~0.28 for ribbons. The recoloring applies to **all three input panes** (the row background tint inside `paint_pane_text`) and to **both connector strips** (the `fill_bezier_ribbon` call in `draw_connector`).

The Catppuccin Macchiato + Latte palettes already expose `green`, `sapphire`, `red`, `yellow` — no new theme entries needed.

### Result pane: originating-side accent stripe

Each hunk in the merged output gets a 4 px-wide vertical accent stripe painted on the **left edge** of the result pane, spanning that hunk's lines. Color per hunk:

| State | Stripe color |
| --- | --- |
| `Stable` | none |
| `LocalOnly`, default resolution (Local) | Green |
| `LocalOnly`, resolution = Base | Yellow |
| `LocalOnly`, resolution = Custom | Overlay1 (gray) |
| `RemoteOnly`, default (Remote) | Sapphire |
| `RemoteOnly`, resolution = Base | Yellow |
| `RemoteOnly`, resolution = Custom | Overlay1 |
| `Conflict`, unresolved | Red |
| `Conflict`, resolution = Local | Green |
| `Conflict`, resolution = Remote | Sapphire |
| `Conflict`, resolution = Base | Yellow |
| `Conflict`, resolution = Custom | Overlay1 |

The stripe is purely a visual cue; it does not alter the text widget's geometry or line wrapping. Stripes are clipped to the result pane's visible scroll region.

When the user has typed into the result pane (`manual_result` is set), stripes still render against the line numbers they'd occupy in the *computed* result. If `manual_result` is present and the user has edited beyond reconciliation with the hunks, stripes for offsets past the manual-result line count are simply not drawn. (No attempt to re-locate hunks against custom text.)

## Architecture

### New module: `src/app/three_way_header.rs`

Single public entry point:

```rust
pub struct MergeCounts {
    pub local_changes: u32,
    pub remote_changes: u32,
    pub conflicts: u32,
}

pub fn count_hunks(hunks: &[crate::merge::MergeHunk]) -> MergeCounts;

pub fn render(ui: &imgui::Ui, counts: MergeCounts);
```

`render` paints the diagram on the current window's draw list using `add_rect_filled` / `add_circle_filled` / `add_polyline` (for the diamond) / `path_bezier_cubic_curve_to` (for the curves), then positions the counts and legend with regular imgui widgets. Internal helpers stay private to the module.

The diagram constants (marker positions, sizes, curve control points) live as module-level `const` values so visual tweaks are localized to one file.

### Hunk → output-line mapping

Add a new function in `merge.rs`:

```rust
/// For each hunk, return `(hunk_id, first_line_1based, last_line_1based)` of
/// the lines the hunk will contribute to the merged output, given the current
/// resolutions. Skips hunks that resolve to zero lines.
pub fn hunk_output_ranges(
    hunks: &[MergeHunk],
    resolutions: &std::collections::HashMap<u32, Resolution>,
) -> Vec<(u32, u32, u32)>;
```

It walks the same logic as `apply_resolutions` but tracks a running line counter. Used by `result_pane` to know where to paint stripes.

### Result-pane stripes

`result_pane::render` gains two new parameters:

```rust
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    state: &mut ResultState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    hunks: &[crate::merge::MergeHunk],
    resolutions: &std::collections::HashMap<u32, crate::merge::Resolution>,
);
```

After the `input_text_multiline` renders, paint stripes on the foreground draw list at:
- `x_left = widget_min.x`, `x_right = x_left + 4.0`
- `y` per hunk = `widget_top + (first_line - 1) * lh - scroll_y` to `widget_top + last_line * lh - scroll_y`
- Clipped to the widget's rect

When `manual_result` is set on the session, the stripes still render — they reflect the *would-be* output line ranges from `hunk_output_ranges`. Stripes whose y range is outside the visible area are skipped.

`lh` comes from `ui.text_line_height()` inside the mono font scope, matching how merge_view computes it.

### Caller integration

In `mod.rs::current_session_summary`'s `ThreeWay` arm:

```rust
let counts = three_way_header::count_hunks(hunks);
three_way_header::render(ui, counts);
ui.separator();
```

Inserted before the existing `pane_header_bar` call.

The `result_pane::render` call already has `hunks` and `anchors` in scope from the session snapshot; `resolutions` lives on the session — pass them through.

### Color choices reference

| Role | Function | Used for |
| --- | --- | --- |
| Remote | `theme::SAPPHIRE()` | RemoteOnly hunk tint, ribbon, diagram marker, legend, result stripe |
| Base | `theme::YELLOW()` | Diagram marker, legend, Base-resolution result stripe |
| Local | `theme::GREEN()` | LocalOnly hunk tint, ribbon, diagram marker, legend, result stripe |
| Conflict | `theme::RED()` | Conflict hunk tint, ribbon, conflict count, unresolved result stripe |
| Merge / neutral | `theme::OVERLAY1()` | Diagram merge diamond, legend, custom-resolution result stripe |

## Files touched

- `src/app/three_way_header.rs` *(new)* — `MergeCounts`, `count_hunks`, `render`. ~150 LOC.
- `src/app/mod.rs` — `mod three_way_header;` declaration; call the new renderer in the `ThreeWay` arm of `current_session_summary`; pass `hunks` + `resolutions` to `result_pane::render`.
- `src/app/merge_view.rs` — change tint colors in `paint_pane_text` and ribbon colors in `ribbon_color` per the table above. (No structural change to the rendering pipeline.)
- `src/app/result_pane.rs` — extended signature; paint accent stripes after the multiline build.
- `src/merge.rs` — add `hunk_output_ranges`.

## Testing

- **Unit:** `merge::tests::hunk_output_ranges_*`
  - `stable_hunk_uses_text_lines`: a Stable hunk with N text lines occupies N output lines.
  - `local_only_default_resolution_uses_local_lines`: LocalOnly with no override uses `local.len()` output lines.
  - `local_only_base_resolution_uses_base_lines`: same hunk with `Resolution::Base` uses `base.len()`.
  - `conflict_resolutions_match_chosen_side`: one assertion per resolution variant.
  - `multiple_hunks_total_matches_apply_resolutions`: sum of ranges equals line count of `apply_resolutions` output.
- **Unit:** `three_way_header::tests::count_hunks_categorizes_correctly` — a synthetic mix of hunk kinds returns the right counts.
- **Manual verification:** visual inspection with a known 3-way example (the cli_args/common/editor/game/asd repro from this session works well).

## Out of scope

- Editable filename rendering in the new header strip (filenames stay in `pane_header_bar`).
- Click-to-resolve from the diagram (e.g. clicking the green circle to pick Local for all conflicts).
- Per-pane filename coloring matching the legend.
- Reworking the 2-way view to use the new color vocabulary.
- Re-mapping stripes onto the manual-result buffer when the user edits past hunk boundaries.
