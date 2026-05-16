# 3-Way Syntax Highlighting

Add tree-sitter syntax highlighting to the three input panes of the 3-way merge view, reusing the existing `HighlightCache` and per-line span painter that the 2-way diff view already uses.

## Background

The 2-way diff view in `src/app/diff_view/` paints each line's text with token-level colors derived from a tree-sitter parse. The flow is:

1. `current_session_summary`'s `TwoWay` arm reads the tab's per-side paths, resolves each side's language via `syntax::lang_for_path`, splits the text into lines, and calls `state.syntax.highlights(key, lang, &lines).to_vec()` to get a `Vec<LineSpans>` per side.
2. The `LineSpans` are passed to `diff_view::render`, which forwards them to `render_pane` and then to `paint_pane_text` in `overlay.rs`.
3. `paint_pane_text` walks each line's spans and emits one `add_text` call per span (in the span's color) plus default-colored gaps and a tail.

The 3-way merge view in `src/app/merge_view.rs` currently paints every line in `theme::TEXT()`. This spec extends the existing 2-way machinery to cover the three input panes (Base / Local / Remote).

## User-facing change

Lines in the Base, Local, and Remote panes of a 3-way merge tab render with the same token-level coloring the 2-way view already shows for matching file extensions. The merged result pane stays unhighlighted (out of scope).

## Architecture

### Shared painter module

Extract the per-line span-walking text painter from `src/app/diff_view/overlay.rs` into a new module `src/app/syntax_paint.rs`. Public API:

```rust
use crate::app::syntax::LineSpans;
use imgui::Ui;

pub fn paint_line_with_spans(
    ui: &Ui,
    line_start: [f32; 2],         // screen-space top-left of the line
    line_text: &str,
    line_spans: Option<&LineSpans>,
    scroll_x: f32,
    padding_x: f32,
);
```

`paint_line_with_spans` is responsible for:

- Walking the spans in order and emitting `add_text` for each span in `span.kind.color()`.
- Emitting default-colored (`theme::TEXT()`) gaps between spans.
- Emitting a default-colored tail after the last span.
- When `line_spans` is `None` or empty (and `line_text` is non-empty), emitting one `add_text` at the line origin in `theme::TEXT()`.

The existing `text_x_at_byte` helper (currently private to `overlay.rs`) moves to `syntax_paint` as a module-private helper. If `text_x_at_byte` has other callers in `overlay.rs` outside the span-walking block, leave it accessible to them via the new module (`pub(in crate::app)` or similar).

`overlay::paint_pane_text` replaces its inline span loop with a call to `syntax_paint::paint_line_with_spans`. Net effect on `overlay.rs`: shrinks by ~60 LOC.

### 3-way wiring

`merge_view::render` signature gains three trailing parameters:

```rust
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[MergeHunk],
    anchors: &[MergeAnchor],
    status: &mut String,
    state: &mut MergeViewState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    pending_edits: &mut Vec<DiffEdit>,
    base_highlights: &[LineSpans],
    local_highlights: &[LineSpans],
    remote_highlights: &[LineSpans],
);
```

`render_pane` gains one new parameter, `highlights: &[LineSpans]`, picked from the matching call site in `render`. The signature is parallel to 2-way's `render_pane`.

`paint_pane_text` gains `highlights: &[LineSpans]` and replaces the body of its current `if !line_text.is_empty() { dl.add_text(...) }` branch with:

```rust
let line_start = [
    widget_left + padding_x - scroll_x,
    y,
];
syntax_paint::paint_line_with_spans(
    ui,
    line_start,
    line_text,
    highlights.get(line_idx),
    scroll_x,
    padding_x,
);
```

(The helper accepts the precomputed `line_start` but still takes `scroll_x` and `padding_x` so its internal `text_x_at_byte` calculation can position each span correctly — the same as 2-way today.)

### Caller integration

In `mod.rs::current_session_summary`'s `ThreeWay` arm, mirror what the 2-way arm already does:

