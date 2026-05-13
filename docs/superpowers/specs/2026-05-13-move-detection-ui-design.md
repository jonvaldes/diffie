# Move Detection UI — Design

Status: proposed
Date: 2026-05-13

## Goal

Render move-tagged hunks in the 2-way diff view so users can see which
deleted blocks of lines reappear elsewhere as inserts, distinguish them
from ordinary edits, and jump from one half of a move to the other.

The underlying detection landed in a prior branch (commits
`a446194..e541a79`): `DiffOp::Delete` and `DiffOp::Insert` already carry
`move_id: Option<u32>`, and the histogram engine wires the
`detect_moves` post-pass into `recompute_two_way` when
`opts.detect_moves` is true. This spec covers only the renderer changes
needed to surface those tags to the user.

## Non-goals

- Move detection in the 3-way merge view (`merge_view.rs`,
  `recompute_three_way`).
- Exposing `move_min_lines` or the similarity threshold in the
  Preferences dialog. The default (`move_min_lines = 3`) stays.
- Keyboard shortcuts for move navigation, "next move / previous move"
  multi-step navigation, or a moves-overview panel.
- Right-click context menus. This app does not have context menu
  infrastructure today and v1 does not add any.
- Snapshot/pixel-level rendering tests. Verification stays at the
  logic level plus manual eyeballing, matching existing test discipline.
- Move-aware merge semantics. The `↕` jump button is purely
  navigational. Apply A→B and B→A on a moved hunk behave exactly the
  same as on any other change hunk — the `move_id` is informational.

## Architecture

All changes live in three existing files:

- `src/app/diff_view/common.rs` — new helpers:
  - `move_color() -> [f32; 4]` — returns `theme::PEACH` so the call
    sites do not hard-code a palette pick.
  - `move_ribbon_alpha(dy_px: f32) -> f32` — distance-based fade.
  - `find_paired_hunk<'a>(hunks: &'a [Hunk], move_id: u32, my_side: Side) -> Option<&'a Hunk>`.
  - `hunk_move_id(hunk: &Hunk) -> Option<u32>` — returns the `move_id`
    of the first non-Equal op in the hunk, or `None` for pure-Equal
    hunks or hunks whose change ops are untagged.

- `src/app/diff_view/render.rs` — call sites:
  - Extend the row-background block (currently `match row.cls { ... }`
    around line 408) to overlay the peach tint when the row belongs to
    a moved op.
  - Extend the ribbon loop (around line 65) so moved hunks paint a
    peach bezier whose alpha is computed by `move_ribbon_alpha`.
  - Extend the hover overlay panel (around lines 200–250) so moved
    hunks get a third `↕` button.

- `src/app/mod.rs` (`AppState`) — add a transient flash field:
  `flash: Option<MoveFlash>` where
  `struct MoveFlash { session_id: SessionId, hunk_id: u32, frames_remaining: u8 }`.

No new file. No new public API beyond the helpers in `common.rs`.

## Data flow

1. `recompute_two_way` already tags ops with `move_id` and
   `group_into_hunks` keeps consecutive Delete or Insert ops together,
   so a moved Delete run on side A becomes one hunk and the paired
   Insert run on side B becomes a separate hunk. The two hunks carry
   the same `move_id` on their ops.
2. The renderer reads `hunk_move_id(hunk)` per hunk and treats the
   whole hunk as "moved" if it returns `Some(_)`.
3. Pairing is computed by `find_paired_hunk(hunks, move_id, my_side)`,
   which linearly scans the hunks list looking for one on the opposite
   side carrying the same `move_id`. Returns `None` if no pair exists
   (this can only happen if a future engine emits a half-stamped run;
   the current `detect_moves` always stamps in pairs).

The renderer does not cache pairings. The hunks list is small enough
(usually <100 entries) that a per-frame O(n) scan per moved hunk is
cheap.

## Visual encoding

### Row background

When a row corresponds to a Delete or Insert op whose `move_id` is
`Some(_)`, the row background is `theme::with_alpha(theme::PEACH,
0.30)` — same alpha as the existing red/green tints so the visual
weight matches. This replaces the current red/green tint for moved
rows; it does not stack on top of it. Equal rows are unaffected. The
hover tint and selection rect already paint on top of the row bg in
the existing code; they continue to do so unchanged.

Text foreground colors are unchanged. The bg shift carries the signal
on its own.

