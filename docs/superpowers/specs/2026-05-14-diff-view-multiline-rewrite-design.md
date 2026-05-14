# Diff View Multi-line Widget Rewrite — Design

Status: proposed
Date: 2026-05-14

## Goal

Replace the 2-way diff view's per-row `input_text` widgets with one
`input_text_multiline` per pane, and rewire the per-row visual
decorations (backgrounds, sub-line spans, hover overlays, anchor
gutter) as draw-list overlays on top of the multiline widget. Apply
the same change to the merge view's base/local/remote panes.

The current architecture pairs N single-line `input_text` widgets
stacked vertically with a parallel cross-row state machine
(`state.selection`, `state.drag`, suppression callbacks, post-build
clipboard hooks, splice handlers, char filters) that re-implements
what `input_text_multiline` does natively. Each multi-line editing
feature added in the last six commits has been a workaround for the
same root mismatch. This spec swaps the topology so imgui owns the
editing semantics natively.

## Non-goals

- Soft-wrap, diff-view-level search, line-number jump targets, touch
  input. Out of scope.
- Changes to the diff engine, move detection, sub-line span
  computation, anchor model, decision/resolution state machines.
  Engine layer is orthogonal.
- The Result pane in 3-way mode is already `input_text_multiline`
  and is unchanged.

## Architecture

### Session storage flips to `String` per side

`SessionMode::TwoWay` and `SessionMode::ThreeWay` change:
- `a_lines: Vec<String>` → `a_text: String`.
- `b_lines: Vec<String>` → `b_text: String`.
- 3-way: `base_lines`/`local_lines`/`remote_lines` → `base_text`/`local_text`/`remote_text`.

The diff engine still consumes `&[&str]`. `recompute_two_way` (and
`recompute_three_way`) calls `text.split('\n').collect::<Vec<_>>()`
before each diff call. The split is cheap on inputs of the size
diffie handles (already done at load time via `read_text` →
`split_lines`).

Each side records a `trailing_newline: bool` (new field on the
session mode struct) captured at load time so save preserves the
original convention. The in-memory `String` does NOT carry a
trailing newline. `read_text` (in `io.rs`) is extended to return
`(text_without_trailing_newline, trailing_newline_flag)`; `write_text`
appends `\n` based on the flag.

### Edit types collapse

Add one, drop two, keep one:

```rust
pub enum DiffEdit {
    SetSide {
        session_id: SessionId,
        side: SideRef,                 // TwoWaySide or ThreeWaySide
        new_text: String,
        old_text: Option<String>,
    },
    ReplaceHunkSide { ... },           // KEPT — Apply A→B / B→A
    // DELETED: SetTwoWayLine
    // DELETED: SpliceTwoWayLines
}
```

`SetSide` is emitted on every change reported by `input_text_multiline`'s
`changed = true` return. The existing `DiffEdit::merge` method on
the undo stack (today merges consecutive `SetTwoWayLine` on the same
`(session_id, side, line_no)`) gets a new arm: consecutive `SetSide`
entries on the same `(session_id, side)` merge by keeping the latest
`new_text` and the earliest `old_text`. Same merging trigger logic
as today (back-to-back edits with no intervening break);
arrow-keys/clicks/blur push a break by calling the existing
`Record::end_session` helper.

`SideRef` is a new enum unifying side-naming across edit types:

```rust
pub enum SideRef {
    TwoWay(TwoWaySide),       // A | B (existing enum)
    ThreeWay(ThreeWaySide),   // new enum: Base | Local | Remote
}
```

`ThreeWaySide` is added in `session.rs`; today three-way uses
three separate field accesses.

`ReplaceHunkSide` (Apply A→B) gains a new helper:

```rust
fn replace_hunk_in_text(text: &mut String, line_range: Range<u32>, replacement: &str);
```

It computes byte offsets in `text` by walking `text.lines()` to the
line at `line_range.start` (1-based), accumulates byte length plus 1
per line for the `\n`, then splices `replacement` into the
corresponding byte range. ASCII-safe, UTF-8 safe.

### Diff view layout per side, left-to-right

