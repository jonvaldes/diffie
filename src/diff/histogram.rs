use std::ops::Range;

use imara_diff::{intern::InternedInput, Algorithm, Sink};

use super::{normalize::normalize_lines, DiffEngine, DiffOp, DiffOptions, Whitespace};

/// Histogram line-level diff (via the `imara-diff` crate).
#[derive(Clone)]
pub struct HistogramDiff;

impl DiffEngine for HistogramDiff {
    fn name(&self) -> &'static str { "histogram" }

    fn capabilities(&self) -> super::EngineCapabilities {
        super::EngineCapabilities { supports_moves: true }
    }

    fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp> {
        if matches!(opts.whitespace, Whitespace::None) {
            run_histogram(a, b)
        } else {
            let a_norm = normalize_lines(a, opts.whitespace);
            let b_norm = normalize_lines(b, opts.whitespace);
            let a_refs: Vec<&str> = a_norm.iter().map(|s| s.as_str()).collect();
            let b_refs: Vec<&str> = b_norm.iter().map(|s| s.as_str()).collect();
            // Run histogram against normalized lines, then map back to originals.
            let ops_norm = run_histogram(&a_refs, &b_refs);
            map_text_back(&ops_norm, a, b)
        }
    }
}

/// Replace the (normalized) text in each op with the matching original-line
/// text addressed by line number.
fn map_text_back(ops: &[DiffOp], a: &[&str], b: &[&str]) -> Vec<DiffOp> {
    ops.iter()
        .map(|op| match op {
            DiffOp::Equal { a: ai, b: bi, .. } => DiffOp::Equal {
                a: *ai,
                b: *bi,
                text: a[(*ai - 1) as usize].to_string(),
            },
            DiffOp::Delete { a: ai, .. } => DiffOp::delete(*ai, a[(*ai - 1) as usize].to_string()),
            DiffOp::Insert { b: bi, .. } => DiffOp::insert(*bi, b[(*bi - 1) as usize].to_string()),
        })
        .collect()
}

struct ChangeCollector {
    changes: Vec<(Range<u32>, Range<u32>)>,
}

impl Sink for ChangeCollector {
    type Out = Vec<(Range<u32>, Range<u32>)>;

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        self.changes.push((before, after));
    }

    fn finish(self) -> Self::Out { self.changes }
}

fn run_histogram(a: &[&str], b: &[&str]) -> Vec<DiffOp> {
    // imara-diff tokenizes &str inputs by lines, so we round-trip through
    // joined strings. Our lines never contain '\n' (split_lines guarantees).
    let before_text = a.join("\n");
    let after_text = b.join("\n");
    let input = InternedInput::new(before_text.as_str(), after_text.as_str());

    let changes = imara_diff::diff(
        Algorithm::Histogram,
        &input,
        ChangeCollector { changes: Vec::new() },
    );

    let mut out: Vec<DiffOp> = Vec::new();
    let mut a_cur: u32 = 0;
    let mut b_cur: u32 = 0;

    for (br, ar) in changes {
        while a_cur < br.start {
            out.push(DiffOp::Equal {
                a: a_cur + 1,
                b: b_cur + 1,
                text: a[a_cur as usize].to_string(),
            });
            a_cur += 1;
            b_cur += 1;
        }
        for i in br.clone() {
            out.push(DiffOp::delete(i + 1, a[i as usize].to_string()));
        }
        a_cur = br.end;
        for i in ar.clone() {
            out.push(DiffOp::insert(i + 1, b[i as usize].to_string()));
        }
        b_cur = ar.end;
    }

    while (a_cur as usize) < a.len() {
        out.push(DiffOp::Equal {
            a: a_cur + 1,
            b: b_cur + 1,
            text: a[a_cur as usize].to_string(),
        });
        a_cur += 1;
        b_cur += 1;
    }

    out
}
