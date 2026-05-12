# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Diffie is a native desktop code diffing & 3-way merge app written in Rust.

Native GUI built with `imgui-rs` + `wgpu` + `winit`. GUI deps are gated behind the `gui` Cargo feature, which is **on by default**. The core diff/merge/session library is engine-agnostic and remains testable without GUI deps.

## Commands

- `cargo test --no-default-features --lib` — fast core-only unit tests (no GUI deps).
- `cargo test --lib` — same tests with GUI deps compiled (slower).
- `cargo build` / `cargo run` — builds and launches the GUI app.
- `cargo build --no-default-features` — core library only.

## Architecture

### Core library (`src/`, crate `diffie_lib`)

- `diff/` — engine trait + implementations.
  - `diff::DiffEngine` trait (`name()`, `diff(a, b) -> Vec<DiffOp>`).
  - `basic.rs` — line-level Myers via the `diff` crate.
  - `anchored.rs` — engine that respects user-supplied `Anchor`s (forces line alignments).
  - `mod.rs` defines `DiffOp` (Equal/Delete/Insert with 1-based `LineNo`), `Hunk`, `Anchor`, and the `group_into_hunks` grouper that alternates equal/change runs with deterministic ids.
- `merge.rs` — 3-way merge. `MergeHunk` variants: `Stable`, `LocalOnly`, `RemoteOnly`, `Conflict`. `Resolution` (Local/Remote/Base/Custom) + `apply_resolutions` produces the merged text. `MergeAnchor { base, local, remote }` pins line triples.
- `session.rs` — `SessionStore` (Mutex<HashMap<SessionId, DiffSession>>) holds session state. `SessionMode::TwoWay` vs `ThreeWay` carry file lines, anchors, hunks, and a per-hunk decision/resolution map. `manual_result: Option<String>` overrides the computed merged buffer when the user edits the result pane. `available_engines()` lists registered engine names; `build_engine(name)` constructs by name.
- `io.rs` — `read_text` / `write_text` with UTF-8 fallback via `encoding_rs`.

### Cross-cutting conventions

- Line numbers are **1-based** `u32` (`LineNo`) on both sides.
- Hunk ids are stable u32s assigned at grouping time; UI layers key decisions/resolutions by these ids.
- Two-way state uses `HunkDecision` (AcceptA/AcceptB/Both/Neither/Custom/PerLine). Three-way state uses `Resolution` (Local/Remote/Base/Custom). Don't conflate the two enums.
- DTO enums serialize with `#[serde(tag = "kind", rename_all = "snake_case")]` (or `"lowercase"` for `DiffOp`) — preserved for potential state-persistence even though IPC is gone.

### GUI layer (`src/app/`)

Renderer: `wgpu` + `imgui-rs` + `winit`. File dialogs: `rfd`. Clipboard: `arboard`. Recents persisted via `dirs` to AppData. Syntax highlighting via `tree-sitter` (rust/cpp/c#/hlsl). Undo/redo via the `undo` crate.

- `mod.rs` — `App`/`AppState`, tabs, menu bar, keyboard shortcuts, font loading, GPU setup. Tabs are session-scoped (no session persistence).
- `diff_view.rs` — 2-way view. Files are **edited in place** (no result pane); bezier ribbons, hover overlay (Apply A/B), inline hunk buttons, center-anchored same-frame scroll sync via `SetNextWindowScroll`, multiline drag-selection with auto-scroll, syntax highlighting.
- `merge_view.rs` — 3-way view; uses `result_pane.rs` for the editable merged buffer (backed by `session.manual_result`).
- `char_diff.rs` — LCS character-level diff for paired delete/insert lines.
- `syntax.rs`, `theme.rs` — tree-sitter highlighting + color theme.
- `undo_stack.rs` — per-buffer undo/redo.
- `recents.rs` — recent-files list persisted to disk.

Note: 2-way no longer has a result pane (commit 25c0ead). Only 3-way uses `manual_result`.
