# Diff View Multi-line Widget Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 2-way diff view's per-row `input_text` widget topology and the cross-row state machine built around it with one `input_text_multiline` per pane, plus draw-list overlays for per-row decorations. Same treatment for the merge view's base/local/remote panes (becoming editable in the process).

**Architecture:** Session storage flips from `Vec<String>` to `String` per side; diff engine still consumes `&[&str]` via `split('\n')` at the call boundary. Edit types collapse to `SetSide` + the kept `ReplaceHunkSide`. UI: two `input_text_multiline` widgets per 2-way tab (and three per 3-way tab); per-row backgrounds, sub-line spans, hover overlays, and anchor gutter become draw-list overlays computed from the widget's scroll_y and line_h.

**Tech Stack:** Rust, `imgui-rs` + `wgpu` + `winit`, headless wgpu test harness.

Reference spec: `docs/superpowers/specs/2026-05-14-diff-view-multiline-rewrite-design.md`.

---

## File Structure

**Modified:**
- `src/io.rs` — `read_text` returns `(String, bool)` (text without trailing newline + trailing-newline flag); `write_text` appends `\n` based on the flag.
- `src/session.rs` — `SessionMode::TwoWay`/`ThreeWay` flip to `*_text: String` + `*_trailing_newline: bool`. `set_two_way_line`/`splice_two_way_lines`/`set_two_way_lines` removed, replaced by `set_side_text` + `set_three_way_side_text`. New `ThreeWaySide` enum. `replace_hunk_side` reworked to operate on String via `replace_hunk_in_text`. `recompute_two_way`/`recompute_three_way` split strings before calling the engine.
- `src/app/undo_stack.rs` — drop `SetTwoWayLine` and `SpliceTwoWayLines`; add `SetSide` and `SetThreeWaySide`. `DiffEdit::merge` extended for the new variants. New `SideRef` enum.
- `src/app/diff_view/common.rs` — drop cross-row state fields (`selection`, `drag`, `arrow_focus`, `shift_arrow_extend`, etc.); add only what overlays need (line-y math, hover detection helpers).
- `src/app/diff_view/render.rs` — replaced wholesale. New entry point builds two `input_text_multiline` widgets with overlay calls between them.
- `src/app/diff_view/mod.rs` — slimmed down: scroll sync between the two multiline widgets, anchor click handling on the gutter, hover overlay dispatch.
- `src/app/diff_view/input.rs` — deleted entirely (selection-replace splice helper and `compute_*_split` go away).
- `src/app/diff_view/tests.rs` — most existing headless tests are `#[ignore]`'d in the UI-rewrite task; new behavior tests added before old ones are deleted.
- `src/app/input.rs` — deleted entirely (selection state machine).
- `src/app/input_imgui.rs` — deleted entirely.
- `src/app/merge_view.rs` — three multiline widgets for base/local/remote with the same overlay machinery. Per-row `draw_row` deleted.
- `src/app/mod.rs` — `do_copy`, `copy_enabled`, `PendingKey`, `inject_pending_key`, the post-build do_copy hook, and the Ctrl+C/Ctrl+A handlers in `keyboard_shortcuts` all go away. Save uses `a_text` directly. Edit menu items are simplified.

**Created:**
- `src/app/diff_view/overlay.rs` — new module: `paint_row_overlays`, `line_y_for`, hover detection, anchor-line lookup helpers. Pure functions where possible; tested standalone.

---

## Task 1: `read_text` / `write_text` trailing-newline awareness

Additive change. No behavior change to anything else yet — this preps `io.rs` for the storage refactor.

**Files:**
- Modify: `src/io.rs`
- Modify: callers of `read_text` (search via grep, expected to be in `src/app/mod.rs` and `src/session.rs`).

- [ ] **Step 1: Update `read_text` signature and add a unit test**

In `src/io.rs`, replace the `read_text` function and add tests:

```rust
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of reading a text file: the contents WITHOUT any trailing
/// `\n`, plus a flag indicating whether the source file ended in `\n`.
/// `write_text` re-applies the trailing newline based on the flag so
/// save/load preserves the original convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRead {
    pub text: String,
    pub trailing_newline: bool,
}

pub fn read_text<P: AsRef<Path>>(path: P) -> Result<TextRead, IoError> {
    let bytes = std::fs::read(path)?;
    let raw = if let Ok(s) = std::str::from_utf8(&bytes) {
        s.to_string()
    } else {
        let (cow, _, _) = encoding_rs::UTF_8.decode(&bytes);
        cow.into_owned()
    };
    let trailing_newline = raw.ends_with('\n');
    let text = if trailing_newline {
        raw[..raw.len() - 1].to_string()
    } else {
        raw
    };
    Ok(TextRead { text, trailing_newline })
}

pub fn write_text<P: AsRef<Path>>(
    path: P,
    content: &str,
    trailing_newline: bool,
) -> Result<(), IoError> {
    if trailing_newline {
        let mut out = String::with_capacity(content.len() + 1);
        out.push_str(content);
        out.push('\n');
        std::fs::write(path, out)?;
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_strips_trailing_newline_and_records_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "alpha\nbeta\n").unwrap();
        let r = read_text(&p).unwrap();
        assert_eq!(r.text, "alpha\nbeta");
        assert!(r.trailing_newline);
    }

    #[test]
    fn read_keeps_text_when_no_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "alpha\nbeta").unwrap();
        let r = read_text(&p).unwrap();
        assert_eq!(r.text, "alpha\nbeta");
        assert!(!r.trailing_newline);
    }

    #[test]
    fn write_preserves_trailing_newline_via_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        write_text(&p, "alpha\nbeta", true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"alpha\nbeta\n");
        write_text(&p, "alpha\nbeta", false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"alpha\nbeta");
    }
}
```

Add `tempfile` to `[dev-dependencies]` in `Cargo.toml` if not already there:

```bash
grep -q 'tempfile' Cargo.toml || cargo add --dev tempfile
```

- [ ] **Step 2: Update callers of `read_text`**

```bash
grep -rn 'read_text' src/
```

Likely sites: `src/app/mod.rs` (file-open paths), maybe `src/main.rs` for CLI args. Each call site receiving `String` now receives `TextRead`. For now (this task), drop the trailing_newline flag at the call site and keep the existing behavior:

```rust
let text = io::read_text(&path)?.text;  // existing call sites
```

The trailing_newline flag will get plumbed through the session in Task 3.

- [ ] **Step 3: Compile + tests**

Run: `cargo build --no-default-features` and `cargo build`.
Expected: success.

Run: `cargo test --no-default-features --lib io::tests` and `cargo test --lib`.
Expected: all PASS, including 3 new io tests.

- [ ] **Step 4: Commit**

```bash
git add src/io.rs Cargo.toml Cargo.lock
git add -u  # call-site changes
git commit -m "io: read_text returns TextRead { text, trailing_newline }"
```

---

## Task 2: Add `ThreeWaySide` enum and `SideRef`

Additive scaffolding. The new types compile but aren't used yet.

**Files:**
- Modify: `src/session.rs`

- [ ] **Step 1: Add the enums**

In `src/session.rs`, near the existing `TwoWaySide`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreeWaySide {
    Base,
    Local,
    Remote,
}

