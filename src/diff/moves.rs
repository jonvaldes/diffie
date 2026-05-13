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
    if !opts.detect_moves {
        return;
    }
    detect_moves_impl(ops, opts);
}

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

    #[test]
    fn lcs_basic() {
        assert_eq!(lcs_len(&["a", "b", "c"], &["a", "b", "c"]), 3);
        assert_eq!(lcs_len(&["a", "b", "c"], &["a", "x", "c"]), 2);
        assert_eq!(lcs_len(&["a", "b", "c"], &["x", "y", "z"]), 0);
        assert_eq!(lcs_len(&[], &["a"]), 0);
    }

    #[test]
    fn similarity_matches_spec_formula() {
        let a = ["l1", "l2", "l3", "l4", "l5"];
        let b = ["l1", "l2", "lX", "l4", "l5"];
        let sim = similarity(&a, &b);
        assert!((sim - 0.8).abs() < 1e-9, "got {sim}");
    }

    #[test]
    fn exact_block_move_is_tagged() {
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
        let mut ops = vec![
            DiffOp::delete(1, "a".into()),
            DiffOp::delete(2, "b".into()),
            DiffOp::delete(3, "c".into()),
            DiffOp::delete(4, "d".into()),
            DiffOp::delete(5, "e".into()),
            DiffOp::Equal { a: 6, b: 1, text: "=".into() },
            DiffOp::insert(2, "a".into()),
            DiffOp::insert(3, "b".into()),
            DiffOp::insert(4, "X".into()),
            DiffOp::insert(5, "d".into()),
            DiffOp::insert(6, "e".into()),
        ];
        detect_moves(&mut ops, &opts(true, 3));
        assert!(move_id_of(&ops[0]).is_some(), "delete-run should be tagged");
        assert!(move_id_of(&ops[8]).is_some(), "edited insert line still in the run carries the move_id");
    }

    #[test]
    fn two_internal_edits_below_threshold_rejected() {
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
        let mut ops = vec![
            DiffOp::delete(1, "a1".into()),
            DiffOp::delete(2, "a2".into()),
            DiffOp::delete(3, "a3".into()),
            DiffOp::delete(4, "a4".into()),
            DiffOp::delete(5, "a5".into()),
            DiffOp::Equal { a: 6, b: 1, text: "=".into() },
            DiffOp::delete(7, "a1".into()),
            DiffOp::delete(8, "a2".into()),
            DiffOp::delete(9, "Z".into()),
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
        let id_a = move_id_of(&ops[0]).expect("A should be paired");
        assert_eq!(move_id_of(&ops[12]), Some(id_a));
        for k in 6..=10 {
            assert_eq!(move_id_of(&ops[k]), None, "B[{k}] should remain unpaired");
        }
    }

    #[test]
    fn end_to_end_via_histogram_engine() {
        use crate::diff::{build_engine, DiffOp};

        // Histogram aligns the blk block as Equal (it finds the best anchors),
        // so ftr1/ftr2 appear as a Delete/Insert pair representing the move.
        // Use move_min_lines: 2 so the two-line footer run qualifies.
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
            move_min_lines: 2,
            ..DiffOptions::default()
        };
        let ops = engine.diff(&a, &b, &opts);

        // The histogram engine itself does NOT call detect_moves; this test
        // exercises the engine output directly and then runs the post-pass
        // manually, matching what session.rs does.
        let mut ops = ops;
        crate::diff::moves::detect_moves(&mut ops, &opts);

        let del_id = ops.iter().find_map(|op| match op {
            DiffOp::Delete { move_id: Some(id), text, .. } if text.starts_with("ftr") => Some(*id),
            _ => None,
        });
        let ins_id = ops.iter().find_map(|op| match op {
            DiffOp::Insert { move_id: Some(id), text, .. } if text.starts_with("ftr") => Some(*id),
            _ => None,
        });
        assert!(del_id.is_some(), "expected a tagged delete: {ops:?}");
        assert_eq!(del_id, ins_id, "delete and insert should share move_id");
    }

    #[test]
    fn session_pipeline_tags_moves_when_engine_supports_them() {
        use crate::diff::DiffOp;
        use crate::session::{SessionMode, SessionStore};

        let a_text = "hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n";
        let b_text = "hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n";

        let store = SessionStore::new();
        let opts = DiffOptions {
            detect_moves: true,
            move_min_lines: 2,
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

    #[test]
    fn multiple_independent_moves_get_distinct_ids() {
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
}
