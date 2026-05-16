# 3-Way Syntax Highlighting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add tree-sitter syntax highlighting to the three input panes of the 3-way merge view, reusing the 2-way view's existing `HighlightCache` and per-line span painter.

**Architecture:** Extract the per-line span-walking text painter from `diff_view/overlay.rs` into a new shared `src/app/syntax_paint.rs` module. `merge_view::paint_pane_text` gains a `highlights: &[LineSpans]` arg and calls the shared painter the same way `overlay::paint_pane_text` does. The caller in `mod.rs` computes per-side language and highlights for Base/Local/Remote and threads them in.

**Tech Stack:** Rust, `imgui-rs`, existing `tree-sitter` highlight cache in `src/app/syntax.rs`.

---

## File Structure

**New files:**
- `src/app/syntax_paint.rs` — Shared per-line text painter + the small pure helper used by its byte-offset math. ~110 LOC. Exports `paint_line_with_spans` (the painter) and `snap_to_char_boundary` (testable helper); `text_x_at_byte` lives here too and is `pub(crate)` so `overlay.rs`'s sub-line tint code can still call it.

**Modified files:**
- `src/app/diff_view/overlay.rs` — Replace inline span-walking with a call to `syntax_paint::paint_line_with_spans`. Delete the `pub(super) fn text_x_at_byte` definition (now lives in `syntax_paint`); update internal call sites to use the imported function.
- `src/app/merge_view.rs` — Extend `render`, `render_pane`, and `paint_pane_text` with `&[LineSpans]` params; replace the existing `dl.add_text(...)` line painting with a call into `syntax_paint::paint_line_with_spans`.
- `src/app/mod.rs` — `pub mod syntax_paint;` declaration; bump the 2-way cache-key shift from `id << 1` to `id << 2` for consistency; compute per-side `(lang, lines, highlights)` in the `ThreeWay` arm of `current_session_summary` and pass them to `merge_view::render`.

---

## Task 1: Create `syntax_paint` module

**Files:**
- Create: `src/app/syntax_paint.rs`
- Modify: `src/app/mod.rs` (declaration only)

Build the new module with the painter and helpers. The 2-way path keeps working unchanged this task; the new module is unused until Task 2.

- [ ] **Step 1: Add the module declaration**

In `src/app/mod.rs`, near the other top-level module declarations (search for `pub mod three_way_header;` and add adjacent):

```rust
pub mod syntax_paint;
```

- [ ] **Step 2: Write the failing test**

Create `src/app/syntax_paint.rs` with this content:

