# Histogram Move Detection — Design

Status: proposed
Date: 2026-05-13

## Goal

Detect moved blocks of lines in the histogram diff engine and tag them in
the op stream so that future UI work can render them distinctly. A "move"
is a contiguous block of lines that appears as both a Delete (in file A)
and an Insert (in file B), where the two blocks have nearly identical
content but the engine emitted them as unrelated change ops because of
their distant line positions.

The work in this spec adds detection only. UI rendering and merge
semantics for moves are deferred.

## Non-goals

- Move detection in `myers` or `patience`. The post-pass is built to be
  reusable by them later, but only `histogram` opts in here.
- Per-line pairing inside a moved block. A move is matched at run
  granularity. Lines that differ between the matched delete- and
  insert-runs remain ordinary Delete/Insert ops; they just carry the
  same `move_id` as their neighbours.
- 3-way merge semantics for moved hunks.
- Rendering changes in `diff_view.rs` / `merge_view.rs`.

## Data model

`DiffOp` gains an optional move identifier on the two change variants
(src/diff/mod.rs):

```rust
DiffOp::Delete {
    a: LineNo,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spans: Option<Vec<SubSpan>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_id: Option<u32>,
}

DiffOp::Insert {
    b: LineNo,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spans: Option<Vec<SubSpan>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_id: Option<u32>,
}
```

Constructor helpers `DiffOp::delete()` and `DiffOp::insert()` default
`move_id: None`. The `serde` attributes keep the field absent from
serialized output when unset, so existing JSON consumers and tests are
unaffected.

A `move_id` is an opaque pairing token: Delete ops and Insert ops sharing
the same `move_id` belong to the same moved block. Ids are minted from a
zero-based counter local to a single `diff()` call; they are not stable
across calls.

## Detection algorithm

A new module `src/diff/moves.rs` exposes:

```rust
pub fn detect_moves(ops: Vec<DiffOp>, opts: &DiffOptions) -> Vec<DiffOp>;
```

When `opts.detect_moves` is false the function returns `ops` unchanged.
Otherwise:

1. **Collect runs.** Walk `ops` once and produce two lists:
   - `delete_runs`: maximal contiguous slices of `DiffOp::Delete`.
   - `insert_runs`: maximal contiguous slices of `DiffOp::Insert`.

   Each run records its `(start_idx, end_idx)` in the op vector and the
   line texts in order. A run is *eligible* when its length is at least
   `opts.move_min_lines`. Ineligible runs are dropped.

2. **Score every (delete_run, insert_run) pair.** Similarity is
   LCS-based on raw line text:

   ```
   sim = 2 * lcs_len(d_lines, i_lines) / (|d_lines| + |i_lines|)
   ```

   Comparison uses the line text exactly as it appears in the op (no
   re-normalisation). The base diff has already absorbed whitespace
   handling per `opts.whitespace`, so the texts in change ops reflect
   the post-normalisation view.

3. **Greedy pairing.** Sort candidate pairs by `(sim desc, d_start asc,
   i_start asc)` for determinism. Accept a pair iff `sim >= 0.8` and
   neither run has already been claimed. The threshold is a const in
   `moves.rs`, not exposed via `DiffOptions` in v1.

4. **Stamp move ids.** For each accepted pair, mint a fresh `move_id`
   from a local counter and write it onto every Delete op in the
   delete-run and every Insert op in the insert-run. Lines that did not
   participate in the LCS still receive the `move_id` — they are the
   "internal edits" part of the move.

Returns the (possibly mutated) op vector.

### LCS

A standard O(n·m) dynamic-programming LCS over `&str` lines is
sufficient. Runs are bounded by `move_min_lines` from below and by the
size of a change region from above; in practice runs are short and the
quadratic table is fine. No early-out heuristics in v1.

### Greedy matching rationale