/// Side reference unifying 2-way and 3-way edits. Used by the new
/// `DiffEdit::SetSide` variant so a single edit type can target any
/// editable pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SideRef {
    TwoWay(TwoWaySide),
    ThreeWay(ThreeWaySide),
}
```

- [ ] **Step 2: Compile**

Run: `cargo build --no-default-features` and `cargo build`.
Expected: success. Both enums are `pub` but unused (dead-code warnings — acceptable for this task; they'll be used in Task 4).

- [ ] **Step 3: Commit**

```bash
git add src/session.rs
git commit -m "session: add ThreeWaySide and SideRef enums (unused scaffolding)"
```

---

## Task 3: Session storage flips from `Vec<String>` to `String`

Big refactor. After this task: session stores text as `String`, but the legacy line-based edits (`SetTwoWayLine`, `SpliceTwoWayLines`, `set_two_way_lines`) keep working by splitting/joining internally. UI is unchanged.

**Files:**
- Modify: `src/session.rs` extensively.
- Adapt: `src/app/undo_stack.rs` (the `edit`/`undo` paths read from `a_lines` — they'll now split `a_text`).
- Adapt: callers in `src/app/mod.rs` (save/load), `src/app/diff_view/`, `src/app/merge_view.rs`, `src/app/result_pane.rs`.

- [ ] **Step 1: Flip `SessionMode` to store strings**

In `src/session.rs`, replace the existing enum:

```rust
#[derive(Debug, Clone)]
pub enum SessionMode {
    TwoWay {
        a_text: String,
        b_text: String,
        a_trailing_newline: bool,
        b_trailing_newline: bool,
        anchors: Vec<Anchor>,
        hunks: Vec<Hunk>,
        decisions: HashMap<u32, HunkDecision>,
    },
    ThreeWay {
        base_text: String,
        local_text: String,
        remote_text: String,
        base_trailing_newline: bool,
        local_trailing_newline: bool,
        remote_trailing_newline: bool,
        anchors: Vec<MergeAnchor>,
        hunks: Vec<MergeHunk>,
        resolutions: HashMap<u32, Resolution>,
    },
}
```

- [ ] **Step 2: Add `lines_of(s: &str) -> Vec<&str>` helper at module scope in session.rs**

```rust
/// Split a side's `String` into the `&[&str]` shape the diff engine
/// wants. Empty strings produce one empty line; otherwise splits on
/// `'\n'`. Cheap; do not memoize.
pub(crate) fn lines_of(s: &str) -> Vec<&str> {
    if s.is_empty() {
        vec![""]
    } else {
        s.split('\n').collect()
    }
}
```

- [ ] **Step 3: Update `open_two_way_with` and `open_three_way_with` to accept `TextRead` and store strings**

Find the existing `open_two_way_with` and `open_three_way_with`. Both currently call `split_lines(...)` and build `Vec<String>`. Replace with direct text storage:

```rust
pub fn open_two_way_with(
    &self,
    a_text: String,
    b_text: String,
    a_trailing_newline: bool,
    b_trailing_newline: bool,
    engine: Option<String>,
    options: DiffOptions,
) -> Result<SessionId, SessionError> {
    let engine = engine.unwrap_or_else(default_engine_name);
    let hunks = recompute_two_way(&engine, &a_text, &b_text, &[], &options)?;
    let id = self.alloc_id();
    let s = DiffSession {
        id, engine, options,
        mode: SessionMode::TwoWay {
            a_text, b_text,
            a_trailing_newline, b_trailing_newline,
            anchors: vec![], hunks, decisions: HashMap::new(),
        },
        manual_result: None,
    };
    self.sessions.lock().unwrap().insert(id, s);
    Ok(id)
}
```

The convenience `open_two_way` (which currently takes `&str` for both) becomes:

```rust
pub fn open_two_way(
    &self,
    a_text: &str,
    b_text: &str,
    engine: Option<String>,
) -> Result<SessionId, SessionError> {
    self.open_two_way_with(
        a_text.trim_end_matches('\n').to_string(),
        b_text.trim_end_matches('\n').to_string(),
        a_text.ends_with('\n'),
        b_text.ends_with('\n'),
        engine,
        DiffOptions::default(),
    )
}
```

Same shape for the three-way pair.

- [ ] **Step 4: Update `recompute_two_way` to take `&str` text**

```rust
fn recompute_two_way(
    engine_name: &str,
    a_text: &str,
    b_text: &str,
    anchors: &[Anchor],
    opts: &DiffOptions,
) -> Result<Vec<Hunk>, SessionError> {
    let inner = build_engine(engine_name)?;
    let caps = inner.capabilities();
    let a_lines_vec = lines_of(a_text);
    let b_lines_vec = lines_of(b_text);
    let a: Vec<&str> = a_lines_vec.iter().copied().collect();
    let b: Vec<&str> = b_lines_vec.iter().copied().collect();
    let ops: Vec<DiffOp> = if anchors.is_empty() {
        inner.diff(&a, &b, opts)
    } else {
        // (existing DynEngine wrapper code unchanged)
        let wrapper = AnchoredDiff::new(DynEngine(inner.as_ref()), anchors.to_vec());
        wrapper.diff_checked(&a, &b, opts)?
    };
    let mut ops = split_trivial_equals(ops);
    crate::diff::sub_line::populate_pair_spans(&mut ops, opts.sub_line);
    if opts.detect_moves && caps.supports_moves {
        crate::diff::moves::detect_moves(&mut ops, opts);
    }
    Ok(group_into_hunks(&ops))
}
```

`recompute_three_way` is similar — split base/local/remote strings before calling the engine.

- [ ] **Step 5: Replace `set_two_way_line` / `set_two_way_lines` / `splice_two_way_lines` with a single `set_side_text`**

These three legacy methods all manipulated `Vec<String>`. Replace them with one:

```rust
pub fn set_side_text(
    &self,
    id: SessionId,
    side: SideRef,
    new_text: String,
) -> Result<(), SessionError> {
    let mut sessions = self.sessions.lock().unwrap();
    let s = sessions.get_mut(&id).ok_or(SessionError::UnknownSession(id))?;
    match (&mut s.mode, side) {
        (SessionMode::TwoWay { a_text, .. }, SideRef::TwoWay(TwoWaySide::A)) => *a_text = new_text,
        (SessionMode::TwoWay { b_text, .. }, SideRef::TwoWay(TwoWaySide::B)) => *b_text = new_text,
        (SessionMode::ThreeWay { base_text, .. }, SideRef::ThreeWay(ThreeWaySide::Base)) => *base_text = new_text,
        (SessionMode::ThreeWay { local_text, .. }, SideRef::ThreeWay(ThreeWaySide::Local)) => *local_text = new_text,
        (SessionMode::ThreeWay { remote_text, .. }, SideRef::ThreeWay(ThreeWaySide::Remote)) => *remote_text = new_text,
        _ => return Err(SessionError::WrongMode),
    }
    // Recompute hunks via the right path for the mode:
    match &mut s.mode {
        SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
            *hunks = recompute_two_way(&s.engine, a_text, b_text, anchors, &s.options)?;
        }
        SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
            *hunks = recompute_three_way(&s.engine, base_text, local_text, remote_text, anchors, &s.options)?;
        }
    }
    Ok(())
}
```

Delete `set_two_way_line`, `set_two_way_lines`, and `splice_two_way_lines`.

- [ ] **Step 6: Rework `replace_hunk_side`**

Currently operates on `Vec<String>`. New version operates on `String` using the byte-range math helper from Task 5 (write it inline here):

```rust
pub fn replace_hunk_side(
    &self,
    id: SessionId,
    hunk_id: u32,
    target: TwoWaySide,
) -> Result<(), SessionError> {
    let mut sessions = self.sessions.lock().unwrap();
    let s = sessions.get_mut(&id).ok_or(SessionError::UnknownSession(id))?;
    let SessionMode::TwoWay { a_text, b_text, hunks, .. } = &mut s.mode else {
        return Err(SessionError::WrongMode);
    };
    let hunk = hunks.iter().find(|h| h.id == hunk_id)
        .ok_or(SessionError::WrongMode)?  // reuse existing error for now
        .clone();
    let (target_text, source_text, target_range, source_range) = match target {
        TwoWaySide::B => (b_text, a_text.clone(), hunk.b_range, hunk.a_range),
        TwoWaySide::A => (a_text, b_text.clone(), hunk.a_range, hunk.b_range),
    };
    let source_slice = extract_line_range(&source_text, source_range);
    replace_line_range_in_text(target_text, target_range, &source_slice);
    // Recompute hunks
    match &mut s.mode {
        SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
            *hunks = recompute_two_way(&s.engine, a_text, b_text, anchors, &s.options)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Return the substring of `text` covering the line range
/// `(start_line..=end_line)`, 1-based, inclusive on both ends.
/// If end_line == 0 returns "" (empty range; convention for "no
/// lines on this side" used by Delete-only / Insert-only hunks).
fn extract_line_range(text: &str, range: (u32, u32)) -> String {
    if range.0 == 0 || range.1 == 0 || range.0 > range.1 {
        return String::new();
    }
    let lines: Vec<&str> = lines_of(text);
    let lo = (range.0 as usize).saturating_sub(1).min(lines.len());
    let hi = (range.1 as usize).min(lines.len());
    if lo >= hi { return String::new(); }
    lines[lo..hi].join("\n")
}

/// Replace lines in `text` covering `range` (1-based inclusive) with
/// `replacement`. If range is (0, 0) — meaning "no lines on this
/// side" — `replacement` is inserted at the end.
fn replace_line_range_in_text(text: &mut String, range: (u32, u32), replacement: &str) {
    if range.0 == 0 || range.1 == 0 {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(replacement);
        return;
    }
    let lines: Vec<&str> = lines_of(text);
    let lo = (range.0 as usize).saturating_sub(1).min(lines.len());
    let hi = (range.1 as usize).min(lines.len());
    let mut out: Vec<&str> = Vec::new();
    out.extend(lines[..lo].iter().copied());
    if !replacement.is_empty() {
        // Treat replacement as one or more lines.
        out.extend(replacement.split('\n'));
    }
    out.extend(lines[hi..].iter().copied());
    *text = out.join("\n");
}
```

(These helpers will get unit tests in Task 5; for now they exist as inline helpers in `session.rs`.)

- [ ] **Step 7: Update `undo_stack.rs` to read/write through the new String storage**

In `src/app/undo_stack.rs`, all the `SessionMode::TwoWay { a_lines, b_lines, .. }` destructures break. Adapt each to use `a_text`/`b_text`:

- `SetTwoWayLine::edit`: read the line's text via `lines_of(a_text).get(idx).cloned().unwrap_or_default()`. To apply, split `a_text` into lines, replace the indexed line, join. Verbose but mechanical.
- `SpliceTwoWayLines::edit`: same pattern — split, splice, join.
- `ReplaceHunkSide::edit`: snapshot old `a_text`/`b_text` (as String) instead of `Vec<String>` lines. Change `old_target_lines` field name to `old_target_text: Option<String>`.

Note: `SetTwoWayLine` and `SpliceTwoWayLines` are about to be deleted in Task 4. Don't fight to keep them clean — just keep them compiling.

- [ ] **Step 8: Update callers in `src/app/`**

```bash
grep -rn 'a_lines\|b_lines\|base_lines\|local_lines\|remote_lines' src/
```

Each match either:
- Reads a line by index: switch to `lines_of(a_text).get(idx)`.
- Iterates all lines: switch to `a_text.lines()` or `lines_of(a_text).iter()`.
- Computes total line count: `a_text.lines().count().max(1)` (the `.max(1)` accounts for empty string = 1 line).

Result-pane (`result_pane.rs`) and save logic in `mod.rs` change: write `text` directly to file via the new `write_text(path, text, trailing_newline)`.

- [ ] **Step 9: Update existing session tests in `src/session.rs::tests`**

Find tests that construct sessions via `open_two_way("a\nb\nc\n", ...)`. The `open_two_way` API still takes `&str`, so callers don't change. Tests that inspect `a_lines` in `SessionMode::TwoWay { a_lines, .. }` need to switch to inspecting `a_text` and split where needed.

- [ ] **Step 10: Run the full test suite**

Run: `cargo build`
Expected: success.

Run: `cargo test --lib`
Expected: PASS. Many old tests adapted; nothing yet deleted.

- [ ] **Step 11: Commit**

```bash
git add -u
git commit -m "session: store text as String per side; preserve trailing_newline"
```

---

## Task 4: New `SetSide` / `SetThreeWaySide` edit types

Adds the new edit types; the old ones get deleted in Task 11 after the UI rewrite is done. They coexist temporarily.

**Files:**
- Modify: `src/app/undo_stack.rs`

- [ ] **Step 1: Add the new variants**

In `src/app/undo_stack.rs`, add to the `DiffEdit` enum:

```rust
/// Replace the entire text of one side (2-way `A` or `B`, or 3-way
/// `Base` / `Local` / `Remote`). Emitted on every `input_text_multiline`
/// `changed = true` return; consecutive entries on the same
/// `(session_id, side)` coalesce via `DiffEdit::merge`.
SetSide {
    session_id: SessionId,
    side: SideRef,
    new_text: String,
    old_text: Option<String>,
},
```

Import: `use crate::session::{SessionId, SessionMode, SessionStore, SideRef, ThreeWaySide, TwoWaySide};`

- [ ] **Step 2: Implement `edit` / `undo` for `SetSide`**

In the `impl Edit for DiffEdit::edit` match, add:

```rust
DiffEdit::SetSide { session_id, side, new_text, old_text } => {
    if old_text.is_none() {
        if let Ok(snap) = store.snapshot(*session_id) {
            *old_text = current_side_text(&snap.mode, *side);
        }
    }
    let _ = store.set_side_text(*session_id, *side, new_text.clone());
}
```

In the `undo` match:

```rust
DiffEdit::SetSide { session_id, side, old_text, .. } => {
    if let Some(old) = old_text.clone() {
        let _ = store.set_side_text(*session_id, *side, old);
    }
}
```

Add `current_side_text` helper at module scope:

```rust
fn current_side_text(mode: &SessionMode, side: SideRef) -> Option<String> {
    match (mode, side) {
        (SessionMode::TwoWay { a_text, .. }, SideRef::TwoWay(TwoWaySide::A)) => Some(a_text.clone()),
        (SessionMode::TwoWay { b_text, .. }, SideRef::TwoWay(TwoWaySide::B)) => Some(b_text.clone()),
        (SessionMode::ThreeWay { base_text, .. }, SideRef::ThreeWay(ThreeWaySide::Base)) => Some(base_text.clone()),
        (SessionMode::ThreeWay { local_text, .. }, SideRef::ThreeWay(ThreeWaySide::Local)) => Some(local_text.clone()),
        (SessionMode::ThreeWay { remote_text, .. }, SideRef::ThreeWay(ThreeWaySide::Remote)) => Some(remote_text.clone()),
        _ => None,
    }
}
```

- [ ] **Step 3: Implement `merge` for `SetSide`**

In the `DiffEdit::merge` match, add:

```rust
(
    DiffEdit::SetSide { session_id: a_sid, side: a_side, new_text: a_new, .. },
    DiffEdit::SetSide { session_id: b_sid, side: b_side, new_text: b_new, .. },
) if *a_sid == b_sid && *a_side == b_side => {
    *a_new = b_new;
    Merged::Yes
}
```

(Falls through to the existing catch-all if sides differ.)

- [ ] **Step 4: Add unit tests for SetSide**

In `src/app/undo_stack.rs` add `#[cfg(test)] mod set_side_tests`:

```rust
#[cfg(test)]
mod set_side_tests {
    use super::*;
    use crate::session::{SessionStore, SideRef, TwoWaySide};
    use undo::Record;

    #[test]
    fn set_side_edit_then_undo_round_trips() {
        let store = SessionStore::new();
        let id = store.open_two_way("hello\n", "hello\n", None).unwrap();
        let mut rec: Record<DiffEdit> = Record::new();
        let mut store_mut = store;  // for &mut access in this test
        rec.edit(&mut store_mut, DiffEdit::SetSide {
            session_id: id,
            side: SideRef::TwoWay(TwoWaySide::A),
            new_text: "world".into(),
            old_text: None,
        });
        let snap = store_mut.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "world");
        rec.undo(&mut store_mut);
        let snap = store_mut.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "hello");
    }

    #[test]
    fn consecutive_set_side_same_side_coalesce() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\n", "a\n", None).unwrap();
        let mut rec: Record<DiffEdit> = Record::new();
        let mut store_mut = store;
        for new in ["b", "bc", "bcd"] {
            rec.edit(&mut store_mut, DiffEdit::SetSide {
                session_id: id,
                side: SideRef::TwoWay(TwoWaySide::A),
                new_text: new.into(),
                old_text: None,
            });
        }
        // One undo reverts back to "a", not stepping through "bc" and "b".
        rec.undo(&mut store_mut);
        let snap = store_mut.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "a");
        // Confirm no second undo to step through is available.
        assert!(!rec.can_undo());
    }
}
```

(If `SessionStore` doesn't implement clone/move for this test pattern, adjust to take `&mut` directly. The undo crate's `Record::edit` requires `&mut Target` so be sure the test calls `&mut store`.)

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Expected: PASS (104 → 106 tests).

- [ ] **Step 6: Commit**

```bash
git add src/app/undo_stack.rs
git commit -m "undo_stack: add DiffEdit::SetSide (coalesces same-side edits)"
```

---

## Task 5: UI rewrite — two `input_text_multiline` per 2-way tab

The big one. Replaces `diff_view`'s entire UI. After this task: text renders, edits work, but NO per-row decorations, NO hover overlay, NO anchor gutter, NO sub-line spans. Subsequent tasks add those back.

**Files:**
- Modify: `src/app/diff_view/mod.rs` (heavy rewrite).
- Modify: `src/app/diff_view/common.rs` (slim down — drop selection/drag state).
- Delete: `src/app/diff_view/render.rs` (per-row pipeline gone).
- Delete: `src/app/diff_view/input.rs` (cross-row machinery gone).
- Delete: `src/app/input.rs` (selection state machine).
- Delete: `src/app/input_imgui.rs`.
- Modify: `src/app/diff_view/tests.rs` (mark most existing tests `#[ignore]`).
- Modify: `src/app/mod.rs` (callers).

- [ ] **Step 1: Slim down `DiffViewState`**

In `src/app/diff_view/common.rs`, replace the existing `DiffViewState` with:

```rust
#[derive(Default)]
pub struct DiffViewState {
    /// Buffer mirror of `session.a_text`. Synced at start of every
    /// render; written-back on every `input_text_multiline` change.
    pub(super) a_buf: String,
    pub(super) b_buf: String,
    /// Last scroll_y per pane (for sync math).
    pub(super) last_left_scroll_y: f32,
    pub(super) last_right_scroll_y: f32,
    /// Pending scroll set by sync; consumed on next render via
    /// `igSetNextWindowScroll`.
    pub(super) pending_left_scroll: Option<f32>,
    pub(super) pending_right_scroll: Option<f32>,
    /// Last scroll_x per pane (test harness reads these).
    pub last_left_scroll_x: f32,
    pub last_right_scroll_x: f32,
    /// Two-click anchor creation: line picked on side A awaiting partner on B.
    pub(super) pending_a: Option<u32>,
    pub(super) pending_b: Option<u32>,
    /// Jump-to-pair and arrival flash (unchanged).
    pub(super) pending_jump: Option<PendingJump>,
    pub(super) flash: Option<MoveFlash>,
}
```

Delete `selection`, `drag`, `arrow_focus`, `caret_blink_reset`, `input_epoch`, `pin_scroll_x_after_splice`, `last_active_input_selection`, `last_active_caret_offset`. Also delete `Selection`, `SelPoint`, `DragState`, `Side`, `Row`, `Segment` types if they're only used by deleted code. Keep `Side` and `PendingJump`/`MoveFlash`/`MOVE_FLASH_FRAMES`/`MOVE_FLASH_PEAK_ALPHA` (used by overlays in later tasks).

- [ ] **Step 2: Delete `src/app/diff_view/render.rs` and `src/app/diff_view/input.rs`**

```bash
git rm src/app/diff_view/render.rs src/app/diff_view/input.rs
```

- [ ] **Step 3: Delete `src/app/input.rs` and `src/app/input_imgui.rs`**

```bash
git rm src/app/input.rs src/app/input_imgui.rs
```

Update `src/app/mod.rs` to drop the module declarations for those (`mod input;`, `mod input_imgui;`).

- [ ] **Step 4: Rewrite `diff_view::mod` with two multiline widgets**

In `src/app/diff_view/mod.rs`, replace the entire file with:

```rust
//! 2-way diff view: two `input_text_multiline` widgets side-by-side
//! with a bezier connector strip in the middle. Per-row decorations
//! (backgrounds, sub-line spans, hover overlays, anchor gutter) are
//! draw-list overlays added by `overlay::paint_row_overlays` (next
//! task).

use std::cell::Cell;
use std::collections::HashSet;

use imgui::{FontId, Ui};

use crate::diff::{Anchor, Hunk};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};

mod common;
mod overlay;
#[cfg(test)]
mod tests;

pub use common::{DiffViewState, Side, PendingJump, MoveFlash, MOVE_FLASH_FRAMES, MOVE_FLASH_PEAK_ALPHA};
use common::{CONNECTOR_W, gutter_w, line_h};

use super::undo_stack::DiffEdit;

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[Hunk],
    anchors: &[Anchor],
    status: &mut String,
    state: &mut DiffViewState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    pending_edits: &mut Vec<DiffEdit>,
    a_highlights: &[crate::app::syntax::LineSpans],
    b_highlights: &[crate::app::syntax::LineSpans],
) {
    let _ = anchors; // overlays use anchors in task 7
    let _ = a_highlights;
    let _ = b_highlights;
    let _ = status;

    // Sync buffers from session at start of frame. (Session text is the
    // source of truth between frames; the widget's buffer is the source
    // of truth WITHIN a frame.)
    let snap = match store.snapshot(session_id) {
        Ok(s) => s,
        Err(_) => return,
    };
    let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode else {
        return;
    };
    if state.a_buf != *a_text {
        state.a_buf = a_text.clone();
    }
    if state.b_buf != *b_text {
        state.b_buf = b_text.clone();
    }

    let avail = ui.content_region_avail();
    let total_w = avail[0];
    let pane_w = ((total_w - CONNECTOR_W) * 0.5).max(100.0);
    let pane_h = avail[1].max(100.0);

    let panes_top_left = ui.cursor_screen_pos();
    let left_pos = panes_top_left;
    let connector_pos = [left_pos[0] + pane_w, left_pos[1]];
    let right_pos = [connector_pos[0] + CONNECTOR_W, left_pos[1]];

    let _font_tok = mono_font.map(|f| ui.push_font(f));

    // Left pane: gutter + multiline widget.
    let (left_widget_rect, left_scroll_y) = render_pane(
        ui, state, left_pos, pane_w, pane_h, Side::Left, session_id,
        pending_edits, hunks,
    );

    // Connector strip — empty for now, ribbons added back in task 6.
    ui.set_cursor_screen_pos(connector_pos);
    ui.invisible_button("connector_strip", [CONNECTOR_W, pane_h]);

    // Right pane: gutter + multiline widget.
    let (right_widget_rect, right_scroll_y) = render_pane(
        ui, state, right_pos, pane_w, pane_h, Side::Right, session_id,
        pending_edits, hunks,
    );

    // Stash scroll positions for the test harness.
    state.last_left_scroll_y = left_scroll_y;
    state.last_right_scroll_y = right_scroll_y;
    let _ = left_widget_rect;
    let _ = right_widget_rect;

    // Reserve space so subsequent widgets land below the panes.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + pane_h]);

    let _ = focus_request;
}

fn render_pane(
    ui: &Ui,
    state: &mut DiffViewState,
    pane_pos: [f32; 2],
    pane_w: f32,
    pane_h: f32,
    side: Side,
    session_id: SessionId,
    pending_edits: &mut Vec<DiffEdit>,
    _hunks: &[Hunk],
) -> ([f32; 4], f32) {
    let g_w = gutter_w();
    let widget_pos = [pane_pos[0] + g_w, pane_pos[1]];
    let widget_w = pane_w - g_w;

    // Gutter strip — drawn in overlay task; for now reserve via invisible_button.
    ui.set_cursor_screen_pos(pane_pos);
    ui.invisible_button(format!("gutter_{:?}", side), [g_w, pane_h]);

    // Apply any pending scroll set last frame.
    let pending_scroll = match side {
        Side::Left => state.pending_left_scroll.take(),
        Side::Right => state.pending_right_scroll.take(),
    };
    if let Some(y) = pending_scroll {
        unsafe {
            imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x: -1.0, y });
        }
    }

    ui.set_cursor_screen_pos(widget_pos);
    let buf = match side {
        Side::Left => &mut state.a_buf,
        Side::Right => &mut state.b_buf,
    };
    let widget_id = format!("##diffie_pane_{:?}", side);
    let changed = ui
        .input_text_multiline(&widget_id, buf, [widget_w, pane_h])
        .no_undo_redo(true)
        .build();
    let scroll_y = 0.0; // placeholder until task 6 wires the inside-widget scroll read
    if changed {
        let side_ref = SideRef::TwoWay(match side {
            Side::Left => TwoWaySide::A,
            Side::Right => TwoWaySide::B,
        });
        pending_edits.push(DiffEdit::SetSide {
            session_id,
            side: side_ref,
            new_text: buf.clone(),
            old_text: None,
        });
    }
    let widget_rect = [widget_pos[0], widget_pos[1], widget_pos[0] + widget_w, widget_pos[1] + pane_h];
    (widget_rect, scroll_y)
}
```

- [ ] **Step 5: Slim `common.rs`**

Replace `DiffViewState` per Step 1; delete `Selection`, `SelPoint`, `DragState`, `Row`, `Segment`, all helpers that touched them. Keep:

```rust
use serde::{Deserialize, Serialize};

pub(super) const CONNECTOR_W: f32 = 60.0;
const ROW_H_BASE: f32 = 18.0;  // adjust to whatever the current value is
const GUTTER_W_BASE: f32 = 60.0;

pub(super) fn line_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

pub(super) fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Side { Left, Right }

#[derive(Clone, Copy, Debug)]
pub(super) struct PendingJump {
    pub(super) session_id: crate::session::SessionId,
    pub(super) pane: Side,
    pub(super) target_line: crate::diff::LineNo,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MoveFlash {
    pub(super) session_id: crate::session::SessionId,
    pub(super) hunk_id: u32,
    pub(super) frames_remaining: u8,
}

pub(super) const MOVE_FLASH_FRAMES: u8 = 30;
pub(super) const MOVE_FLASH_PEAK_ALPHA: f32 = 0.20;

// DiffViewState definition from Step 1 goes here.
```

Also keep `apply_two_way_decisions` and any other pure helpers that aren't UI-coupled.

- [ ] **Step 6: Create empty `src/app/diff_view/overlay.rs` placeholder**

```rust
//! Draw-list overlays painted on top of the per-pane `input_text_multiline`
//! widget: row backgrounds, sub-line spans, hover-overlay positioning,
//! and the anchor gutter. Pure functions where possible.
//!
//! Wired up by `paint_row_overlays` in task 6.
```

- [ ] **Step 7: Mark old per-row tests `#[ignore]`**

In `src/app/diff_view/tests.rs`, search for all `#[test] fn ...` and prefix each with `#[ignore]` IF the test depends on deleted internals (`view_state.selection`, `view_state.drag`, per-row `draw_row`, `update_selection`, `compute_*_split`, the splice handler, etc.).

```bash
# Quick way: every test currently in tests.rs is per-row-state-based and
# will need adapting or deleting. Mark the whole module:
sed -i 's/    #\[test\]/    #[ignore]\n    #[test]/' src/app/diff_view/tests.rs
```

(Eyeball: the `splice_helper_tests` and word-bounds tests are pure functions and probably still work — un-ignore those after this batch operation if so.)

Tests that no longer compile (because `Selection`, `update_selection`, etc. are gone) should be commented out wholesale rather than marked `#[ignore]`. Use `// REMOVED in multiline rewrite — see task 11` markers.

- [ ] **Step 8: Build + run tests**

Run: `cargo build`
Expected: success. Compiler will surface every place that referenced deleted types — fix each by deletion or stubbing. The pattern: anything that used `state.selection`, `update_selection`, `Selection`, `SelPoint`, etc. either gets deleted or simplified.

Run: `cargo test --lib`
Expected: PASS for tests that didn't get ignored/removed. The ignored ones don't run; the still-active ones (engine, session, sub-line) must remain green.

- [ ] **Step 9: Manual smoke test**

If you can run the GUI: `cargo run`, open a 2-way diff with two text files, verify:
- Both panes render text.
- You can type into either pane and the diff recomputes.
- Scrolling works (each pane independently for now).

If you can't run the GUI: note "manual smoke pending" in the commit message.

- [ ] **Step 10: Commit**

```bash
git add -u
git add src/app/diff_view/overlay.rs
git commit -m "diff_view: rewrite UI on input_text_multiline (no overlays yet)"
```

---

## Task 6: Overlays — row backgrounds, sub-line spans, scroll read

Adds the visual decorations BACK on top of the multiline widget.

**Files:**
- Modify: `src/app/diff_view/overlay.rs`
- Modify: `src/app/diff_view/mod.rs` (call into overlay after each widget builds)

- [ ] **Step 1: Read scroll_y from inside the multiline widget**

In `mod.rs::render_pane`, after `.build()`, you need the widget's internal scroll. The multiline widget itself is a `BeginChild`; `ui.scroll_y()` outside the widget gives the outer window's scroll, not the multiline's. There are two options:

- **Preferred:** Use the `ALWAYS` input_text callback to read scroll. Inside the callback, call `data` methods… but `TextCallbackData` doesn't expose scroll. So fall back to:
- **Pragmatic:** Detect the multiline's child window by name. ImGui uses an internal name `##<widget_id>` for the child. Use `ui.is_window_hovered()` while inside the widget's content rect with `is_window_hovered_with_flags(ImGuiHoveredFlags_ChildWindows)`. To read scroll, push/pop the focus and use `igGetWindowScrollY`. Easier alternative: store the scroll via the imgui callback by reading `data.has_selection()`-adjacent state… NO clean way.

The cleanest approach: drop a `child_window` of our own around the input_text_multiline so WE own scroll. But that adds nesting. Instead, use this pattern from result_pane: input_text_multiline already manages scroll internally; we read scroll via:

```rust
// After build, push the widget's internal child by ID and query scroll.
// ImGui constructs the child as "{widget_id}_child". imgui-rs doesn't
// expose `child_window` by string id, so we use raw sys:
let scroll_y = unsafe {
    let child_id = imgui::sys::igGetID_Str(
        std::ffi::CString::new(format!("##diffie_pane_{:?}", side)).unwrap().as_ptr()
    );
    // Walking imgui's window list for the child is brittle. Better:
    // call igBeginChild on a child of OUR construction that the multiline
    // shares scroll with — not possible, the multiline owns its child.
    0.0
};
```

Actually the spec calls out scroll sync as the major risk. **Resolve it now in this task with a spike:**

Run: `cargo run` and add `println!("scroll: {}", ui.scroll_y())` at a few placement points; find which call site returns the multiline's internal scroll. If none does, the fallback is to wrap input_text_multiline in our own `child_window` and ignore the widget's internal scroll (so the widget always fills the child fully, no internal scroll). Then `ui.scroll_y()` on our outer child IS the scroll.

**Recommended implementation:** wrap each pane in our own `child_window`:

```rust
ui.set_cursor_screen_pos(widget_pos);
let mut scroll_y_out = 0.0;
ui.child_window(&format!("##diffie_pane_child_{:?}", side))
    .size([widget_w, pane_h])
    .scroll_bar(true)
    .build(|| {
        scroll_y_out = ui.scroll_y();
        // The multiline widget grows to its content; we want NO internal
        // scroll on it. Use size [content_w, 0.0] which means "auto fit
        // to content".
        let content_h = state_buf_lines(buf) as f32 * line_h();
        ui.input_text_multiline(
            &format!("##diffie_pane_{:?}", side),
            buf,
            [widget_w, content_h.max(pane_h)],
        )
        .no_undo_redo(true)
        .build()
    });
```

Wrap the widget in our own child window. The widget itself is sized to fit its content (`content_h = line_count * line_h`). Our child window scrolls; the widget doesn't. `ui.scroll_y()` inside the closure gives us the child's scroll, which we capture.

Implement this approach. Test that scrolling works AND `scroll_y_out` updates as expected.

- [ ] **Step 2: Implement `paint_row_overlays`**

In `src/app/diff_view/overlay.rs`:

```rust
use imgui::{ImColor32, Ui};
use crate::diff::{DiffOp, Hunk, LineNo};
use super::common::{Side, line_h};
use crate::app::theme;

/// Paint per-row backgrounds and sub-line spans for one pane.
/// `widget_rect = [x0, y0, x1, y1]` is the screen-space rect of the
/// pane's text content (just the input_text_multiline, not including
/// the gutter). `scroll_y` is the pane's vertical scroll.
pub(super) fn paint_row_overlays(
    ui: &Ui,
    widget_rect: [f32; 4],
    hunks: &[Hunk],
    side: Side,
    scroll_y: f32,
) {
    let dl = ui.get_window_draw_list();
    let lh = line_h();
    let widget_top = widget_rect[1];
    let widget_h = widget_rect[3] - widget_rect[1];
    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line  = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    // Helper: screen y for a given 1-based line number.
    let y_for = |line: u32| -> f32 {
        widget_top + (line as f32 - 1.0) * lh - scroll_y
    };

    for h in hunks {
        let (range, op_select): (
            (LineNo, LineNo),
            fn(&DiffOp) -> Option<(LineNo, bool)>,
        ) = match side {
            Side::Left => (h.a_range, |op| match op {
                DiffOp::Equal { a, .. } => Some((*a, false)),
                DiffOp::Delete { a, move_id, .. } => Some((*a, move_id.is_some())),
                _ => None,
            }),
            Side::Right => (h.b_range, |op| match op {
                DiffOp::Equal { b, .. } => Some((*b, false)),
                DiffOp::Insert { b, move_id, .. } => Some((*b, move_id.is_some())),
                _ => None,
            }),
        };
        if range == (0, 0) { continue; }
        if range.1 < first_line || range.0 > last_line { continue; }
        for op in &h.ops {
            let Some((ln, is_moved)) = op_select(op) else { continue };
            if ln < first_line || ln > last_line { continue; }
            let y = y_for(ln);
            let color = if is_moved {
                theme::with_alpha(theme::PEACH, 0.30)
            } else {
                match op {
                    DiffOp::Equal { .. } => continue,
                    DiffOp::Delete { .. } => [0.55, 0.18, 0.18, 0.30],
                    DiffOp::Insert { .. } => [0.18, 0.50, 0.22, 0.30],
                }
            };
            dl.add_rect(
                [widget_rect[0], y],
                [widget_rect[2], y + lh],
                color,
            ).filled(true).build();
        }
    }
}
```

(Sub-line spans are a follow-up in the same task — add them once the row-bg pass is working.)

- [ ] **Step 3: Add sub-line span painting**

Extend `paint_row_overlays` to walk each Delete/Insert op's `spans: Option<Vec<SubSpan>>` and paint char-level rects via the same line-y math plus char_w from `ui.calc_text_size("m")[0]`. Mirror the existing sub-line painting from the old `draw_row`.

- [ ] **Step 4: Call `paint_row_overlays` from `mod.rs::render_pane`**

After the widget builds (inside the child_window closure):

```rust
paint_row_overlays(ui, widget_rect, hunks, side, scroll_y_out);
```

- [ ] **Step 5: Add unit tests for line-y math**

In `src/app/diff_view/overlay.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_y_math_at_zero_scroll() {
        // (No actual painting in tests — just verify the math helper
        // returns expected screen-y for given (widget_top, line, scroll).)
        // Factor `y_for` out as a pub(super) function `fn line_screen_y(
        //     widget_top: f32, line: u32, scroll_y: f32, line_h: f32) -> f32`
        // to make this testable.
        let y = line_screen_y(100.0, 1, 0.0, 20.0);
        assert_eq!(y, 100.0);
        let y = line_screen_y(100.0, 5, 0.0, 20.0);
        assert_eq!(y, 100.0 + 80.0);
    }

    #[test]
    fn line_y_math_with_scroll() {
        let y = line_screen_y(100.0, 5, 40.0, 20.0);
        assert_eq!(y, 100.0 + 80.0 - 40.0);
    }
}
```

Refactor the closure inside `paint_row_overlays` into a free function:

```rust
pub(super) fn line_screen_y(widget_top: f32, line: u32, scroll_y: f32, line_h: f32) -> f32 {
    widget_top + (line as f32 - 1.0) * line_h - scroll_y
}
```

- [ ] **Step 6: Build + tests**

Run: `cargo test --lib`
Expected: PASS, including 2 new line-y tests.

- [ ] **Step 7: Manual smoke**

If GUI runs: open a diff with known Delete/Insert/Equal hunks, verify the per-row backgrounds appear in the right rows and colors. Scroll; verify the bgs scroll with the text.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "diff_view: paint_row_overlays — row bg + sub-line spans on multiline"
```

---

## Task 7: Hover overlay panel + anchor gutter clicks

**Files:**
- Modify: `src/app/diff_view/overlay.rs` (hover detection, anchor-line lookup).
- Modify: `src/app/diff_view/mod.rs` (panel rendering, gutter click handling).

- [ ] **Step 1: Hover detection in `paint_row_overlays`**

Add an out-cell parameter for the hovered hunk:

```rust
pub(super) fn paint_row_overlays(
    ui: &Ui,
    widget_rect: [f32; 4],
    hunks: &[Hunk],
    side: Side,
    scroll_y: f32,
    hover_out: &Cell<Option<(u32, [f32; 2])>>,  // hunk_id + screen pos
) {
    // existing loop ...
    // additionally: if mouse is over a row in a change hunk on this side,
    // set hover_out to (h.id, [widget_rect[0], y_of_first_visible_row_in_hunk]).
}
```

Determine the hovered row: `let mouse_y = ui.io().mouse_pos[1]; let line = ((mouse_y - widget_rect[1] + scroll_y) / line_h()) as u32 + 1;` then check whether `line` falls inside any change hunk's range.

- [ ] **Step 2: Draw the hover overlay panel**

Adapt the existing `draw_control_overlay` from the deleted `render.rs` (find it in git history at commit `5bbba33^`). It draws Apply A→B, B→A, ↕ small_buttons in a 200/240px panel. Move it to `overlay.rs` and call it after the row-bg pass when `hover_out` is set.

Click handlers push `DiffEdit::ReplaceHunkSide` to `pending_edits` (same as today) and set `pending_jump_cell` for `↕`.

- [ ] **Step 3: Anchor gutter clicks**

In `mod.rs::render_pane`, replace the gutter `invisible_button` to also map clicks to line numbers and call `handle_anchor_click(state, store, session_id, side, clicked_line, status)`:

```rust
ui.set_cursor_screen_pos(pane_pos);
if ui.invisible_button(format!("gutter_{:?}", side), [g_w, pane_h]) {
    let mouse_y = ui.io().mouse_pos[1];
    let line = ((mouse_y - pane_pos[1] + state.scroll_y_for(side)) / line_h()) as u32 + 1;
    handle_anchor_click(state, side, line, status, store, session_id);
}
```

Define `handle_anchor_click` in `mod.rs` (adapted from the deleted `handle_anchor_clicks` in `input.rs`):

```rust
fn handle_anchor_click(
    state: &mut DiffViewState,
    side: Side,
    line: u32,
    status: &mut String,
    store: &SessionStore,
    session_id: SessionId,
) {
    match side {
        Side::Left => state.pending_a = Some(line),
        Side::Right => state.pending_b = Some(line),
    }
    if let (Some(a), Some(b)) = (state.pending_a, state.pending_b) {
        match store.add_anchor_two_way(session_id, Anchor { a, b }) {
            Ok(()) => *status = format!("anchor added: A:{a} ↔ B:{b}"),
            Err(e) => *status = format!("anchor error: {e}"),
        }
        state.pending_a = None;
        state.pending_b = None;
    }
}
```

- [ ] **Step 4: Draw anchor dots in the gutter**

After the gutter invisible_button, paint dots for existing anchors using the draw list:

```rust
let dl = ui.get_window_draw_list();
for anc in anchors {
    let line = match side { Side::Left => anc.a, Side::Right => anc.b };
    let y = pane_pos[1] + (line as f32 - 1.0) * line_h() - scroll_y + line_h() * 0.5;
    dl.add_circle([pane_pos[0] + g_w * 0.5, y], 3.0, theme::LAVENDER).filled(true).build();
}
```

- [ ] **Step 5: Add line numbers in the gutter**

For each visible line, paint the line number right-aligned at `(pane_pos[0] + g_w - 4, y)`:

```rust
for line in first_visible..=last_visible {
    let y = line_screen_y(pane_pos[1], line, scroll_y, line_h());
    let text = format!("{line}");
    let text_w = ui.calc_text_size(&text)[0];
    dl.add_text([pane_pos[0] + g_w - 4.0 - text_w, y + 2.0], theme::OVERLAY1, &text);
}
```

- [ ] **Step 6: Unit tests for anchor line lookup and hover detection**

In `overlay.rs::tests`:

```rust
#[test]
fn anchor_click_maps_mouse_y_to_line() {
    // Given pane_top=100, line_h=20, scroll_y=40:
    //   mouse_y=120 → line 3 (40px = 2 lines scrolled, then 20px in = 1 more)
    let line = mouse_y_to_line(120.0, 100.0, 40.0, 20.0);
    assert_eq!(line, 3);
}

#[test]
fn hover_detection_finds_change_hunk() {
    // Construct two hunks and verify that a mouse_y in the second hunk's
    // a_range returns that hunk's id.
    // ...
}
```

Refactor `mouse_y_to_line` out as a `pub(super) fn mouse_y_to_line(mouse_y, pane_top, scroll_y, line_h) -> u32`.

- [ ] **Step 7: Build + tests**

Run: `cargo build && cargo test --lib`
Expected: PASS.

- [ ] **Step 8: Manual smoke**

If GUI: hover over a change hunk and verify the panel appears with Apply A→B / B→A buttons. Click `Apply A→B` and verify B side updates. Right-click… wait, anchor uses LMB on the gutter — verify it.

- [ ] **Step 9: Commit**

```bash
git add -u
git commit -m "diff_view: hover overlay panel + anchor gutter + line numbers"
```

---

## Task 8: Scroll sync between the two multiline widgets

**Files:**
- Modify: `src/app/diff_view/mod.rs`

- [ ] **Step 1: Detect which pane scrolled this frame**

After both panes render, compare each pane's just-captured `scroll_y_out` to the previously stored `state.last_left_scroll_y` / `last_right_scroll_y`. Whichever changed is the "driver".

- [ ] **Step 2: Compute the target scroll for the other pane**

Reuse the existing `target_scroll` helper (currently in `common.rs:659`). Its signature is `(src_scroll, src_view_h, dst_view_h, src_ranges, dst_ranges) -> Option<f32>`. The "ranges" inputs are `Vec<(u32, f32, f32)>` mapping `hunk_id` to `(top_y, bot_y)` in content space.

Build these ranges inline from the hunks list using `line_screen_y` math at scroll_y=0:

```rust
fn build_pane_ranges(hunks: &[Hunk], side: Side) -> Vec<(u32, f32, f32)> {
    let lh = line_h();
    hunks.iter().filter_map(|h| {
        let (lo, hi) = match side {
            Side::Left => h.a_range,
            Side::Right => h.b_range,
        };
        if lo == 0 || hi == 0 { return None; }
        Some((h.id, (lo as f32 - 1.0) * lh, hi as f32 * lh))
    }).collect()
}
```

- [ ] **Step 3: Apply the target scroll via `state.pending_*_scroll`**

```rust
// After both panes render:
let l = state.last_left_scroll_y;
let r = state.last_right_scroll_y;
// ... detect change, compute target, write to state.pending_right_scroll
//     or state.pending_left_scroll for next frame.
```

The pending value will be consumed at the start of next frame's `render_pane` via `igSetNextWindowScroll` (already wired in Task 5).

- [ ] **Step 4: Echo dampening**

The existing `sync_scrolls` in the deleted `render.rs` had echo dampening (`state.written_left` / `written_right` + `ECHO_TOLERANCE`). Port that logic verbatim — without it, the two panes will oscillate.

- [ ] **Step 5: Manual smoke**

If GUI: scroll one pane via mouse wheel. Verify the OTHER pane scrolls to keep hunks aligned. Confirm no oscillation.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "diff_view: scroll sync between the two multiline widgets"
```

---

## Task 9: New behavior tests

Add tests covering the user-facing behaviors that were previously in `tests.rs` per-row tests. Get all green before deleting the old tests in Task 11.

**Files:**
- Modify: `src/app/diff_view/tests.rs`

The existing test file already has helpers (`imgui_lock`, `try_init_wgpu`, `run_frame_with_wgpu`, `FrameInput`, `SharedClipboard`, `TestClipboard`) — REUSE them. Each test below is self-contained; the "setup" abbreviations in the code blocks below mean: build a fresh `imgui::Context` + `imgui_wgpu::Renderer` + `DiffViewState` and a `SessionStore` with the named text, exactly as the existing tests in this file do (e.g. `headless_wgpu_double_click_selects_word`). Do not abbreviate any of the assertion logic.

- [ ] **Step 1: Test — drag-select + Ctrl+C**

```rust
#[test]
fn multiline_drag_then_ctrl_c_copies_multi_line() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let text = "alpha\nbeta\ngamma\n";
    let id = store.open_two_way(text, text, None).unwrap();
    // Use SharedClipboard from previous tests.
    let clipboard = SharedClipboard::default();
    let clip = clipboard.handle();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.set_clipboard_backend(clipboard);
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Click into the widget at the start of row 1, drag to row 3.
    // ImGui's multiline widget owns selection — driven via mouse events.
    for input in [
        FrameInput { mouse_pos: Some([80.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 100.0]), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 100.0]), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    // Ctrl+C
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, true);
    ctx.io_mut().add_key_event(imgui::Key::C, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::C, false);
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, false);

    let c = clip.lock().unwrap().clone();
    assert!(c.contains('\n'), "multi-line drag + Ctrl+C should write multi-line text; got {c:?}");
}
```

- [ ] **Step 2: Test — type a char replaces selection**

```rust
#[test]
fn multiline_select_then_type_replaces_selection() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let text = "alpha\nbeta\ngamma\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Drag-select rows 1..=2 in the left pane.
    for input in [
        FrameInput { mouse_pos: Some([80.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 70.0]), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 70.0]), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    // Type 'X'.
    ctx.io_mut().add_input_character('X');
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    assert!(
        a_text.len() < text.len() - 1,
        "selection should have been deleted; before={} after={}",
        text.len() - 1, a_text.len(),
    );
    assert!(a_text.contains('X'), "typed 'X' should be present; got {a_text:?}");
}
```

- [ ] **Step 3: Test — Enter at caret inserts newline**

```rust
#[test]
fn enter_at_caret_inserts_newline_in_session_text() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let text = "alpha\nbeta\n";  // 2 lines + trailing newline → 2 in storage
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Click in the middle of row 1 to set the caret.
    for input in [
        FrameInput { mouse_pos: Some([90.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    // Press Enter.
    ctx.io_mut().add_key_event(imgui::Key::Enter, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::Enter, false);

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    let line_count_before = text.trim_end_matches('\n').lines().count();
    let line_count_after = a_text.lines().count().max(1);
    assert_eq!(
        line_count_after, line_count_before + 1,
        "Enter should add one line; before={line_count_before} after={line_count_after} a_text={a_text:?}",
    );
}
```

- [ ] **Step 4: Test — Ctrl+V multi-line paste at caret**

```rust
#[test]
fn ctrl_v_multiline_at_caret_inserts_lines() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let text = "alpha\nbeta\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.set_clipboard_backend(TestClipboard { text: "foo\nbar".into() });
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Click into row 1.
    for input in [
        FrameInput { mouse_pos: Some([90.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    // Ctrl+V.
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, true);
    ctx.io_mut().add_key_event(imgui::Key::V, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::V, false);
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, false);

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    assert!(a_text.contains("foo"), "paste should insert 'foo'; got {a_text:?}");
    assert!(a_text.contains("bar"), "paste should insert 'bar'; got {a_text:?}");
    assert!(
        a_text.lines().count() >= 3,
        "paste of two lines should grow line count; got {} lines: {a_text:?}",
        a_text.lines().count(),
    );
}
```

- [ ] **Step 5: Test — Apply A→B**

```rust
#[test]
fn apply_a_to_b_button_splices_b_text() {
    // Need a session where A and B differ on a known hunk. Use simple
    // texts: A = "alpha\ndelta\n", B = "ALPHA\ndelta\n". The first
    // line is a change hunk; Apply A→B should make B's first line "alpha".
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let id = store.open_two_way("alpha\ndelta\n", "ALPHA\ndelta\n", None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();

    // Find the change hunk's id from the snapshot.
    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { hunks, .. } = &snap.mode else { panic!() };
    let change_hunk = hunks.iter().find(|h| {
        h.ops.iter().any(|op| matches!(op, DiffOp::Delete { .. } | DiffOp::Insert { .. }))
    }).expect("a change hunk should exist");
    let hunk_id = change_hunk.id;

    // Apply by directly queuing the edit (avoids needing to drive the
    // hover overlay panel mouse-precisely). This is the same edit the
    // button would queue.
    let mut pending_edits = vec![DiffEdit::ReplaceHunkSide {
        session_id: id,
        hunk_id,
        target: TwoWaySide::B,
        old_target_text: None,
    }];
    // Apply via the app-level path (the test harness's apply_edit
    // already handles ReplaceHunkSide).
    for e in pending_edits.drain(..) {
        apply_edit(&store, e);
    }
    let _ = view;
    let _ = ctx;
    let _ = renderer;

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { b_text, .. } = snap.mode else { panic!() };
    assert!(b_text.starts_with("alpha"), "B should now start with 'alpha'; got {b_text:?}");
}
```

- [ ] **Step 6: Test — ↕ jump-to-pair**

```rust
#[test]
fn move_jump_sets_pending_scroll_on_opposite_pane() {
    // Open a session where histogram detects a move. Manually set
    // state.pending_jump = Some(...) (the button would set this);
    // render a frame; assert the target pane's pending_*_scroll is
    // populated with a non-zero target.
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    let a = "hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n";
    let b = "hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n";
    let opts = DiffOptions { detect_moves: true, move_min_lines: 2, ..DiffOptions::default() };
    let id = store.open_two_way_with(
        a.trim_end_matches('\n').to_string(),
        b.trim_end_matches('\n').to_string(),
        true, true,
        Some("histogram".into()), opts,
    ).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Pre-render once so widgets initialize.
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    view.pending_jump = Some(PendingJump {
        session_id: id,
        pane: Side::Right,           // jump TO the right pane
        target_line: 3,              // line that contains "ftr1" on B
    });
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    // The consume_jump logic writes the centered scroll to the OPPOSITE
    // pane. With pane=Right as the destination, that pane's scroll should
    // be set; the source pane (Left) should not.
    assert!(view.pending_jump.is_none(), "jump should have been consumed");
    // Either pending_right_scroll is set OR last_right_scroll_y reflects
    // the new position. Accept either as a success signal:
    let scrolled = view.pending_right_scroll.is_some() || view.last_right_scroll_y > 0.0;
    assert!(scrolled, "right pane should have scrolled");
}
```

- [ ] **Step 7: Test — scroll sync**

```rust
#[test]
fn scrolling_one_pane_targets_the_other() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else { return; };
    let store = SessionStore::new();
    // Need enough content for scroll to be meaningful.
    let mut a = String::new();
    let mut b = String::new();
    for i in 1..=50 {
        a.push_str(&format!("line{i:02}\n"));
        b.push_str(&format!("line{i:02}\n"));
    }
    let id = store.open_two_way(&a, &b, None).unwrap();

    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Two warm-up frames.
    for _ in 0..2 {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    }
    // Scroll the left pane via pending_left_scroll.
    view.pending_left_scroll = Some(200.0);
    for _ in 0..2 {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    }
    // After sync runs, right pane should also have scrolled to match
    // (texts are identical so the target scroll equals the source's).
    assert!(
        view.last_right_scroll_y > 100.0,
        "right pane should follow left's scroll; got right_y={}",
        view.last_right_scroll_y,
    );
}
```

- [ ] **Step 8: Build + run all new tests**

Run: `cargo test --lib`
Expected: at least the 7 new tests PASS.

- [ ] **Step 9: Commit**

```bash
git add -u
git commit -m "diff_view: new behavior tests on the multiline UI (cover/replace deleted tests)"
```

---

## Task 10: Merge view — multiline base/local/remote

Apply the same machinery to `merge_view.rs`.

**Files:**
- Modify: `src/app/merge_view.rs` (extensive rewrite).

- [ ] **Step 1: Replace per-row rendering with three `input_text_multiline`s + child_window wrappers**

Pattern identical to `diff_view::render_pane`, generalized to three columns. Reuse `paint_row_overlays` (it already takes a `Side`, but merge view uses three panes — extend the enum or add a `Pane::Base|Local|Remote` enum and a parallel overlay variant).

- [ ] **Step 2: Edits emit `SetSide` with `SideRef::ThreeWay(...)`**

```rust
let side_ref = SideRef::ThreeWay(match pane {
    Pane::Base => ThreeWaySide::Base,
    Pane::Local => ThreeWaySide::Local,
    Pane::Remote => ThreeWaySide::Remote,
});
pending_edits.push(DiffEdit::SetSide { session_id, side: side_ref, new_text: buf.clone(), old_text: None });
```

- [ ] **Step 3: Delete `merge_view`'s old `draw_row` and `extract_selection_text`**

These are now subsumed by imgui's native behaviors + `paint_row_overlays`.

- [ ] **Step 4: Adapt existing merge_view tests**

Same pattern as diff_view: ignore tests that touch deleted internals, replace with behavior tests where appropriate.

- [ ] **Step 5: Build + tests**

Run: `cargo build && cargo test --lib`
Expected: PASS.

- [ ] **Step 6: Manual smoke**

If GUI: open a 3-way merge. Verify all three panes render, are editable. Type into local → result pane updates (via re-merge).

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "merge_view: rewrite on input_text_multiline; base/local/remote become editable"
```

---

## Task 11: Delete obsolete tests + dead helpers

After all behaviors have new tests covering them, sweep out the old.

**Files:**
- Modify: `src/app/diff_view/tests.rs` — delete every `#[ignore]`'d test (and the `// REMOVED` commented blocks).
- Modify: `src/app/undo_stack.rs` — delete `DiffEdit::SetTwoWayLine` and `DiffEdit::SpliceTwoWayLines` variants entirely (now that the UI no longer emits them).
- Modify: `src/session.rs` — delete `set_two_way_line`, `set_two_way_lines`, `splice_two_way_lines` (replaced by `set_side_text`).
- Modify: `src/app/mod.rs` — delete `do_copy`, `copy_enabled`, `PendingKey`, `inject_pending_key`, the post-build do_copy hook, the Ctrl+C and Ctrl+A handlers in `keyboard_shortcuts`. Adapt the Edit menu to disable Cut/Copy/Paste/Select All for the diff view (imgui owns them inside the widget).

- [ ] **Step 1: Delete ignored tests**

```bash
# Remove every block of code in tests.rs between #[ignore] and the
# closing brace of its #[test] function:
# (Manually edit since `sed` over braced regions is fragile.)
```

Or simpler: keep the file but delete each test by name. Use the list in the spec's "Tests deleted" section as the canonical list.

- [ ] **Step 2: Delete `DiffEdit::SetTwoWayLine` and `DiffEdit::SpliceTwoWayLines`**

In `src/app/undo_stack.rs`, remove the two variants from the enum, their match arms in `edit`/`undo`/`merge`. Cargo build will flag every remaining call site — there should be none (Task 5 deleted the UI emitters).

- [ ] **Step 3: Delete `session.rs` legacy methods**

`set_two_way_line`, `set_two_way_lines`, `splice_two_way_lines`. Cargo build will flag remaining callers (should be none).

- [ ] **Step 4: Delete `app/mod.rs` clipboard helpers**

`do_copy`, `copy_enabled`, `PendingKey`, `inject_pending_key`, the post-build do_copy hook, the Ctrl+C/Ctrl+A handlers. The Edit menu's Cut/Copy/Paste/Select All items become disabled or fall through to imgui-native (depending on focus).

- [ ] **Step 5: Build + tests**

Run: `cargo build && cargo test --lib`
Expected: PASS. The build is the gate: if anything still references deleted helpers, it surfaces here.

- [ ] **Step 6: Manual smoke**

If GUI: full regression — open, edit, copy, paste, cut, undo, save, all of it. Note any rough edges.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "cleanup: delete obsolete tests, legacy edit types, clipboard hooks"
```

---

## Verification Checklist

After all tasks complete:

- [ ] `cargo build --no-default-features` passes.
- [ ] `cargo build` passes (no new warnings outside `syntax.rs`).
- [ ] `cargo test --no-default-features --lib` passes.
- [ ] `cargo test --lib` passes (no `#[ignore]`'d tests remaining).
- [ ] Manual smoke list from the spec passes (drag, Ctrl+C/X/V/A, Enter, type-with-selection, Apply A→B, ↕ jump, scroll sync, anchor clicks, save/load round-trip).
- [ ] LOC delta is in the spec's predicted range (~-500..-1000 net).

---

## Rollback Plan

Each task is a single commit; if any task lands and turns out to be a regression, `git revert` on that commit puts the branch back to a working state without disturbing later commits. Task 5 is the riskiest (UI replacement) — if it fails manual smoke, revert Tasks 5+ and leave Tasks 1-4 (the storage refactor) intact, since those are net-positive on their own.