```rust
//! Shared per-line text painter used by both the 2-way and 3-way diff views.
//!
//! Walks a line's syntax-highlight spans in order and emits one `add_text`
//! call per span (in the span's color) plus default-colored gaps and a tail.
//! Lines without spans render in a single default-colored `add_text` call.
//! Both view kinds suppress imgui's own text rendering and rely on this
//! helper to paint text on the foreground draw list.

use imgui::Ui;

use crate::app::syntax::LineSpans;
use crate::app::theme;

/// Snap a byte offset to the nearest preceding char boundary in `s`.
pub fn snap_to_char_boundary(s: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(s.len());
    let mut snap = clamped;
    while snap > 0 && !s.is_char_boundary(snap) {
        snap -= 1;
    }
    snap
}

/// Compute the x offset of a byte position within `line`, clamped to a
/// char boundary, using imgui's font metrics (matches the multiline widget's
/// own hit-testing).
pub(crate) fn text_x_at_byte(ui: &Ui, line: &str, byte_offset: usize, padding_x: f32) -> f32 {
    let snap = snap_to_char_boundary(line, byte_offset);
    padding_x + ui.calc_text_size(&line[..snap])[0]
}

/// Paint one line of text on the current window's draw list. If `line_spans`
/// is provided and non-empty, emit one chunk per span boundary: default-color
/// gaps + span-colored ranges + default-color tail. Otherwise emit the whole
/// line in `theme::TEXT()`. `line_origin_x` is the screen-space x of byte 0 of
/// the line (i.e. `widget_left + padding_x - scroll_x`).
pub fn paint_line_with_spans(
    ui: &Ui,
    line_origin: [f32; 2],
    line_text: &str,
    line_spans: Option<&LineSpans>,
    scroll_x: f32,
    padding_x: f32,
) {
    if line_text.is_empty() {
        return;
    }
    let dl = ui.get_window_draw_list();
    let widget_left = line_origin[0] - padding_x + scroll_x;
    let text_y = line_origin[1];

    let Some(line_spans) = line_spans.filter(|v| !v.is_empty()) else {
        dl.add_text(line_origin, theme::TEXT(), line_text);
        return;
    };

    let chars: Vec<(usize, char)> = line_text.char_indices().collect();
    let mut cursor_col: usize = 0;
    for span in line_spans {
        let s = span.start_col;
        let e = span.end_col.min(chars.len());
        if e <= s {
            continue;
        }
        // Default-colored gap before this span.
        if s > cursor_col {
            let gap_start_byte = chars[cursor_col].0;
            let gap_end_byte = if s >= chars.len() {
                line_text.len()
            } else {
                chars[s].0
            };
            if gap_end_byte > gap_start_byte {
                let x = widget_left - scroll_x
                    + text_x_at_byte(ui, line_text, gap_start_byte, padding_x);
                dl.add_text(
                    [x, text_y],
                    theme::TEXT(),
                    &line_text[gap_start_byte..gap_end_byte],
                );
            }
        }
        // Colored span.
        if s >= chars.len() {
            cursor_col = s;
            continue;
        }
        let span_start_byte = chars[s].0;
        let span_end_byte = if e >= chars.len() {
            line_text.len()
        } else {
            chars[e].0
        };
        if span_end_byte > span_start_byte {
            let x = widget_left - scroll_x
                + text_x_at_byte(ui, line_text, span_start_byte, padding_x);
            dl.add_text(
                [x, text_y],
                span.kind.color(),
                &line_text[span_start_byte..span_end_byte],
            );
        }
        cursor_col = e;
    }
    // Tail after the last span.
    if cursor_col < chars.len() {
        let tail_byte = chars[cursor_col].0;
        if tail_byte < line_text.len() {
            let x = widget_left - scroll_x
                + text_x_at_byte(ui, line_text, tail_byte, padding_x);
            dl.add_text([x, text_y], theme::TEXT(), &line_text[tail_byte..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_boundary_is_no_op_for_ascii() {
        let s = "hello";
        for i in 0..=s.len() {
            assert_eq!(snap_to_char_boundary(s, i), i);
        }
    }

    #[test]
    fn snap_to_boundary_clamps_past_end() {
        let s = "hi";
        assert_eq!(snap_to_char_boundary(s, 99), s.len());
    }

    #[test]
    fn snap_to_boundary_snaps_mid_codepoint() {
        // "é" is 2 bytes in UTF-8 (0xC3, 0xA9). Byte offset 1 is mid-codepoint.
        let s = "café";
        assert_eq!(snap_to_char_boundary(s, 0), 0);
        assert_eq!(snap_to_char_boundary(s, 1), 1); // 'c' boundary
        assert_eq!(snap_to_char_boundary(s, 2), 2); // 'a' boundary
        assert_eq!(snap_to_char_boundary(s, 3), 3); // 'f' boundary
        assert_eq!(snap_to_char_boundary(s, 4), 4); // 'é' start
        assert_eq!(snap_to_char_boundary(s, 5), 4); // mid 'é', snaps back
        assert_eq!(snap_to_char_boundary(s, 6), 6); // end of string
    }

    #[test]
    fn snap_to_boundary_empty_string() {
        assert_eq!(snap_to_char_boundary("", 0), 0);
        assert_eq!(snap_to_char_boundary("", 99), 0);
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib syntax_paint`
Expected: 4 tests PASS.

- [ ] **Step 4: Confirm clean build**

