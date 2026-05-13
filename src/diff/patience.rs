use similar::Algorithm;

use super::{DiffEngine, DiffOp, DiffOptions};

/// Patience line-level diff (via the `similar` crate).
#[derive(Clone)]
pub struct PatienceDiff;

impl DiffEngine for PatienceDiff {
    fn name(&self) -> &'static str { "patience" }

    fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp> {
        crate::diff::similar_runner::run(Algorithm::Patience, a, b, opts.whitespace)
    }
}
