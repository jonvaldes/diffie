# Hover-to-Anchor UX (2-way)

A direct-manipulation way to create and remove diff anchors in the 2-way view. Anchor icons live in the bezier-ribbon strip between the two panes; clicking one starts an "anchor mode" where a bezier follows the mouse until the user clicks a target icon on the opposite side (creating the anchor) or cancels.

Scope: **2-way only**. 3-way (`MergeAnchor`) is deferred — its three-pick flow needs its own design.

## Background

The core already supports anchored diffs:

- `diff::Anchor { a: LineNo, b: LineNo }` pins a line pair.
- `diff::anchored::AnchoredDiff` wraps any engine and forces matches at the supplied anchors.
- `SessionStore::add_anchor_two_way(id, anchor)` and `remove_anchor(id, idx)` insert/remove and recompute hunks atomically.

The only existing UI surface for anchors is small "gutter dots" rendered by `overlay::paint_gutter` for anchored lines, plus a half-finished two-click prototype using `DiffViewState::pending_a` / `pending_b` (currently unwired). This spec replaces both with a single, discoverable interaction.

## User-facing behavior

### Icon placement

- Two vertical **rails**, one per pane, sit inside the 60 px connector strip between panes — left rail flush against the left pane's right edge, right rail flush against the right pane's left edge. Each rail is one icon (~`lh` px) wide.
- For pane row at content line `n` with eased scroll `s`, the icon's vertical center is `pane_top + (n - 1) * lh + lh/2 - s` (the existing `line_screen_y` helper).
- Glyph: nerd-font anchor (`\u{f13d}`), already loaded in the mono font.

### Icon states

| Line state                | Idle rendering                | Picking-mode rendering                                   |
| ------------------------- | ----------------------------- | -------------------------------------------------------- |
| Not anchored, not hovered | nothing                       | nothing                                                  |
| Not anchored, hovered     | outline icon                  | outline icon (valid target if opposite side)             |
| Anchored, not hovered     | filled icon (anchor color)    | filled icon                                              |
| Anchored, hovered         | filled icon + "remove" cursor | filled icon (no-op on click if opposite side, see below) |

The existing `paint_gutter` anchor dots are removed — the rails replace them.

### Interaction state machine

Stored on `DiffViewState`:

```rust
enum AnchorPick {
    Idle,
    Picking { side: Side, line: LineNo },
}
```

Transitions (event → action / next state):

| From      | Event                                                              | Action                                                        | To                            |
| --------- | ------------------------------------------------------------------ | ------------------------------------------------------------- | ----------------------------- |
| `Idle`    | click on unanchored icon, side `S`, line `n`                       | —                                                             | `Picking { S, n }`            |
| `Idle`    | click on anchored icon, side `S`, line `n`                         | `session.remove_anchor(idx_of_matching_anchor)`               | `Idle`                        |
| `Picking` | press `Esc`                                                        | —                                                             | `Idle`                        |
| `Picking` | click in empty space (neither rail)                                | —                                                             | `Idle`                        |
| `Picking` | click on opposite-side icon, line `m`, that line is **unanchored** | `session.add_anchor_two_way(Anchor::with_sides(S, n, !S, m))` | `Idle`                        |
| `Picking` | click on opposite-side icon, line `m`, that line is **anchored**   | no-op (ambiguous; user must remove existing first)            | `Picking` (unchanged)         |
| `Picking` | click on same-side icon, line `m`                                  | replace the source line                                       | `Picking { S, m }`            |
| `Picking` | every frame while held                                             | draw black cubic bezier from source icon center to mouse pos  | unchanged                     |

`Anchor::with_sides(left_line, right_line)` maps to the `Anchor { a, b }` ordering (`a` = left/A side, `b` = right/B side) regardless of which side the user picked first.

### In-progress visual