Run: `cargo build`
Expected: succeeds. The new module compiles. Expect "function `paint_line_with_spans` is never used" and possibly "function `text_x_at_byte` is never used" warnings since nothing imports them yet — those go away in Task 2.

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/syntax_paint.rs
git commit -m "feat(app): add syntax_paint module with shared per-line painter"
```

---

## Task 2: Diff_view consumes shared painter

**Files:**
- Modify: `src/app/diff_view/overlay.rs`

Replace the inline span-walking block and the local `text_x_at_byte` helper. 2-way's behavior must stay identical (visual change = none).

- [ ] **Step 1: Update imports at the top of `overlay.rs`**

Find the existing `use crate::app::syntax::LineSpans;` line near the top of `src/app/diff_view/overlay.rs` (around line 14). Right under it, add:

```rust
use crate::app::syntax_paint;
```

- [ ] **Step 2: Remove the local `text_x_at_byte` function**

In `src/app/diff_view/overlay.rs`, locate this block (around lines 86–96):

```rust
/// Compute the x offset of a byte position within `line`, clamped to a
/// char boundary, using imgui's font metrics (matches the multiline widget's
/// own hit-testing).
pub(super) fn text_x_at_byte(ui: &Ui, line: &str, byte_offset: usize, padding_x: f32) -> f32 {
    let clamped = byte_offset.min(line.len());
    let mut snap = clamped;
    while snap > 0 && !line.is_char_boundary(snap) {
        snap -= 1;
    }
    padding_x + ui.calc_text_size(&line[..snap])[0]
}
```

Delete it entirely.

- [ ] **Step 3: Replace remaining `text_x_at_byte` call sites with the imported one**

Anywhere in `overlay.rs` that still calls `text_x_at_byte(...)` (the sub-line span tint loop around lines 222–225 is one such caller), the unqualified call already binds to `syntax_paint::text_x_at_byte` because we imported the module. Verify with: `grep -n 'text_x_at_byte' src/app/diff_view/overlay.rs`. Every remaining call should now read like `text_x_at_byte(ui, line_text, ...)` — change them to `syntax_paint::text_x_at_byte(ui, line_text, ...)` so the source-of-truth is explicit and the import goes through the module path.

For example, the sub-line tint block at lines 222–225:

```rust
                                let x0 = widget_left - scroll_x
                                    + text_x_at_byte(ui, line_text, sp.start as usize, padding_x);
                                let x1 = widget_left - scroll_x
                                    + text_x_at_byte(ui, line_text, sp.end as usize, padding_x);
```

becomes:

```rust
                                let x0 = widget_left - scroll_x
                                    + syntax_paint::text_x_at_byte(ui, line_text, sp.start as usize, padding_x);
                                let x1 = widget_left - scroll_x
                                    + syntax_paint::text_x_at_byte(ui, line_text, sp.end as usize, padding_x);
```

- [ ] **Step 4: Replace the inline span-walking text painter**

In `src/app/diff_view/overlay.rs`, locate the block that paints text (around lines 238–309):

```rust
                // Paint text. If there are highlight spans for this line,
                // walk the line and emit a chunk per span boundary in default
                // color + each span in its color. Otherwise emit the whole
                // line in default color.
                let text_y = y;
                let line_spans_opt = highlights.get(line_idx);
                if let Some(line_spans) = line_spans_opt.filter(|v| !v.is_empty()) {
                    // Walk char-indexed positions.
                    let chars: Vec<(usize, char)> = line_text.char_indices().collect();
                    let mut cursor_col: usize = 0;
                    for span in line_spans {
                        let s = span.start_col;
                        let e = span.end_col.min(chars.len());
                        if e <= s {
                            continue;
                        }
                        // Default-colored gap before this span.
                        if s > cursor_col {
                            let gap_start_byte = chars[cursor_col].0;
                            let gap_end_byte = if s >= chars.len() {
                                line_text.len()
                            } else {
                                chars[s].0
                            };
                            if gap_end_byte > gap_start_byte {
                                let x = widget_left - scroll_x
                                    + text_x_at_byte(ui, line_text, gap_start_byte, padding_x);
                                dl.add_text(
                                    [x, text_y],
                                    theme::TEXT(),
                                    &line_text[gap_start_byte..gap_end_byte],
                                );
                            }
                        }
                        // Colored span.
                        if s >= chars.len() {
                            cursor_col = s;
                            continue;
                        }
                        let span_start_byte = chars[s].0;
                        let span_end_byte = if e >= chars.len() {
                            line_text.len()
                        } else {
                            chars[e].0
                        };
                        if span_end_byte > span_start_byte {
                            let x = widget_left - scroll_x
                                + text_x_at_byte(ui, line_text, span_start_byte, padding_x);
                            dl.add_text(
                                [x, text_y],
                                span.kind.color(),
                                &line_text[span_start_byte..span_end_byte],
                            );
                        }
                        cursor_col = e;
                    }
                    // Tail after the last span.
                    if cursor_col < chars.len() {
                        let tail_byte = chars[cursor_col].0;
                        if tail_byte < line_text.len() {
                            let x = widget_left - scroll_x
                                + text_x_at_byte(ui, line_text, tail_byte, padding_x);
                            dl.add_text([x, text_y], theme::TEXT(), &line_text[tail_byte..]);
                        }
                    }
                } else if !line_text.is_empty() {
                    dl.add_text(
                        [widget_left + padding_x - scroll_x, text_y],
                        theme::TEXT(),
                        line_text,
                    );
                }
