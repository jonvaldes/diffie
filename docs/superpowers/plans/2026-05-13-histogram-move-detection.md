# Histogram Move Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect moved blocks of lines in the histogram diff engine and tag them in the op stream via a new `move_id` field, with detection performed by a generic post-pass that other engines can opt into later.

**Architecture:** Add `move_id: Option<u32>` to `DiffOp::Delete` and `DiffOp::Insert` (non-breaking via `serde(default, skip_serializing_if)`). Build a generic `detect_moves` post-pass in a new `src/diff/moves.rs` that collects maximal Delete/Insert runs, scores every pair by LCS-based similarity, and greedily assigns shared `move_id`s to runs scoring `>= 0.8`. Wire it into `recompute_two_way` in `session.rs` alongside the existing sub-line pass, gated on `opts.detect_moves && engine.supports_moves`. Histogram flips its capability bit; myers and patience keep theirs at `false`.

**Tech Stack:** Rust, `imara-diff` (histogram), `imgui-rs`+`wgpu`+`winit` (GUI, not touched here), `cargo test --no-default-features --lib` for fast core tests.

Reference spec: `docs/superpowers/specs/2026-05-13-histogram-move-detection-design.md`.

---

## File Structure

**Created:**
- `src/diff/moves.rs` — the post-pass: run collection, LCS, greedy pairing, stamping. All move-detection logic lives here. Self-contained, depends only on `super::{DiffOp, DiffOptions}`.

**Modified:**
- `src/diff/mod.rs` — add `move_id` field to `DiffOp::Delete` and `DiffOp::Insert`; declare `pub mod moves`; flip the histogram registry entry to `supports_moves: true`.
- `src/diff/anchored.rs` — propagate `move_id` in the destructure-and-rebuild at lines 120 and 123.
- `src/diff/histogram.rs` — `HistogramDiff::capabilities()` returns `supports_moves: true`. (No call to `detect_moves` here — the post-pass runs at the session layer alongside `populate_pair_spans`.)
- `src/diff/corpus_tests.rs` — split the capability assertion: histogram is allowed `supports_moves: true`, all others must remain `false`; add a check that ops from engines with `supports_moves == false` never carry a `move_id`.
- `src/session.rs` — capture engine capabilities before consuming `inner`, then call `moves::detect_moves(&mut ops, opts)` after `populate_pair_spans` when both `opts.detect_moves` and the engine's `supports_moves` are true.
- `src/app/diff_view/common.rs` — update the two explicit field destructures at lines 382 and 390 to tolerate the new field (add `..` or list `move_id`).

---

## Task 1: Add `move_id` field to DiffOp

**Files:**
- Modify: `src/diff/mod.rs:36-63`
- Modify: `src/diff/anchored.rs:118-128`
- Modify: `src/app/diff_view/common.rs:380-395`

- [ ] **Step 1: Extend DiffOp variants and constructors**

Replace the `DiffOp` enum and its `impl` block in `src/diff/mod.rs` (currently lines 36-63):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffOp {
    Equal {
        a: LineNo,
        b: LineNo,
        text: String,
    },
    Delete {
        a: LineNo,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spans: Option<Vec<SubSpan>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_id: Option<u32>,
    },
    Insert {
        b: LineNo,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spans: Option<Vec<SubSpan>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_id: Option<u32>,
    },
}

