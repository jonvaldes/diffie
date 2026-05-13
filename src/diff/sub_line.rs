use super::{DiffOp, SubLineGranularity, SubSpan, SubSpanKind};

/// Compute sub-line spans for a paired (deletion, insertion) line.
/// Returns `(del_spans, ins_spans)`. Each Vec covers the full line text
/// in non-overlapping ranges marked Same or Changed.
pub fn compute_pair(del_text: &str, ins_text: &str, granularity: SubLineGranularity) -> (Vec<SubSpan>, Vec<SubSpan>) {
    use similar::{ChangeTag, TextDiff};

    if matches!(granularity, SubLineGranularity::None) {
        return (Vec::new(), Vec::new());
    }

    let diff = match granularity {
        SubLineGranularity::Word => TextDiff::from_words(del_text, ins_text),
        // `similar` doesn't ship grapheme tokenization out of the box; char-mode
        // is the closest equivalent and already handles multibyte codepoints.
        SubLineGranularity::Char | SubLineGranularity::Grapheme => {
            TextDiff::from_chars(del_text, ins_text)
        }
        SubLineGranularity::None => unreachable!(),
    };

    let mut del_spans: Vec<SubSpan> = Vec::new();
    let mut ins_spans: Vec<SubSpan> = Vec::new();
    let mut del_cursor: u32 = 0;
    let mut ins_cursor: u32 = 0;

    for change in diff.iter_all_changes() {
        let value: &str = change.value();
        let len = value.len() as u32;
        match change.tag() {
            ChangeTag::Equal => {
                push_span(&mut del_spans, del_cursor, del_cursor + len, SubSpanKind::Same);
                push_span(&mut ins_spans, ins_cursor, ins_cursor + len, SubSpanKind::Same);
                del_cursor += len;
                ins_cursor += len;
            }
            ChangeTag::Delete => {
                push_span(&mut del_spans, del_cursor, del_cursor + len, SubSpanKind::Changed);
                del_cursor += len;
            }
            ChangeTag::Insert => {
                push_span(&mut ins_spans, ins_cursor, ins_cursor + len, SubSpanKind::Changed);
                ins_cursor += len;
            }
        }
    }
    (del_spans, ins_spans)
}

/// Walk a `DiffOp` stream and populate `spans` on paired Delete/Insert ops
/// (deletes paired with inserts inside the same change run). Equal ops and
/// unpaired Deletes/Inserts are left untouched.
pub fn populate_pair_spans(ops: &mut Vec<DiffOp>, granularity: SubLineGranularity) {
    if matches!(granularity, SubLineGranularity::None) {
        return;
    }

    // Walk consecutive Delete/Insert runs; within a run, pair up positionally.
    let mut i = 0;
    while i < ops.len() {
        if matches!(ops[i], DiffOp::Equal { .. }) {
            i += 1;
            continue;
        }
        // Find the end of this change run.
        let mut j = i;
        while j < ops.len() && !matches!(ops[j], DiffOp::Equal { .. }) {
            j += 1;
        }

        // Collect indices of Deletes and Inserts in this run.
        let dels: Vec<usize> = (i..j).filter(|k| matches!(ops[*k], DiffOp::Delete { .. })).collect();
        let inss: Vec<usize> = (i..j).filter(|k| matches!(ops[*k], DiffOp::Insert { .. })).collect();
        let pairs = dels.len().min(inss.len());
        for p in 0..pairs {
            let del_text = match &ops[dels[p]] {
                DiffOp::Delete { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            let ins_text = match &ops[inss[p]] {
                DiffOp::Insert { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            let (d_spans, i_spans) = compute_pair(&del_text, &ins_text, granularity);
            if let DiffOp::Delete { spans, .. } = &mut ops[dels[p]] {
                *spans = Some(d_spans);
            }
            if let DiffOp::Insert { spans, .. } = &mut ops[inss[p]] {
                *spans = Some(i_spans);
            }
        }
        i = j;
    }
}

fn push_span(out: &mut Vec<SubSpan>, start: u32, end: u32, kind: SubSpanKind) {
    if start == end { return; }
    if let Some(last) = out.last_mut() {
        if last.kind == kind && last.end == start {
            last.end = end;
            return;
        }
    }
    out.push(SubSpan { start, end, kind });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_diff_marks_changed_word() {
        let (d, i) = compute_pair("foo bar baz", "foo qux baz", SubLineGranularity::Word);
        assert!(d.iter().any(|s| s.kind == SubSpanKind::Changed));
        assert!(i.iter().any(|s| s.kind == SubSpanKind::Changed));
    }

    #[test]
    fn char_diff_byte_ranges_cover_input() {
        let del = "hello";
        let ins = "hallo";
        let (d, i) = compute_pair(del, ins, SubLineGranularity::Char);
        let d_total: u32 = d.iter().map(|s| s.end - s.start).sum();
        let i_total: u32 = i.iter().map(|s| s.end - s.start).sum();
        assert_eq!(d_total as usize, del.len());
        assert_eq!(i_total as usize, ins.len());
    }

    #[test]
    fn grapheme_diff_handles_unicode() {
        let del = "café";
        let ins = "cafe";
        let (d, i) = compute_pair(del, ins, SubLineGranularity::Grapheme);
        assert!(!d.is_empty());
        assert!(!i.is_empty());
    }

    #[test]
    fn none_returns_empty() {
        let (d, i) = compute_pair("a", "b", SubLineGranularity::None);
        assert!(d.is_empty());
        assert!(i.is_empty());
    }

    #[test]
    fn populate_fills_paired_ops_only() {
        let mut ops = vec![
            DiffOp::Equal { a: 1, b: 1, text: "keep".into() },
            DiffOp::delete(2, "foo bar".into()),
            DiffOp::insert(2, "foo baz".into()),
            DiffOp::delete(3, "lonely".into()),
            DiffOp::Equal { a: 4, b: 3, text: "end".into() },
        ];
        populate_pair_spans(&mut ops, SubLineGranularity::Word);
        match &ops[1] {
            DiffOp::Delete { spans, .. } => assert!(spans.is_some()),
            _ => panic!(),
        }
        match &ops[2] {
            DiffOp::Insert { spans, .. } => assert!(spans.is_some()),
            _ => panic!(),
        }
        match &ops[3] {
            DiffOp::Delete { spans, .. } => assert!(spans.is_none(), "unpaired delete should stay None"),
            _ => panic!(),
        }
    }
}
