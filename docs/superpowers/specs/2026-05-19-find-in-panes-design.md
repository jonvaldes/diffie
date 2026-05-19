# Find-in-panes — Design

Date: 2026-05-19

## Summary

Add a global "find" feature to the top bar that highlights matches across every
visible text pane (both sides of 2-way, all three sides + result of 3-way),
shows match positions as ticks on each pane's scrollbar, and jumps the caret in
the last-focused pane on Enter / Shift+Enter. Supports literal, case-sensitive,
whole-word, and regex matching.

## User-visible behavior

- **Top bar input**: a `Find` text field is added to `engine_bar.rs`, to the
  right of the existing whitespace controls. Three small toggle buttons follow
  it: `Aa` (case-sensitive), `\b` (whole word), `.*` (regex).
- **Ctrl+F** focuses the find field from anywhere in the app.
- **As the user types**, every visible pane paints highlight rectangles behind
  glyphs at every match (soft yellow fill) and thin orange ticks on its
  scrollbar at every matching line.
- **Enter** jumps the caret in the last-focused pane to the next match after
  the caret, selecting the match (caret at end). **Shift+Enter** jumps to the
  previous match. **F3 / Shift+F3** do the same without requiring the field to
  be focused. The jumped-to match gets a stronger orange fill ("current
  match").
- **Escape** while the field has focus clears the query and returns focus to
  the last-focused pane.
- **Red field** when the current query yields zero matches across all visible
  panes, OR when the regex doesn't compile. The field returns to its normal
  color as soon as the query becomes empty or starts matching again.
- **Match options persist** in user preferences; the query text does not.

## Architecture

### New state on `AppState` (`src/app/mod.rs`)

```rust
pub struct AppSearch {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub compiled: Option<CompiledQuery>,
    pub focus_request: bool,
    pub jump_request: Option<JumpDir>,
    pub total_matches: usize,
}
pub enum JumpDir { Next, Prev }
```

`CompiledQuery` lives in the new `src/app/search.rs` module and wraps a
`regex::Regex`. It is rebuilt whenever `query` or any toggle changes (tracked
with a small `dirty` flag set by the input/toggle code). An invalid regex sets
`compiled = None`; matching code treats `None` as "zero matches everywhere".

### New module `src/app/search.rs`

Engine-agnostic, no GUI deps. Compiled even with `--no-default-features` so it
can be unit-tested without the GUI feature.

```rust
pub struct CompiledQuery { pub raw: String, regex: regex::Regex }

impl CompiledQuery {
    pub fn build(
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        regex: bool,
    ) -> Result<Self, regex::Error>;
}

pub struct Match {
    pub line: u32,            // 1-based line number
    pub start_col: usize,     // char index in the line
    pub end_col: usize,       // char index (exclusive)
    pub byte_start: usize,    // byte offset in the full text
    pub byte_end: usize,
}

pub fn find_matches_in_text(text: &str, q: &CompiledQuery) -> Vec<Match>;
```

Non-regex queries are built as `regex::escape(query)`. Whole-word wraps the
final pattern in `\b…\b`. Case-insensitive prepends `(?i)`. Matches that span
line boundaries are clipped to per-line spans before returning, so consumers
only ever deal with same-line ranges.

`regex` is added as a top-level dependency (not feature-gated).

### Top bar (`src/app/engine_bar.rs`)

Append after the existing controls:

1. `ui.same_line(); ui.text("Find");`
2. A ~220px-wide `InputText` bound to `state.search.query`. On
   `is_item_edited`, set the dirty flag and recompile.
3. Three toggle buttons (`Aa`, `\b`, `.*`) that flip the corresponding option
   and trigger recompile.
4. The input's frame bg is forced to red (`push_style_color(FrameBg, …)`) when
   `(!query.is_empty() && compiled.is_none())` OR `(compiled.is_some() &&
   state.search.total_matches == 0)`.
5. At the start of the bar's frame, if `state.search.focus_request` is true,
   call `ui.set_keyboard_focus_here()` immediately before drawing the input
   and clear the flag.
6. Enter on the input (`InputTextFlags::ENTER_RETURNS_TRUE`) → set
   `jump_request = Some(Next)`; if Shift is held → `Some(Prev)`. Escape with
   the input focused clears `query` and emits a "blur" by re-routing focus to
   the last-focused pane.

### Global shortcuts (`src/app/mod.rs`)

In the existing top-level shortcut block (the one that already handles Ctrl+Z
etc.):

- `Ctrl+F` → `state.search.focus_request = true`.
- `F3` → `jump_request = Some(Next)` when `compiled.is_some()`.
- `Shift+F3` → `jump_request = Some(Prev)` when `compiled.is_some()`.

### Highlight painting

Two render paths:

#### Custom-painted panes (`diff_view/*`, `merge_view.rs`)

These already paint glyphs onto the imgui draw list (`syntax_paint.rs` /
local rect helpers). Add a new pass `paint_search_highlights` called
immediately before the glyph pass so glyphs sit on top:

- For each `Match` on a visible line, compute its screen rect from
  `(start_col, end_col)`, the cached `glyph_advance` (monospace), `line_height`,
  and the pane's scroll/origin.
- Draw filled rect with `[1.0, 0.92, 0.2, 0.35]` (soft yellow).
- If `(pane_id, match_index) == state.search.current`, draw with
  `[1.0, 0.55, 0.1, 0.7]` (strong orange) instead.

#### Native `input_text_multiline` (`result_pane.rs`)

The result pane uses imgui's native multiline. Highlights are painted as an
overlay on the same draw list, after the multiline build:

- The pane already tracks `scroll_y`, `font_size`, line count.
- Glyph advance is derived from the same mono font used by custom panes.
- Compute rects in pane-local coordinates, then translate by
  `cursor_screen_pos - scroll`.
- Same fill colors as above.

### Scrollbar ticks

For every pane (custom vbar and native scrollbar alike):

- After matches are collected, group them by line.
- For each unique matching line, draw a 2px-tall rect on the scrollbar track at
  `y = track_top + (line / total_lines) * track_h`, full track width, color
  `[1.0, 0.55, 0.1, 0.85]`.
- Custom panes already know their vbar geometry (`vbar_thumb_geom`). Result
  pane: scrollbar lives on the child window's right edge; paint into the same
  window after the multiline draws.

### Current-match tracking and jumping

`AppSearch` carries `current: Option<(PaneId, usize)>` where `PaneId` is a new
enum mapping 1:1 to `FocusedPane`. Each pane registers its match list with
`AppSearch` per frame (small per-pane scratch buffer; cleared at frame start).

On `jump_request`:

1. Target pane = `state.focused`'s mapped `PaneId`, falling back to the first
   pane (in `PaneId` order) that has any matches.
2. From its match list, pick the next (or previous) match relative to the
   caret's current byte offset; wrap at list boundaries.
3. Update the pane's caret/selection:
   - Custom panes: write directly to their caret/selection state (already
     tracked for click-drag selection).
   - Result pane: queued via the existing `result_pane` jump mechanism (a
     pending caret + selection set, applied through an
     `InputTextCallback::ALWAYS` on the next frame — mirrors how Ctrl+Z is
     already routed).
4. Scroll the match into view centered, using the existing
   `pending_*_scroll` mechanism.

If `total_matches == 0`, `jump_request` is dropped silently (the red field is
the only failure UX).

### Persistence

`preferences.rs` gains three fields: `search_case_sensitive: bool`,
`search_whole_word: bool`, `search_regex: bool`. Defaults: all false. Loaded
on startup, saved when toggles change.

### Out of scope

- Find-and-replace.
- Searching across non-active tabs.
- Incremental jump-while-typing (only highlights are live; Enter is the
  explicit jump).
- Multi-line match highlight as a single rect (always per-line spans).

## Testing

`src/app/search.rs` carries `#[cfg(test)] mod tests`:

- Empty query → `build` returns `Err`-equivalent (decided: returns `Ok` with a
  sentinel "never matches" regex, OR caller skips compile when query empty —
  going with the latter; `build` is only called for non-empty queries).
- Literal hit / miss.
- Case-sensitive ON vs OFF.
- Whole-word boundary cases: `foo` matches `foo bar` but not `foobar`.
- Regex syntax error → `Err`.
- Multiple matches on one line.
- Lines with no match return empty.
- Match clipped at line break (regex `.*` on multi-line input).

Tests compile under `cargo test --no-default-features --lib`.

GUI behavior is verified manually:

- Type into the field; matches appear in all visible panes; scrollbar ticks
  appear.
- Ctrl+F focuses the field from any pane.
- Enter jumps within the last-focused pane; Shift+Enter goes back.
- Field turns red when matches drop to zero, returns to normal otherwise.
- Toggling case/whole-word/regex updates highlights live.
- Preferences persist across restart.

## Dependencies

- Add `regex = "1"` to `Cargo.toml` as a top-level dependency.

## File-level change list

- `Cargo.toml` — add `regex`.
- `src/app/search.rs` — new module (matching + `CompiledQuery`).
- `src/app/mod.rs` — `AppSearch` state, `PaneId` enum, Ctrl+F / F3 handling,
  per-frame match-collection scratch.
- `src/app/engine_bar.rs` — find input + toggles + focus + red-state.
- `src/app/preferences.rs` — three new bool fields.
- `src/app/diff_view/mod.rs` (+ helpers) — paint highlights, scrollbar ticks,
  caret jump.
- `src/app/merge_view.rs` — same for three-way panes.
- `src/app/result_pane.rs` — overlay highlights, scrollbar ticks, queued
  caret jump.