1. **Anchor gutter strip** (~60px, full pane height). A separate
   `invisible_button` over its rect captures clicks. On click,
   compute `line_no = (mouse_y - gutter_top + scroll_y) / line_h`
   and set `state.pending_a` / `state.pending_b`. Line numbers and
   anchor dots are drawn into the foreground draw list.
2. **`input_text_multiline` widget**: spans the rest of the pane.
   Buffer is a `String` mirrored from `session.{a_text, b_text}` at
   the start of each frame. Width: `pane_w - gutter_w`. Height:
   `pane_visible_h`. The widget owns scroll, selection, copy, cut,
   paste, Enter, arrow nav, undo (we disable imgui's per-widget
   undo via `.no_undo_redo(true)` and own undo at the diff level).

The connector strip in the middle (60px) is unchanged.

### Draw-list overlays for row decorations

Run AFTER the multiline widget builds. For each pane, compute
visible-line range from the widget's scroll_y and content rect:

```rust
let line_h = ui.text_line_height();                 // honors current font
let first_line = (scroll_y / line_h).floor() as u32 + 1;
let last_line  = ((scroll_y + visible_h) / line_h).ceil() as u32;
```

For each visible line `n`, compute screen y:

```rust
let y = widget_content_top + (n - 1) as f32 * line_h - scroll_y;
```

A new helper `paint_row_overlays(ui, widget_rect, hunks, scroll_y,
line_h, side)` walks the hunks list and:

- Paints per-row backgrounds (Equal: none; Delete: red @ 0.30;
  Insert: green @ 0.30; Moved: peach @ 0.30) clipped to the widget
  rect.
- Paints sub-line spans by walking each Delete/Insert op's `spans`
  and emitting `text_x_at_byte`-keyed rects.
- Detects mouse hover over change-hunk lines and sets a `hover_out`
  cell with the hunk id + anchor position for the overlay panel.

### Hover overlay (Apply A→B / B→A / ↕)

Drawn after `paint_row_overlays` reports a hover. Position:
hunk's first visible line's screen y. Three `small_button` widgets
(`Apply A→B`, `B→A`, `↕` when moved). Click handlers queue
`DiffEdit::ReplaceHunkSide` or `state.pending_jump`. Z-order: these
widgets are constructed AFTER the multiline widget, so imgui's
construction-order priority gives them click priority.

### Scroll sync

The multiline widget's content scroll is read via `ui.scroll_y()`
inside its internal child window. Set via `igSetNextWindowScroll`
before the widget builds. The existing `target_scroll` helper in
`common.rs:659` (maps content-y between panes via hunk ranges) is
untouched.

If `igSetNextWindowScroll` does NOT apply to the multiline widget's
internal child (likely-but-not-confirmed), fall back to scrolling
inside an `ALWAYS` callback via `data.set_scroll_y(...)`. This
contingency is verified by a spike task at the start of the plan.

### Merge view changes

The base/local/remote panes become `input_text_multiline` and gain
editability (per user direction). New edit type:

```rust
DiffEdit::SetThreeWaySide { session_id, side: ThreeWaySide, new_text, old_text }
```

The same `paint_row_overlays` machinery applies. The Result pane
(`result_pane.rs`) is already multiline and is unchanged.

## What gets deleted

Cross-row machinery in `app/`:

- Entire `app/input.rs` module (`InputFrame`, `Selection`, `DragState`,
  `selection_step`, etc.) and its ~10 unit tests.
- Entire `app/input_imgui.rs` (the `from_ui` adapter).

In `diff_view/`:

- `state.selection`, `state.drag` fields on `DiffViewState`.
- `update_selection`, `build_selection_splice`,
  `build_selection_replace_splice`, `extract_selection_text`,
  `select_all`.
- `compute_enter_split`, `compute_paste_split` (+ 11 helper tests).
- `chars_typed` and the `CHAR_FILTER` callback capture.
- `suppress_imgui_selection`, `multi_row_selection_on_this_side`,
  `drag_on_this_side_past_threshold`.
- `pending_paste`, `paste_target`, `replace_selection_with`,
  `compute_*_split` plumbing in `draw_row`.
- `shift_arrow_extend`, `clear_state_selection`, `arrow_focus`.
- `pin_scroll_x_after_splice`, `input_epoch` bump on splices.
- The whole `draw_row` per-row pipeline.

