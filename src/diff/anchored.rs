use super::{Anchor, DiffEngine, DiffOp, LineNo};

/// Wraps any `DiffEngine` and forces matches at user-supplied anchors.
///
/// Hard anchors: each anchor `(a_i, b_i)` is treated as an equal pair.
/// The two files are split at the anchor lines and each segment between
/// consecutive anchors (and the segments before the first and after the
/// last anchor) is diffed independently with `inner`. Results are stitched
/// back together with line numbers re-offset to the original files.
pub struct AnchoredDiff<E: DiffEngine> {
    pub inner: E,
    pub anchors: Vec<Anchor>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnchorError {
    #[error("anchor a={a} b={b} is out of range (a_len={a_len}, b_len={b_len})")]
    OutOfRange { a: LineNo, b: LineNo, a_len: usize, b_len: usize },
    #[error("anchors must be strictly increasing in both files")]
    NotStrictlyIncreasing,
}

impl<E: DiffEngine> AnchoredDiff<E> {
    pub fn new(inner: E, anchors: Vec<Anchor>) -> Self {
        Self { inner, anchors }
    }

    pub fn validate(&self, a_len: usize, b_len: usize) -> Result<(), AnchorError> {
        let mut prev_a = 0u32;
        let mut prev_b = 0u32;
        for anc in &self.anchors {
            if anc.a == 0 || anc.b == 0
                || (anc.a as usize) > a_len
                || (anc.b as usize) > b_len
            {
                return Err(AnchorError::OutOfRange {
                    a: anc.a, b: anc.b, a_len, b_len,
                });
            }
            if anc.a <= prev_a || anc.b <= prev_b {
                return Err(AnchorError::NotStrictlyIncreasing);
            }
            prev_a = anc.a;
            prev_b = anc.b;
        }
        Ok(())
    }

    pub fn diff_checked(&self, a: &[&str], b: &[&str]) -> Result<Vec<DiffOp>, AnchorError> {
        self.validate(a.len(), b.len())?;
        Ok(self.diff(a, b))
    }
}

impl<E: DiffEngine> DiffEngine for AnchoredDiff<E> {
    fn name(&self) -> &'static str {
        "anchored"
    }

    fn diff(&self, a: &[&str], b: &[&str]) -> Vec<DiffOp> {
        if self.anchors.is_empty() {
            return self.inner.diff(a, b);
        }

        let mut out: Vec<DiffOp> = Vec::new();

        // Segments are bounded by virtual anchors at (0,0) and (len_a+1, len_b+1).
        // Each real anchor is also emitted as a single Equal op for the matched
        // line pair so the stitched edit script is contiguous.
        let mut prev_a: LineNo = 0;
        let mut prev_b: LineNo = 0;

        let total = self.anchors.len();
        for (i, anc) in self.anchors.iter().enumerate() {
            // Segment between previous anchor (exclusive) and this anchor (exclusive)
            // i.e. lines [prev_a+1 .. anc.a-1] in A and [prev_b+1 .. anc.b-1] in B.
            let a_lo = prev_a as usize; // 0-based start
            let a_hi = (anc.a as usize).saturating_sub(1); // 0-based end-exclusive
            let b_lo = prev_b as usize;
            let b_hi = (anc.b as usize).saturating_sub(1);
            let a_seg = &a[a_lo..a_hi];
            let b_seg = &b[b_lo..b_hi];
            let seg_ops = self.inner.diff(a_seg, b_seg);
            push_offset(&mut out, &seg_ops, prev_a, prev_b);

            // The anchor itself: emit as Equal pinning the two lines together.
            let a_text: &str = a[(anc.a as usize) - 1];
            let _b_text: &str = b[(anc.b as usize) - 1];
            // We emit one Equal op using A's text. (Texts at an anchor pair may
            // differ; the anchor is a *positional* pin, not a content claim. We
            // use A's text because the merged result follows A's coordinate when
            // both sides are present in the consumer's logic.)
            out.push(DiffOp::Equal { a: anc.a, b: anc.b, text: a_text.to_string() });

            prev_a = anc.a;
            prev_b = anc.b;
            let _ = (i, total);
        }

        // Tail segment after the last anchor.
        let a_seg = &a[(prev_a as usize)..];
        let b_seg = &b[(prev_b as usize)..];
        let seg_ops = self.inner.diff(a_seg, b_seg);
        push_offset(&mut out, &seg_ops, prev_a, prev_b);

        out
    }
}

