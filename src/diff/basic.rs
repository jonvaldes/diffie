use super::{DiffEngine, DiffOp, LineNo};

/// Diff engine backed by the `diff` crate (line-level Myers).
pub struct BasicDiff;

impl DiffEngine for BasicDiff {
    fn name(&self) -> &'static str {
        "basic"
    }

    fn diff(&self, a: &[&str], b: &[&str]) -> Vec<DiffOp> {
        let results = diff::slice(a, b);
        let mut out = Vec::with_capacity(results.len());
        let mut ai: LineNo = 1;
        let mut bi: LineNo = 1;
        for r in results {
            match r {
                diff::Result::Both(l, _) => {
                    out.push(DiffOp::Equal { a: ai, b: bi, text: (*l).to_string() });
                    ai += 1;
                    bi += 1;
                }
                diff::Result::Left(l) => {
                    out.push(DiffOp::Delete { a: ai, text: (*l).to_string() });
                    ai += 1;
                }
                diff::Result::Right(l) => {
                    out.push(DiffOp::Insert { b: bi, text: (*l).to_string() });
                    bi += 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&str> {
        crate::diff::split_lines(s)
    }

    #[test]
    fn identical_files() {
        let e = BasicDiff;
        let a = lines("alpha\nbeta\ngamma\n");
        let b = lines("alpha\nbeta\ngamma\n");
        let ops = e.diff(&a, &b);
        assert_eq!(ops.len(), 3);
        assert!(ops.iter().all(|o| matches!(o, DiffOp::Equal { .. })));
    }

    #[test]
    fn pure_insertion() {
        let e = BasicDiff;
        let a = lines("a\nc\n");
        let b = lines("a\nb\nc\n");
        let ops = e.diff(&a, &b);
        let inserts: Vec<_> = ops.iter().filter(|o| matches!(o, DiffOp::Insert { .. })).collect();
        assert_eq!(inserts.len(), 1);
        match inserts[0] {
            DiffOp::Insert { b: bn, text } => {
                assert_eq!(*bn, 2);
                assert_eq!(text, "b");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn pure_deletion() {
        let e = BasicDiff;
        let a = lines("a\nb\nc\n");
        let b = lines("a\nc\n");
        let ops = e.diff(&a, &b);
        let dels: Vec<_> = ops.iter().filter(|o| matches!(o, DiffOp::Delete { .. })).collect();
        assert_eq!(dels.len(), 1);
        match dels[0] {
            DiffOp::Delete { a: an, text } => {
                assert_eq!(*an, 2);
                assert_eq!(text, "b");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn interleaved_changes() {
        let e = BasicDiff;
        let a = lines("a\nb\nc\nd\n");
        let b = lines("a\nB\nc\nD\n");
        let ops = e.diff(&a, &b);
        let n_eq = ops.iter().filter(|o| matches!(o, DiffOp::Equal { .. })).count();
        let n_ins = ops.iter().filter(|o| matches!(o, DiffOp::Insert { .. })).count();
        let n_del = ops.iter().filter(|o| matches!(o, DiffOp::Delete { .. })).count();
        assert_eq!(n_eq, 2);
        assert_eq!(n_ins, 2);
        assert_eq!(n_del, 2);
    }
}