```rust
let (base_lang, local_lang, remote_lang) = match &tab_paths_snap {
    Some(paths) => (
        paths.first().and_then(|p| syntax::lang_for_path(p)),
        paths.get(1).and_then(|p| syntax::lang_for_path(p)),
        paths.get(2).and_then(|p| syntax::lang_for_path(p)),
    ),
    None => (None, None, None),
};
let base_key   = (id << 2) | 0;
let local_key  = (id << 2) | 1;
let remote_key = (id << 2) | 2;
let base_lines:   Vec<String> = base_text.lines().map(String::from).collect();
let local_lines:  Vec<String> = local_text.lines().map(String::from).collect();
let remote_lines: Vec<String> = remote_text.lines().map(String::from).collect();
let base_h   = state.syntax.highlights(base_key,   base_lang,   &base_lines).to_vec();
let local_h  = state.syntax.highlights(local_key,  local_lang,  &local_lines).to_vec();
let remote_h = state.syntax.highlights(remote_key, remote_lang, &remote_lines).to_vec();
```

Pass `&base_h`, `&local_h`, `&remote_h` as the three new args to `merge_view::render`.

### Cache key widening

The 2-way cache key currently is `id << 1` / `(id << 1) | 1` (1 bit for the side). 3-way needs 2 bits. Change both arms to `(id << 2) | side_idx` so the keyspace never collides between modes. The shift is purely a key scheme — no on-disk format, no API impact.

`session_id` is `u64`; shifting by 2 loses 2 high bits of address space but in practice session IDs are monotonic small integers, so no realistic collision risk.

### Why per-side language

Per-side language detection is the same approach 2-way uses. Two-way already detects per-side because the two paths can have different extensions (you can diff a `.py` against a `.txt`). The same can happen in 3-way, so we treat each pane independently. Most of the time the three extensions match and we get the obvious behavior; in the mixed case each pane lights up with its own language's tokens.

## Files touched

- `src/app/syntax_paint.rs` *(new)* — extracted painter + `text_x_at_byte`. ~80 LOC.
- `src/app/diff_view/overlay.rs` — replace inline span-walking with `syntax_paint::paint_line_with_spans` call. Remove the now-unused `text_x_at_byte` helper.
- `src/app/diff_view/mod.rs` — none (caller in `mod.rs` changes the cache-key scheme).
- `src/app/mod.rs` — `mod syntax_paint;` declaration; 2-way arm updates cache-key shift from `id << 1` to `id << 2`; 3-way arm computes per-side highlights and threads them into `merge_view::render`.
- `src/app/merge_view.rs` — new params on `render`/`render_pane`/`paint_pane_text`; `paint_pane_text` uses the shared painter for line text.

## Testing

- **Unit:** `syntax_paint::tests::paint_line_with_spans_empty_line_emits_nothing` — when `line_text` is empty, no draw calls.
- **Unit:** `syntax_paint::tests::paint_line_with_spans_no_spans_uses_default_color` — when `line_spans` is `None`, the painter is called once with the full text in `theme::TEXT()`. (Verified by recording calls via a test stub or by exercising `text_x_at_byte` math directly.)
- **Unit:** `syntax_paint::tests::text_x_at_byte_advances_with_text_width` — given a known monospace font, byte offsets map to expected x positions.
- **Manual verification:** open a 3-way merge of three `.rs` files. Confirm keywords, types, and string literals are colored in all three panes the same way as in the 2-way view. Open a mixed-extension 3-way (e.g. `.rs` / `.cpp` / `.rs`) and confirm each pane uses its own language's tokens.

The new test module lives in `src/app/syntax_paint.rs`. We don't add merge-view-level rendering tests; the imgui pipeline harness in `diff_view/tests.rs` is already heavy and tests for the per-line painter belong with the painter itself.

## Out of scope

- Sub-line refinement highlighting (the diff-engine's char-level Delete/Insert spans currently shown in 2-way). 3-way can revisit later.
- Result pane highlighting. The result text changes continuously as the user picks resolutions; a key scheme and invalidation policy for that is its own decision and not needed for this feature.
- Adding new languages to the syntax cache. The current set (Rust / C++ / C# / HLSL) is what 3-way picks up automatically.