impl DiffOp {
    pub fn delete(a: LineNo, text: String) -> Self {
        DiffOp::Delete { a, text, spans: None, move_id: None }
    }
    pub fn insert(b: LineNo, text: String) -> Self {
        DiffOp::Insert { b, text, spans: None, move_id: None }
    }
}
```

- [ ] **Step 2: Fix anchored.rs propagation**

In `src/diff/anchored.rs`, around lines 118-128 the code destructures and rebuilds `Delete` and `Insert`. Replace those two arms with:

```rust
DiffOp::Delete { a, text, spans, move_id } => out.push(DiffOp::Delete {
    a: *a,
    text: text.clone(),
    spans: spans.clone(),
    move_id: *move_id,
}),
DiffOp::Insert { b, text, spans, move_id } => out.push(DiffOp::Insert {
    b: *b,
    text: text.clone(),
    spans: spans.clone(),
    move_id: *move_id,
}),
```

(Open `anchored.rs` to confirm the exact surrounding lines; only the two `DiffOp::Delete { ... }` and `DiffOp::Insert { ... }` arms in the `out.push(DiffOp::...)` block change.)

- [ ] **Step 3: Fix the diff_view/common.rs destructures**

In `src/app/diff_view/common.rs`, the two arms at lines 382 and 390 explicitly destructure all named fields:

```rust
DiffOp::Delete { a, text, spans } => Some((*a, text.as_str(), spans.as_ref())),
// ...
DiffOp::Insert { b, text, spans } => Some((*b, text.as_str(), spans.as_ref())),
```

Replace them with:

```rust
DiffOp::Delete { a, text, spans, .. } => Some((*a, text.as_str(), spans.as_ref())),
// ...
DiffOp::Insert { b, text, spans, .. } => Some((*b, text.as_str(), spans.as_ref())),
```

The trailing `..` is forward-compatible if more fields get added later.

- [ ] **Step 4: Compile-check the whole workspace**

Run: `cargo build --no-default-features`
Expected: success, no warnings introduced by these changes.

Run: `cargo build`
Expected: success. If any other call site destructures `Delete`/`Insert` with explicit named fields, the compiler will point at it — apply the same `..` fix.

- [ ] **Step 5: Run the existing test suite to confirm no regressions**

Run: `cargo test --no-default-features --lib`
Expected: PASS. The new field defaults to `None`, so existing tests that build `DiffOp` via the `delete`/`insert` constructors are unaffected.

- [ ] **Step 6: Commit**

```bash
git add src/diff/mod.rs src/diff/anchored.rs src/app/diff_view/common.rs
git commit -m "diff: add move_id field to Delete/Insert ops"
```

---

## Task 2: Build the `detect_moves` post-pass

**Files:**
- Create: `src/diff/moves.rs`
- Modify: `src/diff/mod.rs` (add `pub mod moves;` near the other module declarations at the top of the file)

- [ ] **Step 1: Declare the module**

In `src/diff/mod.rs`, in the `pub mod` block at the very top (currently lines 1-7), add `pub mod moves;` after `pub mod histogram;`. After the edit the block should read:

```rust
pub mod anchored;
pub mod histogram;
pub mod moves;
pub mod myers;
pub mod normalize;
pub mod patience;
pub mod similar_runner;
pub mod sub_line;
```

- [ ] **Step 2: Create `src/diff/moves.rs` with the function skeleton and a failing test**

Create the file with the public signature, the similarity threshold constant, and the test module containing the first test. Implementation body is `unimplemented!()` so the test fails for the right reason.

```rust
//! Move-detection post-pass. Scans a flat `Vec<DiffOp>` for maximal
//! contiguous Delete and Insert runs whose contents are sufficiently
//! similar, and stamps the matched runs with a shared `move_id`.
//!
//! The pass is engine-agnostic: it relies only on the op stream and the
//! `DiffOptions.detect_moves` / `move_min_lines` settings. Engines opt
//! in by setting `EngineCapabilities::supports_moves = true`; the
//! caller is responsible for honouring that flag before invoking
//! `detect_moves`.

use super::{DiffOp, DiffOptions};

/// Pairs of runs scoring at or above this similarity are accepted as
/// moves. Kept private so we can tune without churning a public API.
const SIMILARITY_THRESHOLD: f64 = 0.8;

/// Walk `ops` and stamp `move_id` onto Delete/Insert runs whose
/// contents are sufficiently similar (LCS-based, threshold 0.8).
///
/// No-op when `opts.detect_moves` is false. Does not reorder, insert,
/// or remove ops — only mutates `move_id` on existing variants.
pub fn detect_moves(ops: &mut Vec<DiffOp>, opts: &DiffOptions) {
    let _ = (ops, opts);
    unimplemented!("detect_moves not yet implemented");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffOp;

    fn opts(detect: bool, min: u32) -> DiffOptions {
        DiffOptions {
            detect_moves: detect,
            move_min_lines: min,
            ..DiffOptions::default()
        }
    }

    fn move_id_of(op: &DiffOp) -> Option<u32> {
        match op {
            DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => *move_id,
            _ => None,
        }
    }

    #[test]
    fn disabled_is_noop() {
        // Construct: Equal, Delete x3 (block "X"), Equal, Insert x3 (block "X")
        let mut ops = vec![
            DiffOp::Equal { a: 1, b: 1, text: "ctx".into() },
            DiffOp::delete(2, "x1".into()),
            DiffOp::delete(3, "x2".into()),
            DiffOp::delete(4, "x3".into()),
            DiffOp::Equal { a: 5, b: 2, text: "mid".into() },
            DiffOp::insert(3, "x1".into()),
            DiffOp::insert(4, "x2".into()),
            DiffOp::insert(5, "x3".into()),
        ];
        let before = ops.clone();
        detect_moves(&mut ops, &opts(false, 3));
        assert_eq!(ops, before, "disabled run must not mutate ops");
    }
}
```

- [ ] **Step 3: Run the new test, confirm it fails for the right reason**

Run: `cargo test --no-default-features --lib diff::moves::tests::disabled_is_noop`
Expected: FAIL with a panic message `detect_moves not yet implemented` (because the call site is reached).

This confirms the test is wired into the module and exercises the function under test.

- [ ] **Step 4: Implement the disabled-path early return**

In `src/diff/moves.rs`, replace the body of `detect_moves` with:

```rust
pub fn detect_moves(ops: &mut Vec<DiffOp>, opts: &DiffOptions) {
    if !opts.detect_moves {
        return;
    }
    detect_moves_impl(ops, opts);
}