### Ribbon

The bezier ribbon between a moved Delete hunk and its paired Insert
hunk uses `theme::PEACH` rather than `theme::BLUE`. Geometry is
identical to today's `fill_bezier_ribbon` — no new control-point math.

The ribbon's alpha is distance-faded:

```rust
const RIBBON_ALPHA_NEAR: f32 = 0.30;
const RIBBON_ALPHA_FAR: f32 = 0.08;
const RIBBON_FADE_RANGE_PX: f32 = 800.0;

fn move_ribbon_alpha(dy_px: f32) -> f32 {
    let t = (dy_px.abs() / RIBBON_FADE_RANGE_PX).clamp(0.0, 1.0);
    RIBBON_ALPHA_NEAR + (RIBBON_ALPHA_FAR - RIBBON_ALPHA_NEAR) * t
}
```

`dy_px` is the absolute vertical distance between the screen
y-midpoint of the moved Delete (on the left pane) and the screen
y-midpoint of the moved Insert (on the right pane), computed inside
the existing ribbon paint loop where both endpoints are already in
hand.

The constants are private to `common.rs`. They are not exposed via
Preferences in v1.

The fade applies only to the ribbon. Row backgrounds stay at fixed
0.30 alpha so users can still identify moved rows when scrolled away
from the paired half.

### Hover overlay (jump button)

The existing per-hunk hover overlay panel is currently 200 px wide
with two `small_button`s (Apply A→B and B→A). When the hovered hunk
is moved (`hunk_move_id(hunk).is_some()` AND `find_paired_hunk(...)`
returns `Some`), the panel widens to 240 px and renders a third
`small_button` labelled `↕`. The button's tooltip reads
`"Jump to paired half (line N)"` where N is the paired hunk's
`a_range.0` or `b_range.0` depending on the destination side.

If `hunk_move_id` is `Some(id)` but no pair is found in the hunks
list, the `↕` button is not rendered. This is defensive against
half-stamped engine output; it should be unreachable today.

## Jump behaviour

Clicking `↕` is pure view state — nothing about the file or session
changes — so it uses a new transient field on `AppState`, NOT the
`DiffEdit` undo-stack channel:

```rust
struct PendingJump {
    session_id: SessionId,
    pane: Side,           // which pane to scroll
    target_line: LineNo,  // 1-based; center this line in the pane
}

// in AppState:
pending_jump: Option<PendingJump>,
flash: Option<MoveFlash>,
```

When the user clicks the button, the renderer writes
`pending_jump = Some(PendingJump { session_id, pane: opposite_side,
target_line })`. The render loop, in the section that already calls
`SetNextWindowScroll` for center-anchored scroll sync, checks
`pending_jump` once per frame: if set, computes the target scroll
offset via the existing `target_scroll` helper, calls
`SetNextWindowScroll` on the opposite pane, then clears
`pending_jump`. The flash field is set at the same time:
`flash = Some(MoveFlash { session_id, hunk_id: paired.id,
frames_remaining: 30 })`.

The flash overlay is painted by the row-rendering loop: when
`state.flash` matches `(session_id, hunk_id)` for the row being drawn,
add an extra rect `theme::with_alpha(theme::PEACH, alpha)` where
`alpha = 0.20 * frames_remaining as f32 / 30.0`. The renderer
decrements `frames_remaining` at the end of each frame; when it hits
0, the flash field is cleared. 30 frames is ~0.5 s at 60 Hz, ~1 s at
30 Hz — the flash is brief either way.

Reciprocal navigation works without special bookkeeping: the jumped-to
pane's hunk has its own hover overlay with a `↕` button, so the user
can ping-pong between halves with two clicks.

## Hunk grouping and existing scaffolding (no changes)

- `group_into_hunks` is not touched. Moved Delete and moved Insert
  runs already land in separate hunks because they appear at
  non-adjacent positions in the op stream.
- The Preferences dialog's "Detect moves" checkbox
  (`src/app/mod.rs:1128`) is unchanged.
- The per-tab engine bar's "Detect moves" checkbox
  (`src/app/engine_bar.rs:90-104`), gated on engine capability, is
  unchanged.
- `Apply A→B` / `B→A` behaviour on moved hunks is unchanged. The
  `move_id` is informational; the resolution semantics are the same
  as for any other change hunk.

