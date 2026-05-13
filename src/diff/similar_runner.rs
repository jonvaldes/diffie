use similar::{capture_diff_slices, Algorithm};

use super::{normalize::normalize_lines, DiffOp, Whitespace};

/// Convert a `similar` algorithm's output on `&[&str]` line slices into
/// our per-line `DiffOp` stream. 1-based line numbers. Lines are compared
/// after applying the requested whitespace normalization, but emitted
/// `DiffOp`s always carry the original line text.
pub fn run(alg: Algorithm, a: &[&str], b: &[&str], whitespace: Whitespace) -> Vec<DiffOp> {
    let ops = if matches!(whitespace, Whitespace::None) {
        capture_diff_slices(alg, a, b)
    } else {
        let a_norm = normalize_lines(a, whitespace);
        let b_norm = normalize_lines(b, whitespace);
        capture_diff_slices(alg, &a_norm, &b_norm)
    };
    let mut out: Vec<DiffOp> = Vec::new();
    for op in ops {
        match op {
            similar::DiffOp::Equal { old_index, new_index, len } => {
                for i in 0..len {
                    out.push(DiffOp::Equal {
                        a: (old_index + i + 1) as u32,
                        b: (new_index + i + 1) as u32,
                        text: a[old_index + i].to_string(),
                    });
                }
            }
            similar::DiffOp::Delete { old_index, old_len, .. } => {
                for i in 0..old_len {
                    out.push(DiffOp::delete(
                        (old_index + i + 1) as u32,
                        a[old_index + i].to_string(),
                    ));
                }
            }
            similar::DiffOp::Insert { new_index, new_len, .. } => {
                for i in 0..new_len {
                    out.push(DiffOp::insert(
                        (new_index + i + 1) as u32,
                        b[new_index + i].to_string(),
                    ));
                }
            }
            similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                for i in 0..old_len {
                    out.push(DiffOp::delete(
                        (old_index + i + 1) as u32,
                        a[old_index + i].to_string(),
                    ));
                }
                for i in 0..new_len {
                    out.push(DiffOp::insert(
                        (new_index + i + 1) as u32,
                        b[new_index + i].to_string(),
                    ));
                }
            }
        }
    }
    out
}
