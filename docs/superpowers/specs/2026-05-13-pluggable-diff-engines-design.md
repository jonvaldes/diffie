# Pluggable diff engines & diff options

**Date:** 2026-05-13
**Status:** Draft for review

## Summary

Diffie currently exposes one line-level Myers engine (`basic`) plus an `anchored` wrapper. This spec replaces that with a registry of named engines (Myers, Patience, Histogram) and adds three orthogonal options that apply to every engine: whitespace normalization, sub-line granularity refinement, and move detection (plumbed now, dormant until an engine supports it).

The hunk/anchor/merge data model stays line-based. All new behavior is additive to `DiffOptions` and the `DiffEngine` trait.

## Goals

- Multiple diff algorithms selectable per tab.
- Whitespace insensitivity options (ignore-all / ignore-leading / ignore-trailing+EOL).
- Sub-line highlight granularity (word / char / grapheme), as a visual refinement only.
- Capability-gated move-detection toggle, with plumbing in place for future engines.
- Per-tab settings with a global default in a Preferences dialog.

## Non-goals

- Changing the atom of the merge model. Hunks, anchors, and resolutions remain line-keyed.
- Implementing native move detection in the initial engines — only the capability flag and UI plumbing land now.
- Token-mode or full-file char diff as a separate view.

## Architecture

### Engine registry

A `OnceLock<EngineRegistry>` populated at startup. Engines are registered by name and constructed on demand by `build_engine(name)`. The session stores only the engine *name* plus a `DiffOptions` struct; engines themselves are stateless.

```rust
pub struct EngineCapabilities {
    pub supports_moves: bool,
}

pub trait DiffEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> EngineCapabilities;
    fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp>;
}

pub fn available_engines() -> Vec<(String, EngineCapabilities)>;
pub fn build_engine(name: &str) -> Result<Box<dyn DiffEngine>, SessionError>;
```

Initial engines:

| Name        | Crate          | `supports_moves` |
|-------------|----------------|------------------|
| `myers`     | `similar`      | false            |
| `patience`  | `similar`      | false            |
| `histogram` | `imara-diff`   | false            |
| `anchored`  | wraps an inner engine name | inherits |

`basic` is removed once `myers` lands; both use the same algorithm and `myers` (via `similar`) is the canonical replacement. Any external callers passing `"basic"` are migrated.

### Diff options

```rust
pub struct DiffOptions {
    pub whitespace: Whitespace,
    pub sub_line: SubLineGranularity,
    pub detect_moves: bool,
    pub move_min_lines: u32,
}

pub enum Whitespace { None, IgnoreAll, IgnoreLeading, IgnoreTrailingEol }
pub enum SubLineGranularity { None, Word, Char, Grapheme }
```

Defaults: `whitespace = None`, `sub_line = None`, `detect_moves = false`, `move_min_lines = 3`.

### Whitespace normalization

Applied as a pre-pass inside each engine adapter, in a shared `diff/normalize.rs` module:

- `IgnoreAll` — strip all whitespace from each line.
- `IgnoreLeading` — `trim_start`.
- `IgnoreTrailingEol` — `trim_end` then normalize CRLF → LF.

Engines diff the normalized lines but emit `DiffOp`s carrying the **original** line text, so the renderer never sees the normalized form. Hunk grouping is unchanged.

### Sub-line granularity

Granularity affects only the post-pass that highlights spans inside paired delete/insert lines. The current `src/app/char_diff.rs` is promoted to `src/diff/sub_line.rs` (core library, GUI-independent) and generalized:

- `None` — no spans computed.
- `Word`, `Char`, `Grapheme` — use `similar::TextDiff::from_words` / `from_chars` / `from_graphemes` on each paired delete/insert pair.

`DiffOp::Delete` and `DiffOp::Insert` gain an optional `spans: Option<Vec<SubSpan>>` field, populated only when granularity ≠ `None` and the op is paired with its counterpart in the same change run. `SubSpan` records `(start_byte, end_byte, kind)` where `kind` is `Same | Changed`.

The renderer reads `spans` when present and falls back to flat coloring otherwise. Tests built against `DiffOp` patterns that don't construct the field still compile (it defaults to `None`).

### Move detection (plumbing only)

