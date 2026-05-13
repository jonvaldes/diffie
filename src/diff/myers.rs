use similar::Algorithm;

use super::{DiffEngine, DiffOp, DiffOptions};

/// Myers line-level diff (via the `similar` crate).
#[derive(Clone)]
pub struct MyersDiff;

impl DiffEngine for MyersDiff {
    fn name(&self) -> &'static str { "myers" }

    fn diff(&self, a: &[&str], b: &[&str], _opts: &DiffOptions) -> Vec<DiffOp> {
        crate::diff::similar_runner::run(Algorithm::Myers, a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::split_lines;

    fn ops(a: &str, b: &str) -> Vec<DiffOp> {
        let al = split_lines(a);
        let bl = split_lines(b);
        MyersDiff.diff(&al, &bl, &DiffOptions::default())
    }

    #[test]
    fn identical_files() {
        let v = ops("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n");
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(|o| matches!(o, DiffOp::Equal { .. })));
    }

    #[test]
    fn pure_insertion() {
        let v = ops("a\nc\n", "a\nb\nc\n");
        let inserts: Vec<_> = v.iter().filter(|o| matches!(o, DiffOp::Insert { .. })).collect();
        assert_eq!(inserts.len(), 1);
        if let DiffOp::Insert { b, text, .. } = inserts[0] {
            assert_eq!(*b, 2);
            assert_eq!(text, "b");
        }
    }

    #[test]
    fn pure_deletion() {
        let v = ops("a\nb\nc\n", "a\nc\n");
        let dels: Vec<_> = v.iter().filter(|o| matches!(o, DiffOp::Delete { .. })).collect();
        assert_eq!(dels.len(), 1);
        if let DiffOp::Delete { a, text, .. } = dels[0] {
            assert_eq!(*a, 2);
            assert_eq!(text, "b");
        }
    }
}