fn detect_moves_impl(_ops: &mut Vec<DiffOp>, _opts: &DiffOptions) {
    unimplemented!("detect_moves_impl not yet written")
}
```

- [ ] **Step 5: Re-run the disabled test, confirm it now passes**

Run: `cargo test --no-default-features --lib diff::moves::tests::disabled_is_noop`
Expected: PASS.

- [ ] **Step 6: Add the run-collection helper and a unit test for it**

Append to `src/diff/moves.rs` (above the `#[cfg(test)] mod tests` block):

```rust
#[derive(Debug, Clone)]
struct Run {
    /// Indices into the `ops` slice (`start..=end`, inclusive). All ops
    /// in the range are the same variant (all Delete or all Insert).
    start: usize,
    end: usize,
}

impl Run {
    fn len(&self) -> usize { self.end - self.start + 1 }

    /// Borrow the line texts in this run from `ops` in order.
    fn lines<'a>(&self, ops: &'a [DiffOp]) -> Vec<&'a str> {
        (self.start..=self.end)
            .map(|i| match &ops[i] {
                DiffOp::Delete { text, .. } | DiffOp::Insert { text, .. } => text.as_str(),
                DiffOp::Equal { .. } => unreachable!("Run only spans Delete or Insert"),
            })
            .collect()
    }
}

/// Walk `ops` once and produce `(delete_runs, insert_runs)`, each a
/// list of maximal contiguous runs of the matching variant.
fn collect_runs(ops: &[DiffOp]) -> (Vec<Run>, Vec<Run>) {
    let mut dels = Vec::new();
    let mut inss = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        match &ops[i] {
            DiffOp::Delete { .. } => {
                let start = i;
                while i < ops.len() && matches!(ops[i], DiffOp::Delete { .. }) {
                    i += 1;
                }
                dels.push(Run { start, end: i - 1 });
            }
            DiffOp::Insert { .. } => {
                let start = i;
                while i < ops.len() && matches!(ops[i], DiffOp::Insert { .. }) {
                    i += 1;
                }
                inss.push(Run { start, end: i - 1 });
            }
            DiffOp::Equal { .. } => {
                i += 1;
            }
        }
    }
    (dels, inss)
}
```

Add this test inside the existing `mod tests`:

```rust
#[test]
fn collect_runs_splits_on_equal() {
    let ops = vec![
        DiffOp::Equal { a: 1, b: 1, text: "a".into() },
        DiffOp::delete(2, "d1".into()),
        DiffOp::delete(3, "d2".into()),
        DiffOp::insert(2, "i1".into()),
        DiffOp::Equal { a: 4, b: 3, text: "b".into() },
        DiffOp::insert(4, "i2".into()),
        DiffOp::insert(5, "i3".into()),
    ];
    let (dels, inss) = collect_runs(&ops);
    assert_eq!(dels.len(), 1);
    assert_eq!(dels[0].start, 1);
    assert_eq!(dels[0].end, 2);
    assert_eq!(inss.len(), 2);
    assert_eq!(inss[0].start, 3);
    assert_eq!(inss[0].end, 3);
    assert_eq!(inss[1].start, 5);
    assert_eq!(inss[1].end, 6);
}
```

- [ ] **Step 7: Run the new test, confirm it passes**

Run: `cargo test --no-default-features --lib diff::moves::tests::collect_runs_splits_on_equal`
Expected: PASS.

- [ ] **Step 8: Add an LCS helper and a unit test for it**

Append to `src/diff/moves.rs` above the `#[cfg(test)] mod tests`:

```rust
/// Length of the longest common subsequence of two slices of `&str`,
/// matching by equality. O(n·m) time, O(min(n, m)) extra space.
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let n = short.len();
    let mut prev = vec![0usize; n + 1];
    let mut cur = vec![0usize; n + 1];
    for i in 1..=long.len() {
        for j in 1..=n {
            if long[i - 1] == short[j - 1] {
                cur[j] = prev[j - 1] + 1;
            } else {
                cur[j] = cur[j - 1].max(prev[j]);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        for v in cur.iter_mut() { *v = 0; }
    }
    prev[n]
}

/// Similarity in [0, 1]: `2·lcs / (|a| + |b|)`. Returns 0 if both
/// inputs are empty.
fn similarity(a: &[&str], b: &[&str]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 0.0;
    }
    2.0 * lcs_len(a, b) as f64 / total as f64
}
```

Add tests inside `mod tests`:

```rust
#[test]
fn lcs_basic() {
    assert_eq!(lcs_len(&["a", "b", "c"], &["a", "b", "c"]), 3);
    assert_eq!(lcs_len(&["a", "b", "c"], &["a", "x", "c"]), 2);
    assert_eq!(lcs_len(&["a", "b", "c"], &["x", "y", "z"]), 0);
    assert_eq!(lcs_len(&[], &["a"]), 0);
}

#[test]
fn similarity_matches_spec_formula() {
    // 5 lines vs 5 lines, 4 in common → 2*4 / 10 = 0.8
    let a = ["l1", "l2", "l3", "l4", "l5"];
    let b = ["l1", "l2", "lX", "l4", "l5"];
    let sim = similarity(&a, &b);
    assert!((sim - 0.8).abs() < 1e-9, "got {sim}");
}
```

- [ ] **Step 9: Run the LCS tests, confirm they pass**

Run: `cargo test --no-default-features --lib diff::moves::tests::lcs`
Expected: both `lcs_basic` and `similarity_matches_spec_formula` PASS.

- [ ] **Step 10: Implement greedy pairing + stamping in `detect_moves_impl`**

Replace the `detect_moves_impl` body in `src/diff/moves.rs` with:

```rust
fn detect_moves_impl(ops: &mut Vec<DiffOp>, opts: &DiffOptions) {
    let min = opts.move_min_lines as usize;
    if min == 0 {
        return;
    }
    let (dels, inss) = collect_runs(ops);
    let dels: Vec<Run> = dels.into_iter().filter(|r| r.len() >= min).collect();
    let inss: Vec<Run> = inss.into_iter().filter(|r| r.len() >= min).collect();
    if dels.is_empty() || inss.is_empty() {
        return;
    }

    // Score every (delete_run, insert_run) pair.
    struct Candidate {
        d: usize,
        i: usize,
        sim: f64,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for (d_idx, d_run) in dels.iter().enumerate() {
        let d_lines = d_run.lines(ops);
        for (i_idx, i_run) in inss.iter().enumerate() {
            let i_lines = i_run.lines(ops);
            let sim = similarity(&d_lines, &i_lines);
            if sim >= SIMILARITY_THRESHOLD {
                candidates.push(Candidate { d: d_idx, i: i_idx, sim });
            }
        }
    }
    // Greedy: highest similarity first; ties broken by lower run-start
    // indices for determinism.
    candidates.sort_by(|x, y| {
        y.sim
            .partial_cmp(&x.sim)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| dels[x.d].start.cmp(&dels[y.d].start))
            .then_with(|| inss[x.i].start.cmp(&inss[y.i].start))
    });

    let mut d_claimed = vec![false; dels.len()];
    let mut i_claimed = vec![false; inss.len()];
    let mut next_id: u32 = 0;
    for c in candidates {
        if d_claimed[c.d] || i_claimed[c.i] {
            continue;
        }
        d_claimed[c.d] = true;
        i_claimed[c.i] = true;
        let id = next_id;
        next_id += 1;
        stamp(ops, &dels[c.d], id);
        stamp(ops, &inss[c.i], id);
    }
}

fn stamp(ops: &mut [DiffOp], run: &Run, id: u32) {
    for k in run.start..=run.end {
        match &mut ops[k] {
            DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => {
                *move_id = Some(id);
            }
            DiffOp::Equal { .. } => unreachable!("run only spans Delete or Insert"),
        }
    }
}
```

- [ ] **Step 11: Add the full behaviour test suite**

Add the following tests inside `mod tests` in `src/diff/moves.rs`. Each test owns its inputs; do not share helpers across tests beyond `opts` and `move_id_of` defined earlier.

