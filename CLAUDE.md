# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Diffie is a desktop code diffing & 3-way merge app. Rust backend (Tauri 2) + Svelte 5 / TypeScript / Vite frontend / CodeMirror 6.

## Commands

All commands are wrapped by `./run.sh` at the repo root:

- `./run.sh` (or `--dev`) — `cargo tauri dev --features desktop`. Auto-installs frontend deps and `tauri-cli` if missing. Vite serves on port 1420 (strict).
- `./run.sh --release` — `cargo tauri build --features desktop`.
- `./run.sh --test` — backend unit tests: `cargo test --no-default-features --lib` (run from `src-tauri/`). The `--no-default-features` flag is required so tests build without webkit2gtk/Tauri native deps.

Direct invocations (when not using `run.sh`):

- Single backend test: `cd src-tauri && cargo test --no-default-features --lib <test_name>`
- Frontend type-check: `cd src && npm run check` (svelte-check)
- Frontend dev server alone: `cd src && npm run dev`

## Architecture

### Workspace layout

Cargo workspace at root with one member, `src-tauri/`. Frontend lives in `src/` (note: this is the npm/Vite project root, **not** Rust source — Rust sources are under `src-tauri/src/`).

### Feature-gated desktop runtime

The Rust crate (`diffie_lib`) compiles the core diff/merge/session logic without Tauri. The `desktop` feature pulls in `tauri`, `tauri-build`, and the dialog/fs plugins, plus the `commands` module. This split exists so `cargo test --no-default-features` works without native GUI deps — keep new core logic out of `commands.rs` and behind no feature gate.

`src-tauri/src/main.rs` prints an error if built without `desktop`; the real entry is `commands::run()`.

### Backend module structure (`src-tauri/src/`)

- `diff/` — engine trait + implementations.
  - `diff::DiffEngine` trait (`name()`, `diff(a, b) -> Vec<DiffOp>`).
  - `basic.rs` — line-level Myers via the `diff` crate.
  - `anchored.rs` — engine that respects user-supplied `Anchor`s (forces line alignments).
  - `mod.rs` defines `DiffOp` (Equal/Delete/Insert with 1-based `LineNo`), `Hunk`, `Anchor`, and the `group_into_hunks` grouper that alternates equal/change runs with deterministic ids.
- `merge.rs` — 3-way merge. `MergeHunk` variants: `Stable`, `LocalOnly`, `RemoteOnly`, `Conflict`. `Resolution` (Local/Remote/Base/Custom) + `apply_resolutions` produces the merged text. `MergeAnchor { base, local, remote }` pins line triples.
- `session.rs` — `SessionStore` (Mutex<HashMap<SessionId, DiffSession>>) holds session state across IPC calls. `SessionMode::TwoWay` vs `ThreeWay` carry the file lines, anchors, hunks, and a per-hunk decision/resolution map. `manual_result: Option<String>` overrides the computed merged buffer when the user edits the result pane. `available_engines()` lists registered engine names; `build_engine(name)` constructs by name.
- `commands.rs` (`desktop`-only) — thin Tauri command wrappers around `SessionStore`. Each command returns a `SessionView` DTO (separate from the internal `SessionMode` so serialization stays decoupled from storage). `run()` registers the invoke handler and manages the `SessionStore` as Tauri state.
- `io.rs` — `read_text` / `write_text` with UTF-8 fallback via `encoding_rs`.

### Frontend structure (`src/src/`)

- `App.svelte` is mounted via Svelte 5 `mount()` in `main.ts`.
- `lib/tauri.ts` — single `api` object that wraps every `invoke<T>(...)` call. **All backend IPC goes through here**; components should not call `invoke` directly. Argument names are `camelCase` (Tauri converts to `snake_case` Rust params).
- `lib/types.ts` — hand-written TS mirrors of the Rust DTOs (`DiffOp`, `Hunk`, `MergeHunk`, `Resolution`, `HunkDecision`, `SessionView` tagged union by `mode`). If these grow, switch to ts-rs/specta codegen — keep them in sync when changing Rust DTOs.
- `lib/stores.ts` — Svelte writable stores for the current `SessionView`, engine list, status text, and a `pendingAnchor` queue used by the two-click anchor-creation UX.
- `components/` — `DiffView` (2-way), `MergeView` (3-way), `ResultEditor`, `AnchorBar`, `HunkControls`, `FilePicker`.

### Cross-cutting conventions

- Line numbers are **1-based** `u32` (`LineNo`) on both sides.
- Hunk ids are stable u32s assigned at grouping time; the frontend keys decisions/resolutions by these ids.
- Two-way state uses `HunkDecision` (AcceptA/AcceptB/Both/Neither/Custom/PerLine). Three-way state uses `Resolution` (Local/Remote/Base/Custom). Don't conflate the two enums.
- DTO enums serialize with `#[serde(tag = "kind", rename_all = "snake_case")]` (or `"lowercase"` for `DiffOp`) — match this exactly when adding new variants on either side.
- `SessionView` is re-serialized fresh after each mutating command; the frontend treats it as immutable snapshots, not deltas.

## Notes

- Tauri config: `src-tauri/tauri.conf.json` — `frontendDist: "src/dist"`, `devUrl: "http://localhost:1420"`, `beforeDevCommand: "npm --prefix src run dev"`. Don't change the port without updating `vite.config.ts` (`strictPort: true`).
- No frontend test runner is configured; `npm run check` is the only frontend verification step.
