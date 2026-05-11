# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Diffie is a native desktop code diffing & 3-way merge app written in Rust.

**Stack migration in progress.** The original Tauri 2 + Svelte 5 frontend has been removed; the GUI is being rebuilt natively using `imgui-rs` rendered via `wgpu`. The core diff/merge/session logic (this entire repo today, minus the deleted UI shell) is the stable foundation the new UI layer will be wired onto.

## Commands

- `cargo test --lib` — run the core library's unit tests (no GUI dependencies).
- `cargo build` — builds the lib and the (currently stub) binary.
- `cargo run` — prints a placeholder message; the GUI is not yet wired.

Once the GUI is added, GUI deps will live behind a `gui` Cargo feature so `cargo test --no-default-features --lib` stays cheap to run.

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

### Planned GUI layer (not yet present)

Renderer: `wgpu` + `imgui-rs` + `winit`. Result editor: `imgui-text-edit-rs`. File dialogs: `rfd`. Tabs are session-scoped (no persistence). Hunk-control buttons stay inline within their change-hunk row range.

Module layout to come under `src/app/`: `mod.rs` (App state, tab list, status), `diff_view.rs`, `merge_view.rs`, `connector.rs` (bezier ribbons + anchor lines via `ImDrawList`), `result_pane.rs`, `layout.rs` (precomputed row-y for virtualization via `ImGuiListClipper`), `char_diff.rs` (LCS character-level diff), `scroll_sync.rs` (center-anchored, last-written echo guard), `file_dialog.rs`.
