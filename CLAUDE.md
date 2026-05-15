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

- `diff/` — engine trait, registry, and implementations.
  - `diff::DiffEngine` trait: `name()`, `capabilities() -> EngineCapabilities`, `diff(a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp>`.
  - Engines: `myers.rs`, `patience.rs`, `histogram.rs` (histogram is the only one with `supports_moves: true`). `anchored.rs` wraps any inner engine to honor user-supplied `Anchor`s.
  - `registry()` / `available_engines()` / `build_engine(name)` — name-based factory. 2-way recompute uses dyn dispatch via the registry; 3-way recompute hard-codes a `match` on engine name because `ThreeWayMerge<E>` is generic over a concrete engine type.
  - `normalize.rs` (whitespace normalization), `sub_line.rs` (intra-line refinement spans), `moves.rs` (move detection), `similar_runner.rs` (adapter for the `similar` crate).
  - `mod.rs` defines `DiffOp` (`Equal { a, b, text }`, `Delete { a, text, spans, move_id }`, `Insert { b, text, spans, move_id }` — `spans`/`move_id` are optional refinement metadata), `Hunk`, `Anchor`, `DiffOptions { whitespace, sub_line, detect_moves, move_min_lines }`, and `group_into_hunks` (alternating equal/change runs, deterministic ids).
- `merge.rs` — 3-way merge. `MergeHunk` variants: `Stable`, `LocalOnly`, `RemoteOnly`, `Conflict`. `Resolution` (Local/Remote/Base/Custom) + `apply_resolutions` produces the merged text. `MergeAnchor { base, local, remote }` pins line triples.
- `session.rs` — `SessionStore` (Mutex<HashMap<SessionId, DiffSession>>) holds session state.
  - `SessionMode::TwoWay` / `ThreeWay` carry **`String` per side plus `*_trailing_newline: bool`** (not pre-split lines), anchors, hunks, and per-hunk decisions/resolutions.
  - `SideRef` (TwoWay(A|B) / ThreeWay(Base|Local|Remote)) + `set_side_text(id, side, text)` is the unified entry point for UI edits; it rewrites the side and recomputes hunks in one call.
  - `manual_result: Option<String>` overrides the computed merged buffer when the user edits the result pane (3-way only).
  - `recompute_two_way` runs `split_trivial_equals` on the ops afterward: any whitespace-only `Equal` is forced into paired `Delete`+`Insert`. This is intentional — distant blank-line matches were dragging connector ribbons across huge vertical distances. Preserve this behavior.
  - Default 2-way decision for change hunks is `AcceptB` (equal hunks always emit their text).
- `io.rs` — `read_text(path) -> TextRead { text, trailing_newline }` (UTF-8 fallback via `encoding_rs`) and `write_text`.

### Cross-cutting conventions

- Line numbers are **1-based** `u32` (`LineNo`) on both sides.
- Hunk ids are stable u32s assigned at grouping time; UI layers key decisions/resolutions by these ids.
- Two-way state uses `HunkDecision` (AcceptA/AcceptB/Both/Neither/Custom/PerLine). Three-way state uses `Resolution` (Local/Remote/Base/Custom). Don't conflate the two enums.
- DTO enums serialize with `#[serde(tag = "kind", rename_all = "snake_case")]` (or `"lowercase"` for `DiffOp`) — preserved for potential state-persistence even though IPC is gone.
- All UI text edits should go through `SessionStore::set_side_text` so the recompute path stays uniform.

### GUI layer (`src/app/`)

Renderer: `wgpu` + `imgui-rs` + `winit`. File dialogs: `rfd`. Clipboard: `arboard`. Recents persisted via `dirs` to AppData. Syntax highlighting via `tree-sitter` (rust/cpp/c#/hlsl). Undo/redo via the `undo` crate.

- `mod.rs` — `App`/`AppState`, tabs, menu bar, keyboard shortcuts, font loading, GPU setup. Tabs are session-scoped (no session persistence).
- `diff_view/` — 2-way view (module dir: `mod.rs`, `common.rs`, `overlay.rs`, `tests.rs`). Files are **edited in place** (no result pane); bezier ribbons, hover overlay (Apply A/B), inline hunk buttons, center-anchored same-frame scroll sync via `SetNextWindowScroll`, multiline drag-selection with auto-scroll. Owns its scroll tracking via `mouse_wheel` + `SetNextWindowScroll` (commit e5bfa24); text + caret are painted directly on the draw list (commit 3dd7ff4).
- `merge_view.rs` — 3-way view; uses `result_pane.rs` for the editable merged buffer (backed by `session.manual_result`).
- `engine_bar.rs` — engine picker + diff option toggles.
- `preferences.rs` — persisted user preferences.
- `char_diff.rs` — LCS character-level diff for paired delete/insert lines.
- `syntax.rs`, `theme.rs` — tree-sitter highlighting + color theme (rainbow brackets share a depth palette, commit 7e44e3b).
- `undo_stack.rs` — per-buffer undo/redo; `DiffEdit::SetSide` coalesces same-side edits.
- `recents.rs` — recent-files list persisted to disk.

Note: 2-way no longer has a result pane (commit 25c0ead). Only 3-way uses `manual_result`.