fn push_offset(out: &mut Vec<DiffOp>, ops: &[DiffOp], a_off: LineNo, b_off: LineNo) {
    for op in ops {
        match op {
            DiffOp::Equal { a, b, text } => out.push(DiffOp::Equal {
                a: a + a_off, b: b + b_off, text: text.clone(),
            }),
            DiffOp::Delete { a, text } => out.push(DiffOp::Delete {
                a: a + a_off, text: text.clone(),
            }),
            DiffOp::Insert { b, text } => out.push(DiffOp::Insert {
                b: b + b_off, text: text.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::basic::BasicDiff;
    use crate::diff::split_lines;

    #[test]
    fn no_anchors_matches_inner() {
        let a = split_lines("a\nb\nc\n");
        let b = split_lines("a\nx\nc\n");
        let inner_ops = BasicDiff.diff(&a, &b);
        let anchored = AnchoredDiff::new(BasicDiff, vec![]);
        assert_eq!(anchored.diff(&a, &b), inner_ops);
    }

    #[test]
    fn anchor_forces_alignment() {
        // Without an anchor the basic Myers would happily match the two `x`
        // lines across what we consider "different" sections. Anchoring
        // (1,1) and (4,4) splits the file at those points so each segment
        // is diffed independently.
        let a = split_lines("HEADER\nx\nA1\nFOOTER\n");
        let b = split_lines("HEADER\nB1\nx\nFOOTER\n");

        // Anchors pin (HEADER,HEADER), (A1,B1), (FOOTER,FOOTER). The lone
        // `x` lines now live in *different* segments so they cannot be
        // matched across the anchor.
        let anchors = vec![
            Anchor { a: 1, b: 1 },
            Anchor { a: 3, b: 2 },
            Anchor { a: 4, b: 4 },
        ];
        let anchored = AnchoredDiff::new(BasicDiff, anchors);
        let ops = anchored.diff(&a, &b);

        let equals: Vec<_> = ops.iter().filter_map(|o| match o {
            DiffOp::Equal { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert!(equals.contains(&"HEADER"));
        assert!(equals.contains(&"FOOTER"));
        assert!(equals.contains(&"A1")); // anchor pins this
        // No Equal for `x`: each is in its own segment paired with empty.
        let n_x_equal = equals.iter().filter(|t| **t == "x").count();
        assert_eq!(n_x_equal, 0);
    }

    #[test]
    fn anchor_validation() {
        let a = split_lines("a\nb\n");
        let b = split_lines("c\nd\n");
        let bad = AnchoredDiff::new(BasicDiff, vec![Anchor { a: 5, b: 1 }]);
        assert!(matches!(bad.diff_checked(&a, &b), Err(AnchorError::OutOfRange { .. })));

        let bad2 = AnchoredDiff::new(BasicDiff, vec![
            Anchor { a: 2, b: 1 },
            Anchor { a: 1, b: 2 },
        ]);
        assert_eq!(bad2.diff_checked(&a, &b), Err(AnchorError::NotStrictlyIncreasing));
    }

    #[test]
    fn line_numbers_are_in_original_file_coords() {
        let a = split_lines("h\nx\np\nq\nf\n");
        let b = split_lines("h\ny\np\nq\nf\n");
        let anchors = vec![Anchor { a: 1, b: 1 }, Anchor { a: 5, b: 5 }];
        let anchored = AnchoredDiff::new(BasicDiff, anchors);
        let ops = anchored.diff(&a, &b);

        // p (line 3 in both) must remain Equal with original line numbers.
        let p_eq = ops.iter().find(|o| matches!(o, DiffOp::Equal { text, .. } if text == "p"));
        match p_eq {
            Some(DiffOp::Equal { a: an, b: bn, .. }) => {
                assert_eq!(*an, 3);
                assert_eq!(*bn, 3);
            }
            _ => panic!("expected Equal for `p` with original line numbers"),
        }
    }
}