```rust
#[test]
fn exact_block_move_is_tagged() {
    // Delete-run "x1 x2 x3 x4 x5" and Insert-run "x1 x2 x3 x4 x5".
    let mut ops = vec![
        DiffOp::Equal { a: 1, b: 1, text: "pre".into() },
        DiffOp::delete(2, "x1".into()),
        DiffOp::delete(3, "x2".into()),
        DiffOp::delete(4, "x3".into()),
        DiffOp::delete(5, "x4".into()),
        DiffOp::delete(6, "x5".into()),
        DiffOp::Equal { a: 7, b: 2, text: "mid".into() },
        DiffOp::insert(3, "x1".into()),
        DiffOp::insert(4, "x2".into()),
        DiffOp::insert(5, "x3".into()),
        DiffOp::insert(6, "x4".into()),
        DiffOp::insert(7, "x5".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    let id = move_id_of(&ops[1]).expect("delete should be tagged");
    for k in 1..=5 {
        assert_eq!(move_id_of(&ops[k]), Some(id), "delete[{k}]");
    }
    for k in 7..=11 {
        assert_eq!(move_id_of(&ops[k]), Some(id), "insert[{k}]");
    }
}

#[test]
fn one_internal_edit_at_threshold_is_accepted() {
    // 5 vs 5, 4 lines identical → sim = 0.8 exactly → accepted.
    let mut ops = vec![
        DiffOp::delete(1, "a".into()),
        DiffOp::delete(2, "b".into()),
        DiffOp::delete(3, "c".into()),
        DiffOp::delete(4, "d".into()),
        DiffOp::delete(5, "e".into()),
        DiffOp::Equal { a: 6, b: 1, text: "=".into() },
        DiffOp::insert(2, "a".into()),
        DiffOp::insert(3, "b".into()),
        DiffOp::insert(4, "X".into()), // edited
        DiffOp::insert(5, "d".into()),
        DiffOp::insert(6, "e".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    assert!(move_id_of(&ops[0]).is_some(), "delete-run should be tagged");
    assert!(move_id_of(&ops[8]).is_some(), "edited insert line still in the run carries the move_id");
}

#[test]
fn two_internal_edits_below_threshold_rejected() {
    // 5 vs 5, 3 lines identical → sim = 0.6 → rejected.
    let mut ops = vec![
        DiffOp::delete(1, "a".into()),
        DiffOp::delete(2, "b".into()),
        DiffOp::delete(3, "c".into()),
        DiffOp::delete(4, "d".into()),
        DiffOp::delete(5, "e".into()),
        DiffOp::Equal { a: 6, b: 1, text: "=".into() },
        DiffOp::insert(2, "a".into()),
        DiffOp::insert(3, "X".into()),
        DiffOp::insert(4, "c".into()),
        DiffOp::insert(5, "Y".into()),
        DiffOp::insert(6, "e".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    for op in &ops {
        assert_eq!(move_id_of(op), None, "no op should be tagged: {op:?}");
    }
}

#[test]
fn run_below_min_lines_ignored() {
    // Both runs are 2 lines; min_lines is 3 → no candidates.
    let mut ops = vec![
        DiffOp::delete(1, "a".into()),
        DiffOp::delete(2, "b".into()),
        DiffOp::Equal { a: 3, b: 1, text: "=".into() },
        DiffOp::insert(2, "a".into()),
        DiffOp::insert(3, "b".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    for op in &ops {
        assert_eq!(move_id_of(op), None);
    }
}

#[test]
fn greedy_picks_highest_similarity_pair() {
    // Two delete-runs A (sim 0.9 with I) and B (sim 0.85 with I), one insert-run I.
    // A wins; B stays unpaired.
    let mut ops = vec![
        // delete-run A: 5 lines, 4 will match I (sim 8/10 = 0.8). Need 0.9 so:
        // build A with 9 lines matching I (a 10-line run) → 2*9/(9+10) ≈ 0.947.
        // Simpler: A is identical to I (sim 1.0), B has 1 edit (sim 0.8).
        // 1.0 > 0.8, so A wins, B is below-or-equal threshold pair that survives
        // only if I is unclaimed — which it won't be.
        DiffOp::delete(1, "a1".into()),
        DiffOp::delete(2, "a2".into()),
        DiffOp::delete(3, "a3".into()),
        DiffOp::delete(4, "a4".into()),
        DiffOp::delete(5, "a5".into()),
        DiffOp::Equal { a: 6, b: 1, text: "=".into() },
        DiffOp::delete(7, "a1".into()),
        DiffOp::delete(8, "a2".into()),
        DiffOp::delete(9, "Z".into()), // makes B's similarity vs I = 0.8
        DiffOp::delete(10, "a4".into()),
        DiffOp::delete(11, "a5".into()),
        DiffOp::Equal { a: 12, b: 2, text: "==".into() },
        DiffOp::insert(3, "a1".into()),
        DiffOp::insert(4, "a2".into()),
        DiffOp::insert(5, "a3".into()),
        DiffOp::insert(6, "a4".into()),
        DiffOp::insert(7, "a5".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    // A (indices 0..=4) is paired with I (indices 12..=16).
    let id_a = move_id_of(&ops[0]).expect("A should be paired");
    assert_eq!(move_id_of(&ops[12]), Some(id_a));
    // B (indices 6..=10) is unpaired — I is already claimed.
    for k in 6..=10 {
        assert_eq!(move_id_of(&ops[k]), None, "B[{k}] should remain unpaired");
    }
}

#[test]
fn multiple_independent_moves_get_distinct_ids() {
    // Two clearly-separate moves; expect move_ids 0 and 1.
    let mut ops = vec![
        DiffOp::delete(1, "a1".into()),
        DiffOp::delete(2, "a2".into()),
        DiffOp::delete(3, "a3".into()),
        DiffOp::Equal { a: 4, b: 1, text: "=".into() },
        DiffOp::delete(5, "b1".into()),
        DiffOp::delete(6, "b2".into()),
        DiffOp::delete(7, "b3".into()),
        DiffOp::Equal { a: 8, b: 2, text: "==".into() },
        DiffOp::insert(3, "a1".into()),
        DiffOp::insert(4, "a2".into()),
        DiffOp::insert(5, "a3".into()),
        DiffOp::Equal { a: 9, b: 6, text: "===".into() },
        DiffOp::insert(7, "b1".into()),
        DiffOp::insert(8, "b2".into()),
        DiffOp::insert(9, "b3".into()),
    ];
    detect_moves(&mut ops, &opts(true, 3));
    let id_a = move_id_of(&ops[0]).expect("first move tagged");
    let id_b = move_id_of(&ops[4]).expect("second move tagged");
    assert_ne!(id_a, id_b, "distinct moves get distinct ids");
}
```