In `app/mod.rs`:

- `do_copy`, `copy_enabled`, the post-build do_copy hook, the
  `PendingKey` enum and `inject_pending_key`.
- The Ctrl+C / Ctrl+A handlers in `keyboard_shortcuts` (imgui native
  inside the widget). Ctrl+Z / Ctrl+Shift+Z stays as it routes
  through the app-level undo stack.

In `merge_view.rs`:

- Cross-row selection state (`Selection`, `extract_selection_text`).
- The custom per-row `draw_row` (replaced by overlays).

Old headless tests deleted in their entirety (~25 tests, all listed
in Section 5 of the brainstorm record).

## What's preserved

Engine layer (unchanged):

- `src/diff/*` (engines, moves, sub_line, normalize, anchored).
- `src/merge.rs`.

Session layer (changed storage; logic preserved):

- Two-way / three-way mode classification.
- Hunk grouping, anchor model.
- Decision/resolution state (`HunkDecision`, `Resolution`).
- Manual result for 3-way.

UI features preserved (rewired to overlay-based painting):

- Connector ribbons (`draw_connector`).
- Per-row backgrounds.
- Sub-line span highlights.
- Hover overlay panel with Apply A→B / B→A / ↕.
- Anchor click-to-pin on the gutter.
- Move detection visualization (peach rows + ribbon + ↕).
- Tabs, recents, preferences, engine bar.

## Testing strategy

### Deletion is gated on equivalent new tests passing

For each behavior covered by an old test, the plan adds a new test
that exercises the SAME behavior via the new model FIRST, gets it
passing, THEN deletes the old. Per-task in the implementation plan:

1. Storage refactor (session → String). Adapt existing session/diff
   tests to compile; add new tests for `SetSide` undo round-trip,
   coalescing, and `trailing_newline` preservation.
2. Diff view UI rewrite to `input_text_multiline` with overlays.
   Old tests asserting on `view_state.selection` etc. WILL break.
   They are temporarily marked `#[ignore]` during the rewrite so
   the harness compiles.
3. Add equivalent new tests (one per behavior), get green.
4. Delete the now-obsolete `#[ignore]`'d tests AND the helper
   modules they consumed (the deleted-list above).

The window where deleted tests are ignored is bounded: a single
implementation pass.

### Behavior coverage map (new tests)

| Behavior | New test (asserts on observable side effects) |
|---|---|
| Drag-select across rows + Ctrl+C | Drag, Ctrl+C, assert clipboard contains multi-line text matching the dragged range |
| Drag + Ctrl+X | Drag, Ctrl+X, assert `a_text` spliced AND clipboard contains the cut text |
| Type a char with selection | Drag, type 'X', assert `a_text` has the cross-row range replaced with "X" |
| Paste multi-line with selection | Drag, Ctrl+V (clipboard has "X\nY"), assert `a_text` has the range replaced with multi-line text |
| Enter at caret | Click in row, press Enter, assert `a_text` gains a `\n` at the caret position |
| Multi-line paste at caret | Click in row, Ctrl+V (multi-line clipboard), assert `a_text` gains the pasted text |
| Shift+Down extends + Ctrl+C | Click, Shift+Down, Ctrl+C, assert clipboard has 2 lines |
| Apply A→B on a change hunk | Click `Apply A→B` button, assert `b_text` reflects the splice |
| ↕ jump-to-pair on moved hunk | Click `↕`, assert opposite pane scrolls to paired half AND flash starts |
| Scroll sync | Programmatically scroll pane A, assert pane B's scroll target is computed correctly |

### New unit tests

- `replace_hunk_in_text` byte-range math: ASCII single-line, ASCII
  multi-line, UTF-8 multi-byte content (e.g. `"αβγ"` line replaced).
- `SetSide` coalescing: two consecutive `SetSide` within 500ms on
  the same side collapse to one undo entry whose `old_text` is the
  pre-first-edit content.
- Anchor-gutter line lookup: `mouse_y → line_no` for varied scroll
  positions.
- Per-row overlay y math: given hunks and scroll_y, the computed
  y for line N matches expected.

### Manual verification (in the implementation plan)