```

Replace the entire block with:

```rust
                // Paint text via the shared per-line span painter.
                let line_origin = [widget_left + padding_x - scroll_x, y];
                syntax_paint::paint_line_with_spans(
                    ui,
                    line_origin,
                    line_text,
                    highlights.get(line_idx),
                    scroll_x,
                    padding_x,
                );
```

- [ ] **Step 5: Build and run tests**

Run: `cargo build`
Expected: succeeds. No new warnings; the previously-"unused" warnings on `paint_line_with_spans` / `text_x_at_byte` from Task 1 are now gone.

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Manual verification**

Run: `cargo run`. Open a 2-way diff of two Rust files. Confirm the syntax highlighting still appears the same as before (keywords, types, strings colored). This is the regression check for the extraction.

- [ ] **Step 7: Commit**

```bash
git add src/app/diff_view/overlay.rs
git commit -m "refactor(diff-view): use shared syntax_paint for line text painting"
```

---

## Task 3: Wire highlights through `merge_view`

**Files:**
- Modify: `src/app/merge_view.rs`

Add `&[LineSpans]` params through `render` → `render_pane` → `paint_pane_text`, and call the shared painter for line text. Module compiles even though `mod.rs` hasn't been updated yet — Task 4 supplies the highlights at the call site.

- [ ] **Step 1: Add imports**

Near the top of `src/app/merge_view.rs`, add:

```rust
use crate::app::syntax::LineSpans;
use crate::app::syntax_paint;
```

- [ ] **Step 2: Extend `render` signature**

Locate `pub fn render(` (around line 184 in `merge_view.rs`). Update the signature to add three trailing parameters:

```rust
#[allow(clippy::too_many_arguments)]
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
) {
```

(`#[allow(clippy::too_many_arguments)]` is already on the function — leave it; if it isn't, add it.)

- [ ] **Step 3: Pass highlights into each `render_pane` call**

Locate the three `render_pane(...)` calls in `render` (around lines 263, 274, 283 after the recent reorder). Each call currently passes 12 args ending in `lh`. Add a 13th arg before `lh` matching that pane's highlights slice. For example:

```rust
    let (_remote_rect, remote_scroll, remote_origin) = render_pane(
        ui, state, remote_pos, pane_w, pane_h, Pane::Remote, session_id,
        pending_edits, &remote_layout, &hover_panes[2], &focus_event,
        remote_highlights, lh,
    );

    ui.set_cursor_screen_pos(connector_rb_pos);
    ui.invisible_button("merge_connector_rb", [CONNECTOR_W, pane_h]);

    let (_base_rect, base_scroll, base_origin) = render_pane(
        ui, state, base_pos, pane_w, pane_h, Pane::Base, session_id,
        pending_edits, &base_layout, &hover_panes[0], &focus_event,
        base_highlights, lh,
    );

    ui.set_cursor_screen_pos(connector_bl_pos);
    ui.invisible_button("merge_connector_bl", [CONNECTOR_W, pane_h]);

    let (_local_rect, local_scroll, local_origin) = render_pane(
        ui, state, local_pos, pane_w, pane_h, Pane::Local, session_id,
        pending_edits, &local_layout, &hover_panes[1], &focus_event,
        local_highlights, lh,
    );
```

- [ ] **Step 4: Extend `render_pane` signature**

Locate `fn render_pane(` (around line 348 in `merge_view.rs`). Update the signature to add `highlights: &[LineSpans]` immediately before `lh: f32`:

```rust
#[allow(clippy::too_many_arguments)]
fn render_pane(
    ui: &Ui,
    state: &mut MergeViewState,
    pane_pos: [f32; 2],
    pane_w: f32,
    pane_h: f32,
    pane: Pane,
    session_id: SessionId,
    pending_edits: &mut Vec<DiffEdit>,
    layout: &PaneLayout,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    highlights: &[LineSpans],
    lh: f32,
) -> ([f32; 4], f32, [f32; 2]) {
```

(If the function already has `#[allow(clippy::too_many_arguments)]`, leave it. If not, add it — we're already at 12+ args.)

- [ ] **Step 5: Pass highlights into `paint_pane_text`**

Locate the `paint_pane_text(...)` call inside `render_pane` (around line 568). Add `highlights` as the trailing arg. Update from:

```rust
    paint_pane_text(
        ui,
        widget_rect,
        buf_for_paint,
        layout,
        scroll_y_out,
        scroll_x_out,
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
    );
```

to:

```rust
    paint_pane_text(
        ui,
        widget_rect,
        buf_for_paint,
        layout,
        scroll_y_out,
        scroll_x_out,
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
        highlights,
    );
```

- [ ] **Step 6: Extend `paint_pane_text` signature**

Locate `fn paint_pane_text(` (around line 620). Add `highlights: &[LineSpans]` as the trailing parameter:

```rust
#[allow(clippy::too_many_arguments)]
fn paint_pane_text(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    layout: &PaneLayout,
    scroll_y: f32,
    scroll_x: f32,
    lh: f32,
    caret_byte: i32,
    widget_active: bool,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
    highlights: &[LineSpans],
) {
```

- [ ] **Step 7: Replace the inline `add_text` line painting with the shared painter**

Inside `paint_pane_text`, locate the block that emits text (around line 679-685 in current source; the body inside `for (line_idx, line_text) in buf.lines().enumerate() { ... }`). It currently reads:

```rust
            if !line_text.is_empty() {
                dl.add_text(
                    [widget_left + padding_x - scroll_x, y],
                    theme::TEXT(),
                    line_text,
                );
            }
```

Replace with:

```rust
            let line_origin = [widget_left + padding_x - scroll_x, y];
            syntax_paint::paint_line_with_spans(
                ui,
                line_origin,
                line_text,
                highlights.get(line_idx),
                scroll_x,
                padding_x,
            );
```

(Remove the `if !line_text.is_empty()` guard — `paint_line_with_spans` has its own early-return for empty text.)

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: builds, BUT there will be compile errors at the `merge_view::render` call site in `mod.rs` because we changed the signature. That's expected — Task 4 fixes the call site. If you'd like a green build before committing, do this in conjunction with Task 4. Otherwise commit and proceed.

If the build error is ONLY the missing `merge_view::render` args, commit and continue. If there are other errors, stop and investigate.

- [ ] **Step 9: Commit**

```bash
git add src/app/merge_view.rs
git commit -m "feat(merge-view): plumb per-pane LineSpans through render_pane to paint_pane_text"
```

The commit leaves the call site in `mod.rs` broken; Task 4 fixes it in the next commit. This is OK because the two commits move together — no intermediate `master` state is checked out manually.

---

## Task 4: Compute and pass per-side highlights in `mod.rs`

**Files:**
- Modify: `src/app/mod.rs`

The build is currently broken from Task 3's signature change. This task makes it whole again by computing per-side highlights for the 3-way arm and threading them in. Also bumps the 2-way cache-key shift from `id << 1` to `id << 2` to widen the keyspace symmetrically.

- [ ] **Step 1: Update the 2-way cache keys**

Locate the 2-way cache-key lines in `current_session_summary` (around line 1775-1776):

```rust
            let a_key = id << 1;
            let b_key = (id << 1) | 1;
```

Replace with:

```rust
            let a_key = id << 2;
            let b_key = (id << 2) | 1;
```

The previous keyspace was disjoint per session because the shift was 1; the new shift of 2 leaves room for the 3-way arm's three keys without collision. Existing entries in the cache will miss once on the first frame after this change (different keys); no functional regression.

- [ ] **Step 2: Compute per-side highlights in the `ThreeWay` arm**

Locate the `ThreeWay` arm in `current_session_summary` (around line 1831). Just before the `let counts = three_way_header::count_hunks(hunks);` line — actually before the whole 3-way render block begins — we need access to `base_text`/`local_text`/`remote_text` and `tab_paths_snap`. Find this:

```rust
        SessionMode::ThreeWay { hunks, anchors, resolutions, .. } => {
            let counts = three_way_header::count_hunks(hunks);
            three_way_header::render(ui, counts);
            ui.separator();
            anchor_bar_three_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
```

The destructure only binds `hunks`, `anchors`, `resolutions`. We need `base_text`, `local_text`, `remote_text` too. Update the destructure:

```rust
        SessionMode::ThreeWay { hunks, anchors, resolutions, base_text, local_text, remote_text, .. } => {
```

Then, right after `anchor_bar_three_way(...)` + `ui.separator();` and BEFORE the `let avail = ui.content_region_avail();` line that begins the rendering block (around line 1837), insert this block:

```rust
            // Per-side syntax highlights for the three input panes, mirroring
            // the 2-way path. Each side may have a different language if the
            // user picked mixed extensions.
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
            let base_lines:   Vec<String> = crate::session::lines_of(base_text)
                .into_iter().map(|s| s.to_string()).collect();
            let local_lines:  Vec<String> = crate::session::lines_of(local_text)
                .into_iter().map(|s| s.to_string()).collect();
            let remote_lines: Vec<String> = crate::session::lines_of(remote_text)
                .into_iter().map(|s| s.to_string()).collect();
            let base_h   = state.syntax.highlights(base_key,   base_lang,   &base_lines).to_vec();
            let local_h  = state.syntax.highlights(local_key,  local_lang,  &local_lines).to_vec();
            let remote_h = state.syntax.highlights(remote_key, remote_lang, &remote_lines).to_vec();
```

- [ ] **Step 3: Update the `merge_view::render` call site**

Locate the existing `merge_view::render(...)` call (around line 1849-1862):

```rust
                        merge_view::render(
                            ui,
                            store,
                            id,
                            hunks,
                            anchors,
                            status,
                            view_state,
                            mono,
                            &mut focus_request,
                            &mut pending_edits,
                        );
```

Replace with:

```rust
                        merge_view::render(
                            ui,
                            store,
                            id,
                            hunks,
                            anchors,
                            status,
                            view_state,
                            mono,
                            &mut focus_request,
                            &mut pending_edits,
                            &base_h,
                            &local_h,
                            &remote_h,
                        );
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build`
Expected: succeeds. The 2-way and 3-way arms both compile; the new highlights args are passed correctly.

Run: `cargo test --no-default-features --lib`
Expected: PASS.

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Manual verification**

Run: `cargo run`. Open a 3-way merge of three `.rs` files (any three would do; if you have a quick repro, use that). Confirm:
- Keywords (`pub`, `mod`, `fn`, `let`, `match`, etc.) are colored.
- Types and identifiers are differently styled per the existing Catppuccin token colors.
- The Remote / Base / Local panes all show consistent highlighting.
- Switching to a 2-way diff still works as before (no regression).

Then open a mixed-extension 3-way (e.g. base.rs / local.rs / remote.cpp). Each pane should use its own language's tokens.

- [ ] **Step 6: Commit**

```bash
git add src/app/mod.rs
git commit -m "feat(app): wire per-side syntax highlights into 3-way merge view"
```

---

## Self-Review

**Spec coverage:**
- New `src/app/syntax_paint.rs` module with `paint_line_with_spans` — Task 1.
- `text_x_at_byte` and `snap_to_char_boundary` helpers in the new module — Task 1.
- 2-way `overlay.rs` uses the shared painter — Task 2.
- `merge_view::render` / `render_pane` / `paint_pane_text` gain `&[LineSpans]` params — Task 3.
- Per-side language detection (`syntax::lang_for_path`) for Base/Local/Remote — Task 4 step 2.
- Cache-key widening to `(id << 2) | side` for both 2-way and 3-way — Task 4 steps 1 + 2.
- Caller threading of `&base_h`, `&local_h`, `&remote_h` into `merge_view::render` — Task 4 step 3.

**Placeholder scan:** No "TBD" or "implement later". Every step has runnable code or exact diff replacements. Task 3 step 8 deliberately leaves the build broken between Tasks 3 and 4 — flagged in the step's text, recovered in Task 4. This is acceptable because subagent-driven execution moves through both tasks before review checkpoints would surface the breakage to the human.

**Type consistency:**
- `LineSpans` is `pub type LineSpans = Vec<LineSpan>` in `src/app/syntax.rs:391`. Used by signature in 2-way overlay, the new painter, and all three new merge_view params.
- `paint_line_with_spans(ui, line_origin, line_text, line_spans, scroll_x, padding_x)` signature consistent across Task 1 (definition) and Tasks 2/3 (call sites).
- `text_x_at_byte` is `pub(crate)` in `syntax_paint`, callable from `overlay.rs` via the `crate::app::syntax_paint::text_x_at_byte` path; Task 2 step 3 confirms the call-site update.
- `snap_to_char_boundary(s, byte_offset) -> usize` is `pub` and tested.
- `merge_view::render` signature with the three new trailing `&[LineSpans]` params matches the Task 4 step 3 call site exactly.
- Cache key shifts are consistent: 2-way is `(id << 2)` and `(id << 2) | 1`; 3-way is `(id << 2)`, `(id << 2) | 1`, `(id << 2) | 2`. The 2-way's `| 1` collides with 3-way's `| 1`, but a single session is either 2-way OR 3-way — the same `id` never has both kinds of entries in the cache.

All consistent.