- [ ] **Step 12: Run the full moves test suite**

Run: `cargo test --no-default-features --lib diff::moves::tests`
Expected: all tests PASS.

If `greedy_picks_highest_similarity_pair` fails, double-check the lengths in run B — the comment in the test computes B vs I as 4/5 in common with run-lengths 5+5, giving similarity 0.8 (acceptable threshold), and A vs I is 5/5 in common giving 1.0. The sort puts A first; A claims I; B's only candidate is now unavailable.

- [ ] **Step 13: Commit**

```bash
git add src/diff/mod.rs src/diff/moves.rs
git commit -m "diff: add detect_moves post-pass (LCS-based, greedy pairing)"
```

---

## Task 3: Flip histogram capability bit and update corpus test

**Files:**
- Modify: `src/diff/histogram.rs:11-13`
- Modify: `src/diff/mod.rs` (the histogram registry entry inside `EngineRegistry::new`)
- Modify: `src/diff/corpus_tests.rs:156-167`

- [ ] **Step 1: Update the capability assertion in corpus_tests.rs to its new shape (failing first)**

Open `src/diff/corpus_tests.rs` and replace the body of `engines_with_supports_moves_false_emit_no_move_tag` (currently lines 157-167) with:

```rust
#[test]
fn capability_matrix_matches_expectations() {
    use std::collections::HashMap;
    let expected: HashMap<&str, bool> = [
        ("myers", false),
        ("patience", false),
        ("histogram", true),
    ]
    .into_iter()
    .collect();
    for (name, caps) in available_engines() {
        let want = *expected
            .get(name.as_str())
            .unwrap_or_else(|| panic!("unexpected engine in registry: {name}"));
        assert_eq!(
            caps,
            EngineCapabilities { supports_moves: want },
            "engine={name}"
        );
    }
}

#[test]
fn engines_without_move_capability_never_emit_move_ids() {
    use crate::diff::DiffOp;
    let a = ["alpha", "beta", "gamma", "delta"];
    let b = ["delta", "gamma", "beta", "alpha"];
    let opts = DiffOptions {
        detect_moves: true,
        move_min_lines: 1,
        ..DiffOptions::default()
    };
    for (name, caps) in available_engines() {
        if caps.supports_moves {
            continue;
        }
        let engine = build_engine(&name).expect("registered engine");
        let ops = engine.diff(&a, &b, &opts);
        for op in &ops {
            let mid = match op {
                DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => *move_id,
                _ => None,
            };
            assert_eq!(mid, None, "engine={name} emitted move_id={mid:?}");
        }
    }
}
```