## Testing

### Unit tests (logic-level)

In `src/app/diff_view/common.rs::tests`:

1. **`move_ribbon_alpha_at_zero_returns_near`** — `move_ribbon_alpha(0.0)` equals `0.30`.
2. **`move_ribbon_alpha_at_fade_range_returns_far`** — `move_ribbon_alpha(800.0)` equals `0.08`.
3. **`move_ribbon_alpha_clamps_above_fade_range`** — `move_ribbon_alpha(5000.0)` equals `0.08`.
4. **`move_ribbon_alpha_clamps_below_zero`** — `move_ribbon_alpha(-100.0)` equals `0.30` (negative dy is treated as |dy|).
5. **`hunk_move_id_pure_equal_returns_none`** — a hunk with only `DiffOp::Equal` ops returns `None`.
6. **`hunk_move_id_untagged_change_returns_none`** — a change hunk whose ops have `move_id: None` returns `None`.
7. **`hunk_move_id_reads_first_change_op`** — a change hunk whose first non-Equal op carries `Some(7)` returns `Some(7)`.
8. **`find_paired_hunk_returns_opposite_side_match`** — build a Vec<Hunk> with a Delete-side hunk tagged `move_id: 0` and an Insert-side hunk tagged `move_id: 0`; both lookups (querying from A side and from B side) return the opposite hunk.
9. **`find_paired_hunk_returns_none_for_unpaired_id`** — query for a `move_id` that does not appear in the hunks list returns `None`.

### Integration test (renderer-layer logic)

In `src/app/diff_view/tests.rs`:

10. **`session_with_move_produces_paired_hunks_with_matching_id`** —
    open a 2-way session via `SessionStore::open_two_way_with(...)`
    using the same `hdr/blk/ftr` inputs as the
    `session_pipeline_tags_moves_when_engine_supports_them` test in
    `moves.rs`, with `detect_moves: true` and `move_min_lines: 2`.
    Snapshot the hunks. Assert:
    - Exactly two hunks carry a non-`None` `move_id` (one on each side).
    - Both carry the same id.
    - `find_paired_hunk(hunks, id, Side::A)` returns the B-side hunk,
      and vice versa.

### No rendering snapshot tests

The renderer paints directly to `imgui::DrawList`, which doesn't expose
a frame capture path without a real GPU. Visual verification is
manual.

### Manual verification checklist (lives in the plan, not the spec)

- Toggle "Detect moves" in the per-tab engine bar; rows turn peach on
  both sides immediately.
- Open a synthetic A/B pair with a known moved block; confirm peach
  rows on both sides and a peach bezier connecting them.
- Click `↕`; opposite pane scrolls so the paired hunk is centered and
  its rows briefly flash a darker peach.
- With the cursor in the middle of a long file, confirm that ribbons
  spanning a viewport or more render at the faded alpha; ribbons
  spanning short distances render at the near alpha.

## Risks and open questions

- **Peach vs hover/selection contrast.** The hover tint is
  `theme::TEXT @ 0.04` and the selection rect is `theme::BLUE @ 0.40`.
  Both paint over the row background. Peach at 0.30 should compose
  cleanly with both, but it is worth eyeballing during manual
  verification — if hover-on-peach becomes hard to distinguish, the
  hover alpha is a one-line tweak.
- **Flash with FPS variance.** 30 frames is a fixed count, not a wall
  clock. On a 30 Hz display the flash lasts ~1 s; on a 120 Hz display
  ~0.25 s. This is acceptable for a UI affordance and matches how
  other transient overlays in the codebase work. If it bothers users,
  switch to wall-clock decay later.
- **Long ribbons across many moved pairs.** If a file has many
  scattered moves, the distance-fade keeps the visual cleaner but
  cannot eliminate crossings entirely. A future "moves overview" lane
  (separate column showing arrows by line number) would be the better
  fix; out of scope for v1.

## Out of scope (follow-ups)

- Expose `move_min_lines` and a "tune move detection" sub-section in
  Preferences.
- Moves overview side lane.
- Keyboard shortcut for jump (e.g. Ctrl+J).
- "Next move / previous move" toolbar buttons.
- Move-aware 3-way merge semantics (a moved-and-edited block in a
  3-way diff has interesting resolution choices that need their own
  spec).
- Apply both halves at once ("undo the move" / "accept the move")
  shortcut.