While `Picking`, each frame draws one cubic bezier (reusing `overlay::stroke_bezier_curve`) from the picked icon's center to `ui.io().mouse_pos`, in `theme::CRUST()` (black-on-light / near-black on dark — matches existing anchor curves). No fill; same stroke weight as anchor curves today (3.0).

## Implementation

### Hit testing

In the connector layout (currently a single `dummy([CONNECTOR_W, pane_h])`), split the strip into three sub-regions laid out horizontally:

1. left rail — `invisible_button("anchor_rail_L", [rail_w, pane_h])`
2. middle ribbon area — `dummy` (unchanged hit behavior)
3. right rail — `invisible_button("anchor_rail_R", [rail_w, pane_h])`

`rail_w = lh` (square icons). On rail hover, query `ui.io().mouse_pos[1]` and call `mouse_y_to_line(mouse_y, pane_top, last_left_scroll_y_or_right, lh)` to find the candidate line. A click anywhere else inside the diff view that isn't on a rail counts as "click in empty space" and cancels.

`Esc` is read via `ui.is_key_pressed(Key::Escape)` at the top of the 2-way render, gated on `is_window_focused`.

### Draw order

Per frame, inside the diff-view window:

1. Both panes render (text, gutters, sub-line spans). Gutter no longer paints anchor dots.
2. Connector ribbons paint (`paint_ribbons`, unchanged).
3. `paint_anchor_rail(Side::Left, ...)` and `paint_anchor_rail(Side::Right, ...)` paint over the ribbons.
4. If `AnchorPick::Picking`, paint the live bezier on top of the rails.
5. Hover panel (`paint_hover_overlay`) paints last, unchanged.

### Files touched

- `src/app/diff_view/common.rs`
  - Add `AnchorPick` enum + field on `DiffViewState`.
  - Remove `pending_a` / `pending_b` (dead prototype).
- `src/app/diff_view/mod.rs`
  - Reserve rail strips in the connector layout.
  - Read rail hover/click + `Esc` and drive `AnchorPick`.
  - Call `session.add_anchor_two_way` / `remove_anchor` on completion.
- `src/app/diff_view/overlay.rs`
  - New `paint_anchor_rail(side, rail_rect, anchors, hover_line, picking, lh, scroll_y)`.
  - Strip anchor-dot logic out of `paint_gutter`.
  - Expose the icon center for a given anchor endpoint (used both by `paint_anchor_rail` and by the live-bezier painter).
- `src/app/diff_view/tests.rs`
  - `anchor_pick_transitions_idle_to_picking_on_click`
  - `anchor_pick_picking_to_idle_on_esc`
  - `anchor_pick_picking_completes_on_opposite_side_click`
  - `anchor_pick_picking_no_op_on_anchored_opposite_target`
  - `anchor_pick_picking_replaces_source_on_same_side_click`
  - `anchor_rail_icon_y_matches_line_screen_y` (geometry sanity)

### Non-goals / explicitly out

- 3-way `MergeAnchor` UX (separate spec).
- Tooltips / labels on anchor icons.
- Drag-to-anchor (click-then-click only; matches the existing event plumbing).
- Keyboard-driven anchor creation.
- Persistence — anchors are already session-scoped; no change.

## Testing strategy

Pure logic (state machine transitions, icon-y arithmetic) is unit-tested in `diff_view/tests.rs` against a synthetic `DiffViewState` and `Anchor` list. Imgui-side hit testing is exercised manually — there is no automated harness for `winit`/`imgui` event flow in the project today, and adding one is out of scope.

Verification checklist before declaring done:

- Hover left pane → outline icon appears on left rail at the hovered row.
- Click outline icon → black bezier follows mouse.
- Click opposite-side outline icon → anchor created, both icons fill, recompute fires.
- `Esc` mid-pick → bezier disappears, no anchor change.
- Click filled icon idle → anchor removed, recompute fires.
- Click anchored icon on opposite side mid-pick → no-op (stay in Picking).
- Click same-side icon mid-pick → source moves to the new line.