Optimal assignment (e.g. Hungarian) would maximise total similarity but
is overkill for a feature whose primary purpose is human-readable
annotation. Greedy descending-similarity matching is deterministic,
trivially testable, and matches what users intuitively expect ("the
best-matching pair wins").

## Engine integration

`src/diff/histogram.rs`:

- After producing base ops and running the existing sub-line refinement
  pass, call `moves::detect_moves(ops, opts)` when `opts.detect_moves`
  is true. Order matters: sub-line spans annotate ops by index, and
  `detect_moves` does not reorder or insert/remove ops, so the two
  passes compose cleanly.
- `HistogramDiff::capabilities()` returns `EngineCapabilities {
  supports_moves: true }`.

`src/diff/mod.rs`:

- Histogram's registry entry uses `EngineCapabilities { supports_moves:
  true }` so `available_engines()` reports the capability.

`myers` and `patience` keep `supports_moves: false` and do not invoke
the post-pass. Adopting them later is a one-line capability flip plus
the same `detect_moves` call.

## Tests

All tests live in `src/diff/moves.rs` as `#[cfg(test)] mod tests` and
run under `cargo test --no-default-features --lib`.

Unit tests for the post-pass (`detect_moves` called directly on
hand-built op vectors):

1. **Exact move.** Two files where a 5-line block has been relocated
   25 lines downward. Post-pass tags both runs with the same
   `move_id`; similarity is 1.0.
2. **Move with one internal edit.** 5-line block, 1 line modified
   in-place at the new location. Similarity = 0.8 exactly — accepted.
   Both runs carry the `move_id`, including the edited line.
3. **Move with two internal edits, rejected.** 5-line block, 2 lines
   modified. Similarity = 0.6 — rejected. No `move_id` is set on any
   op.
4. **Below threshold length.** A 2-line block moves, with
   `move_min_lines = 3`. Not considered.
5. **Disabled.** Same input as test 1 but `detect_moves: false`. No
   `move_id` set anywhere; the output op vector is unchanged.
6. **Greedy overlap.** Two delete-runs A and B and one insert-run I
   where `sim(A, I) = 0.9` and `sim(B, I) = 0.85`. Pairing picks
   (A, I); B remains unpaired even though it cleared the threshold.
7. **Multiple independent moves.** Two delete-runs and two insert-runs
   with two clear high-similarity pairings produce two distinct
   `move_id`s (0 and 1).

Integration tests for the histogram engine:

8. **End-to-end via histogram.** Construct two text inputs containing
   a moved block; run the histogram engine with `detect_moves: true`
   and assert that paired Delete/Insert ops in the returned vector
   share a `move_id`.
9. **Capability bit.** `available_engines()` reports
   `supports_moves: true` for `histogram` and `false` for `myers` and
   `patience`.

Existing `corpus_tests.rs` continues to pass:

- The current assertion at src/diff/corpus_tests.rs:157 is loosened so
  that `histogram` is allowed to advertise `supports_moves: true`;
  `myers` and `patience` are still required to advertise `false`. The
  intent of the test — engines whose capability bit is false must not
  emit move tags — is preserved as a separate assertion that walks the
  ops of every engine with `supports_moves == false` and confirms no
  Delete/Insert carries a `move_id`.

## Risks and open questions

- **Threshold tuning.** 0.8 is a guess. If it produces too many false
  positives in real-world diffs the constant can be raised; if it
  misses obvious moves it can be lowered. Threshold is intentionally
  not in `DiffOptions` so we do not lock in an API surface before we
  have feedback.
- **Run granularity.** A long Delete run may contain two physically
  separate moves stuck together by the base engine. v1 will not split
  them. If this turns out to matter, a follow-up can add sub-run
  matching.
- **Performance.** LCS is O(n·m) per pair, and we score every
  (delete_run, insert_run) pair. For pathological inputs with many
  small change regions this is quadratic in the number of runs times
  quadratic in run length. In practice typical diffs have a handful of
  change regions and short runs; if a real-world corpus hits a slow
  case, add a length-difference prefilter.

## Out of scope (follow-ups)

- UI affordance in `diff_view.rs` to render moved hunks (colour, badge,
  jump-to-pair shortcut).
- Merge-view semantics for moves in 3-way mode.
- Extending move detection to `myers` and `patience`.
- Exposing `move_min_lines` and the similarity threshold in the
  Preferences dialog.