- Drag-select across lines copies multi-line text.
- Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+A all do the obvious things.
- Enter at caret inserts a newline; line count goes up.
- Typing replaces an active selection.
- Apply A→B on a change hunk works; ↕ jump still works.
- Scrolling one pane syncs the other.
- Anchor clicks on the gutter still set anchors.
- Tabs, preferences, recents, save/load all still work.
- Merge view: editing base/local/remote propagates through the
  3-way merge result.

## Risks & mitigations

### Major

**Scroll sync inside the multiline widget.** `igSetNextWindowScroll`
applied before `input_text_multiline.build()` may target the parent
window instead of the widget's internal child. Mitigation: Task 1
of the implementation plan is a scroll-sync spike. If
`igSetNextWindowScroll` doesn't work, fall back to scrolling inside
an `ALWAYS` callback via `data.set_scroll_y(...)` (the callback
runs inside the widget's child context).

**Line-y math under font/zoom changes.** If imgui's multiline widget
rounds line height differently than our `row_h()`, overlay rects
drift sub-pixel as the user zooms. Mitigation: derive `line_h` from
`ui.text_line_height()` inside the widget's font scope; never
hard-code.

**Anchor gutter hit-testing.** The gutter `invisible_button` must
not consume mouse events the multiline widget needs. Mitigation:
position the gutter strictly to the left of the widget with no
overlap.

### Moderate

**`SetSide` coalescing granularity.** Today's `DiffEdit::merge`
collapses consecutive edits with no time bound — they merge
whenever they touch the same `(session_id, side, line_no)` and
nothing else has been recorded between them. The new arm follows
the same rule: consecutive `SetSide` on the same `(session_id,
side)` merge unconditionally. The existing `Record::end_session`
points (already called on blur, undo, manual save) flush the
coalesce. Same model, not a regression.

**Mixed line endings.** Source files might use `\r\n` internally
despite `split('\n')` semantics. Mitigation: normalize at load
(already done by `read_text`); preserve the load-time convention on
save.

### Minor

**Hover overlay z-order.** The `↕` / Apply buttons are constructed
after the multiline widget; imgui's construction-order priority
gives them click priority. Mitigation: confirmed by design — no
action needed.

## Migration strategy

Big-bang replace in a single branch. Each implementation task
leaves `cargo build` green and tests passing-or-`#[ignore]`'d. The
sequence is roughly:

1. Spike: confirm `igSetNextWindowScroll` / `ALWAYS callback
   set_scroll_y` works on `input_text_multiline`. Time-box ~1 day.
2. Session storage refactor: `a_lines: Vec<String>` →
   `a_text: String`. Update all consumers (`recompute_*`,
   `compute_result`, save, splice helpers). Adapt existing session
   tests; add `SetSide` tests.
3. New edit types (`SetSide`, `SetThreeWaySide`). Wire through the
   undo stack with coalescing.
4. Diff view UI rewrite: two `input_text_multiline` widgets per
   2-way tab; gutter + connector stays; mark old per-row tests
   `#[ignore]`.
5. Implement `paint_row_overlays` (backgrounds, sub-line spans,
   hover detection).
6. Hover overlay panel (Apply A→B / B→A / ↕) on top of the new
   layout.
7. Anchor gutter clicks.
8. Scroll sync between the two multiline widgets.
9. Add equivalent new tests for each behavior listed in the
   coverage map; get green.
10. Merge view: apply the same pattern to base/local/remote.
11. Delete `#[ignore]`'d old tests AND the helper modules they
    consumed (the deleted-list in `What gets deleted`).
12. Final pass: prune `app/mod.rs`'s now-unused helpers (`do_copy`
    if Ctrl+C goes through imgui, `PendingKey` if `inject_pending_key`
    has no callers, etc.).

LOC delta estimate: -1500..-2000 (deleted machinery) +800..1200
(new overlay painters, gutter widget, edit type, storage adapters)
= net ~-500..-1000 LOC, plus ~25-30 tests deleted and ~15 added.

## Out of scope (explicit)

- Soft-wrap of long lines.
- Diff-view search/find.
- Line-number gutter clicks beyond anchor pinning.
- Touch / gesture input.
- 4-way diffs.
- Adding a search/replace overlay using the new line-y math
  infrastructure (could be a future follow-up that builds on
  `paint_row_overlays`).
