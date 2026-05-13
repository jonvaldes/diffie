use super::{SubLineGranularity, SubSpan, SubSpanKind};

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
}