- `EngineCapabilities::supports_moves` declared per engine.
- `DiffOptions::detect_moves` and `move_min_lines` carried through recompute paths.
- UI toggle is disabled with an explanatory tooltip when the active engine reports `!supports_moves`.
- No engine reports `true` in this milestone; behavior is identical regardless of the toggle value.

This isolates future move-detection work to a single engine implementation (or a post-pass behind a new engine wrapper) without re-touching options, UI, or session APIs.

## Session integration

`DiffSession` gains:

```rust
pub options: DiffOptions,
```

`recompute_two_way` and `recompute_three_way` take `&DiffOptions` instead of just an engine name. New `SessionStore` setters:

- `set_engine(session_id, name)`
- `set_options(session_id, opts)`

Both trigger a full recompute. Per-hunk decisions/resolutions are keyed by hunk id and are invalidated on recompute (same behavior as today's engine change and anchor edits).

## UI

### Per-tab toolbar

A new toolbar row above each diff/merge tab's panes:

- **Engine** dropdown — lists registry names; reflects `session.engine`.
- **Whitespace** dropdown — None / Ignore all / Ignore leading / Ignore trailing+EOL.
- **Granularity** dropdown — None / Word / Char / Grapheme.
- **Moves** checkbox — enabled iff the active engine reports `supports_moves`. Tooltip when disabled: "This engine does not support move detection."

Changing any control calls the corresponding setter on `SessionStore`, which recomputes via the existing anchor-edit code path.

### Preferences dialog

A new modal dialog opened from the menu bar. Edits a single struct:

```rust
pub struct AppPreferences {
    pub default_engine: String,
    pub default_options: DiffOptions,
}
```

New tabs read these defaults when constructing their `DiffSession`. Existing tabs are unaffected.

### Persistence

A new `settings.json` file in the user's app-data directory (via `dirs`), separate from the existing recents file. Loaded once at startup; written on Preferences-dialog OK. JSON-serde of `AppPreferences`. Missing or invalid file falls back to built-in defaults silently.

## Testing

Core-only (`cargo test --no-default-features --lib`):

- **Shared engine corpus** — each engine runs against the same inputs: identity, pure insert, pure delete, single-line edit, reorder of two blocks, whitespace-only change, mixed change. Assertions are engine-specific where algorithms legitimately diverge, but every engine must satisfy: equal sums = input length, line numbers monotonically increase, every line appears exactly once on its side.
- **Whitespace normalization** — table-driven tests for each `Whitespace` mode, independent of engine.
- **Sub-line spans** — paired del/ins pairs at Word / Char / Grapheme granularities, including Unicode (combining marks, CJK).
- **Capability matrix** — assert each engine's `supports_moves` matches its emitted ops (no `Moved` tags from engines that declare `false`).
- **Anchored wrapper** — verify it composes with each base engine and that anchors still force alignment under whitespace normalization.

GUI smoke (`cargo test --lib`): toolbar dropdown wiring round-trips through `SessionStore` setters.

## Migration & compatibility

- `basic` engine name is removed. Callers in tests and session construction are updated to `myers`. There is no on-disk session state to migrate (sessions are tab-scoped and not persisted).
- `DiffOp::Delete`/`Insert` gain an optional field; pattern-match sites that use `..` or named fields keep compiling. Sites that exhaustively destructure are updated.
- `DiffEngine::diff` signature changes (adds `opts`); the only in-tree implementations are `basic.rs` (deleted) and `anchored.rs` (updated).

## Phasing

Implementation plan will break into phases — phase boundaries are commit-sized and each leaves the tree green:

1. Introduce `DiffOptions`, `EngineCapabilities`, new trait signature. Port `basic` → `myers` via `similar`. Delete `basic.rs`. All existing tests pass with default options.
2. Add `patience` and `histogram` engines + shared engine corpus tests.
3. Add `Whitespace` normalization module + tests; wire through engines.
4. Promote `char_diff.rs` → `diff/sub_line.rs`, generalize to `SubLineGranularity`, add `spans` field, update renderer.
5. Add `supports_moves` capability + `detect_moves` plumbing + UI toggle (dormant).
6. Add per-tab toolbar UI for engine/whitespace/granularity/moves; wire to `SessionStore`.
7. Add Preferences dialog + `settings.json` persistence.

## Open questions

None at spec time.
