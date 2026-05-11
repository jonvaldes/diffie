pub mod basic;
pub mod anchored;

use serde::{Deserialize, Serialize};

pub type LineNo = u32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffOp {
    Equal { a: LineNo, b: LineNo, text: String },
    Delete { a: LineNo, text: String },
    Insert { b: LineNo, text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hunk {
    pub id: u32,
    pub a_range: (LineNo, LineNo),
    pub b_range: (LineNo, LineNo),
    pub ops: Vec<DiffOp>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Anchor {
    pub a: LineNo,
    pub b: LineNo,
}

pub trait DiffEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn diff(&self, a: &[&str], b: &[&str]) -> Vec<DiffOp>;
}

/// Group a flat list of ops into hunks. Equal runs become their own hunks
/// so the frontend can render context cheaply; runs of Insert/Delete become
/// change hunks with deterministic ids.
pub fn group_into_hunks(ops: &[DiffOp]) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut next_id: u32 = 0;
    let mut i = 0;
    while i < ops.len() {
        let is_equal = matches!(ops[i], DiffOp::Equal { .. });
        let mut j = i + 1;
        while j < ops.len() {
            let here_equal = matches!(ops[j], DiffOp::Equal { .. });
            if here_equal != is_equal {
                break;
            }
            j += 1;
        }
        let slice = &ops[i..j];
        let (a_range, b_range) = compute_ranges(slice);
        hunks.push(Hunk {
            id: next_id,
            a_range,
            b_range,
            ops: slice.to_vec(),
        });
        next_id += 1;
        i = j;
    }
    hunks
}

fn compute_ranges(ops: &[DiffOp]) -> ((LineNo, LineNo), (LineNo, LineNo)) {
    let mut a_min = LineNo::MAX;
    let mut a_max = 0u32;
    let mut b_min = LineNo::MAX;
    let mut b_max = 0u32;
    let mut saw_a = false;
    let mut saw_b = false;
    for op in ops {
        match op {
            DiffOp::Equal { a, b, .. } => {
                a_min = a_min.min(*a); a_max = a_max.max(*a); saw_a = true;
                b_min = b_min.min(*b); b_max = b_max.max(*b); saw_b = true;
            }
            DiffOp::Delete { a, .. } => {
                a_min = a_min.min(*a); a_max = a_max.max(*a); saw_a = true;
            }
            DiffOp::Insert { b, .. } => {
                b_min = b_min.min(*b); b_max = b_max.max(*b); saw_b = true;
            }
        }
    }
    let a = if saw_a { (a_min, a_max) } else { (0, 0) };
    let b = if saw_b { (b_min, b_max) } else { (0, 0) };
    (a, b)
}

/// Split text into lines preserving order. Trailing empty line after a final
/// newline is dropped (matches typical diff semantics).
pub fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}