If `available_engines` / `build_engine` / `DiffOptions` / `EngineCapabilities` are not already in scope at the top of the file, extend the existing `use` block accordingly (check the file's top imports — `available_engines`, `build_engine`, `DiffOptions`, `EngineCapabilities` are already used at line 6 in the existing test, so they should be in scope).

- [ ] **Step 2: Run the capability tests, confirm `capability_matrix_matches_expectations` fails**

Run: `cargo test --no-default-features --lib diff::corpus_tests::capability_matrix_matches_expectations`
Expected: FAIL — histogram still has `supports_moves: false`, mismatches expected `true`.

- [ ] **Step 3: Flip the histogram capability in the registry**

In `src/diff/mod.rs`, locate the `EngineRegistry::new` registry vec (currently lines 153-169). Change the histogram entry:

```rust
EngineEntry {
    name: "histogram",
    capabilities: EngineCapabilities { supports_moves: true },
    factory: || Box::new(histogram::HistogramDiff),
},
```

- [ ] **Step 4: Override `capabilities()` on `HistogramDiff`**

In `src/diff/histogram.rs`, extend the `impl DiffEngine for HistogramDiff` block (currently starting line 11):

```rust
impl DiffEngine for HistogramDiff {
    fn name(&self) -> &'static str { "histogram" }

    fn capabilities(&self) -> super::EngineCapabilities {
        super::EngineCapabilities { supports_moves: true }
    }

    fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp> {
        // existing body — do not change in this step
        if matches!(opts.whitespace, Whitespace::None) {
            run_histogram(a, b)
        } else {
            let a_norm = normalize_lines(a, opts.whitespace);
            let b_norm = normalize_lines(b, opts.whitespace);
            let a_refs: Vec<&str> = a_norm.iter().map(|s| s.as_str()).collect();
            let b_refs: Vec<&str> = b_norm.iter().map(|s| s.as_str()).collect();
            let ops_norm = run_histogram(&a_refs, &b_refs);
            map_text_back(&ops_norm, a, b)
        }
    }
}
```

(The `diff` body is unchanged from the file as it stands today; only the new `capabilities` method is added.)

- [ ] **Step 5: Run the capability tests, confirm both pass**

Run: `cargo test --no-default-features --lib diff::corpus_tests`
Expected: PASS. The second test, `engines_without_move_capability_never_emit_move_ids`, also passes because at this stage no engine wires `detect_moves` into its pipeline — every op still has `move_id: None`.

- [ ] **Step 6: Run the whole core suite to confirm no regressions**

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/diff/histogram.rs src/diff/mod.rs src/diff/corpus_tests.rs
git commit -m "diff: histogram advertises supports_moves capability"
```

---

## Task 4: Wire `detect_moves` into the session pipeline

**Files:**
- Modify: `src/session.rs:139-170` (`recompute_two_way`)

- [ ] **Step 1: Capture engine capabilities before consuming `inner`**

In `src/session.rs`, inside `recompute_two_way` (currently lines 139-170), replace the body starting at the `let inner = build_engine(...)` line with:

```rust
    let inner = build_engine(engine_name)?;
    let caps = inner.capabilities();
    let a = refs(a_lines);
    let b = refs(b_lines);
    let ops: Vec<DiffOp> = if anchors.is_empty() {
        inner.diff(&a, &b, opts)
    } else {
        struct DynEngine<'a>(&'a dyn DiffEngine);
        impl<'a> DiffEngine for DynEngine<'a> {
            fn name(&self) -> &'static str { "dyn" }
            fn capabilities(&self) -> crate::diff::EngineCapabilities {
                self.0.capabilities()
            }
            fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp> {
                self.0.diff(a, b, opts)
            }
        }
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

Only two lines change from the current state: the new `let caps = inner.capabilities();` near the top, and the new `if opts.detect_moves && caps.supports_moves { ... }` block right before `Ok(group_into_hunks(&ops))`. Everything else stays identical.

- [ ] **Step 2: Add an end-to-end integration test in moves.rs**

Append this test inside `mod tests` in `src/diff/moves.rs`:

```rust
#[test]
fn end_to_end_via_histogram_engine() {
    use crate::diff::{build_engine, DiffOp};

    // File A: header lines, then a 5-line block "blk1..blk5", then footer.
    // File B: header lines, footer first, then the 5-line block at the end.
    let a: Vec<&str> = vec![
        "hdr1", "hdr2",
        "blk1", "blk2", "blk3", "blk4", "blk5",
        "ftr1", "ftr2",
    ];
    let b: Vec<&str> = vec![
        "hdr1", "hdr2",
        "ftr1", "ftr2",
        "blk1", "blk2", "blk3", "blk4", "blk5",
    ];

    let engine = build_engine("histogram").expect("histogram registered");
    let opts = DiffOptions {
        detect_moves: true,
        move_min_lines: 3,
        ..DiffOptions::default()
    };
    let ops = engine.diff(&a, &b, &opts);

    // The histogram engine itself does NOT call detect_moves; this test
    // exercises the engine output directly and then runs the post-pass
    // manually, matching what session.rs does.
    let mut ops = ops;
    crate::diff::moves::detect_moves(&mut ops, &opts);

    // Expect at least one Delete with move_id == Some and at least one
    // Insert with the matching move_id.
    let del_id = ops.iter().find_map(|op| match op {
        DiffOp::Delete { move_id: Some(id), text, .. } if text.starts_with("blk") => Some(*id),
        _ => None,
    });
    let ins_id = ops.iter().find_map(|op| match op {
        DiffOp::Insert { move_id: Some(id), text, .. } if text.starts_with("blk") => Some(*id),
        _ => None,
    });
    assert!(del_id.is_some(), "expected a tagged delete: {ops:?}");
    assert_eq!(del_id, ins_id, "delete and insert should share move_id");
}
```

- [ ] **Step 3: Add a session-level integration test confirming the pipeline tags moves**

Append this test inside `mod tests` in `src/diff/moves.rs`:

```rust
#[test]
fn session_pipeline_tags_moves_when_engine_supports_them() {
    use crate::diff::DiffOp;
    use crate::session::{SessionMode, SessionStore};

    let a_text = "hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n";
    let b_text = "hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n";

    let store = SessionStore::new();
    let opts = DiffOptions {
        detect_moves: true,
        move_min_lines: 3,
        ..DiffOptions::default()
    };
    let id = store
        .open_two_way_with(a_text, b_text, Some("histogram".into()), opts)
        .expect("create session");
    let session = store.snapshot(id).expect("snapshot");
    let hunks = match &session.mode {
        SessionMode::TwoWay { hunks, .. } => hunks.clone(),
        _ => panic!("expected TwoWay session"),
    };
    let mut saw_move = false;
    for h in &hunks {
        for op in &h.ops {
            if let DiffOp::Delete { move_id: Some(_), .. }
            | DiffOp::Insert { move_id: Some(_), .. } = op
            {
                saw_move = true;
            }
        }
    }
    assert!(saw_move, "session pipeline should produce at least one move_id tag");
}
```

The test uses the actual public API on `SessionStore`: `open_two_way_with(a_text, b_text, engine, options)` returns a `SessionId`, and `snapshot(id)` returns a `DiffSession` whose `mode` is a `SessionMode::TwoWay { hunks, .. }`. Both are already declared `pub` in `src/session.rs` (see lines 233 and 300).

- [ ] **Step 4: Run the new tests**

Run: `cargo test --no-default-features --lib diff::moves::tests::end_to_end_via_histogram_engine`
Expected: PASS.

Run: `cargo test --no-default-features --lib diff::moves::tests::session_pipeline_tags_moves_when_engine_supports_them`
Expected: PASS.

- [ ] **Step 5: Run the full core suite to confirm no regressions**

Run: `cargo test --no-default-features --lib`
Expected: PASS.

- [ ] **Step 6: Build with GUI feature to confirm the full crate still compiles**

Run: `cargo build`
Expected: success.

- [ ] **Step 7: Commit**

```bash
git add src/session.rs src/diff/moves.rs
git commit -m "session: invoke detect_moves post-pass when engine supports it"
```

---

## Verification Checklist

After all tasks complete:

- [ ] `cargo test --no-default-features --lib` passes cleanly.
- [ ] `cargo build` succeeds.
- [ ] `available_engines()` reports `supports_moves: true` only for `histogram`.
- [ ] With `opts.detect_moves: false`, no op in any engine's output carries `move_id`.
- [ ] With `opts.detect_moves: true` and the histogram engine, a clearly-moved 5-line block is tagged on both sides with matching `move_id`s.
- [ ] No UI code reads `move_id` yet — rendering is a follow-up.
