//! Search/find helpers used by every text pane.
//!
//! Engine-agnostic — no GUI deps so it can be unit-tested under
//! `cargo test --no-default-features --lib`. The matching code is shared
//! by `diff_view`, `merge_view`, and `result_pane`.

use regex::Regex;

/// A compiled find query. Built once whenever the user edits the query text
/// or toggles an option, and reused by every pane in the frame.
#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub raw: String,
    regex: Regex,
}

impl CompiledQuery {
    /// Build a query. Returns `Err` for invalid regex syntax (only possible
    /// when `regex` is true). Empty `query` strings should not be passed —
    /// callers are expected to skip compilation when the query is empty.
    pub fn build(
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        regex: bool,
    ) -> Result<Self, regex::Error> {
        let mut pattern = if regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        if whole_word {
            pattern = format!(r"\b(?:{})\b", pattern);
        }
        if !case_sensitive {
            pattern = format!("(?i){}", pattern);
        }
        let re = Regex::new(&pattern)?;
        Ok(Self {
            raw: query.to_string(),
            regex: re,
        })
    }
}

/// A single match, clipped to one line. Multi-line regex hits are split into
/// one `Match` per affected line so consumers only ever deal with same-line
/// ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// 1-based line number.
    pub line: u32,
    /// Char index of the match start within the line.
    pub start_col: usize,
    /// Char index of the match end (exclusive) within the line.
    pub end_col: usize,
    /// Byte offset of the match start in the full text.
    pub byte_start: usize,
    /// Byte offset of the match end (exclusive) in the full text.
    pub byte_end: usize,
}

/// Find every match of `q` in `text` and return one `Match` per affected line.
///
/// Zero-width matches are skipped to avoid infinite loops. Matches are
/// returned in source order.
pub fn find_matches_in_text(text: &str, q: &CompiledQuery) -> Vec<Match> {
    let mut out = Vec::new();
    // Precompute line starts as (byte_offset, line_number) so we can map a
    // byte position to (line, col_in_chars) cheaply.
    let line_starts = compute_line_starts(text);

    for m in q.regex.find_iter(text) {
        if m.start() == m.end() {
            continue;
        }
        let mut cur_byte = m.start();
        let end_byte = m.end();
        while cur_byte < end_byte {
            // Find the line containing cur_byte.
            let line_idx = match line_starts.binary_search_by(|&(off, _)| off.cmp(&cur_byte)) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let (line_start, line_no) = line_starts[line_idx];
            let line_end = line_starts
                .get(line_idx + 1)
                .map(|&(off, _)| off)
                .unwrap_or_else(|| text.len());
            // Exclude the trailing '\n' from the line slice if present.
            let line_text_end = if line_end > line_start
                && text.as_bytes().get(line_end - 1) == Some(&b'\n')
            {
                line_end - 1
            } else {
                line_end
            };

            let span_start = cur_byte.max(line_start);
            let span_end = end_byte.min(line_text_end);
            if span_end > span_start {
                let start_col = char_index_in_line(text, line_start, span_start);
                let end_col = char_index_in_line(text, line_start, span_end);
                out.push(Match {
                    line: line_no,
                    start_col,
                    end_col,
                    byte_start: span_start,
                    byte_end: span_end,
                });
            }
            // Advance to the next line.
            cur_byte = line_end.max(cur_byte + 1);
        }
    }
    out
}

fn compute_line_starts(text: &str) -> Vec<(usize, u32)> {
    let mut v = Vec::with_capacity(text.len() / 32 + 1);
    v.push((0usize, 1u32));
    let bytes = text.as_bytes();
    let mut line = 1u32;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            line += 1;
            v.push((i + 1, line));
        }
    }
    v
}

fn char_index_in_line(text: &str, line_start: usize, byte_pos: usize) -> usize {
    text[line_start..byte_pos].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(text: &str, q: &str, case: bool, word: bool, rx: bool) -> Vec<Match> {
        let cq = CompiledQuery::build(q, case, word, rx).unwrap();
        find_matches_in_text(text, &cq)
    }

    #[test]
    fn literal_hit() {
        let m = matches("hello world", "world", false, false, false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 1);
        assert_eq!(m[0].start_col, 6);
        assert_eq!(m[0].end_col, 11);
    }

    #[test]
    fn literal_miss() {
        let m = matches("hello world", "xyz", false, false, false);
        assert!(m.is_empty());
    }

    #[test]
    fn case_insensitive_default() {
        let m = matches("Hello WORLD", "world", false, false, false);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn case_sensitive_on() {
        let m = matches("Hello WORLD", "world", true, false, false);
        assert!(m.is_empty());
        let m = matches("Hello WORLD", "WORLD", true, false, false);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn whole_word_boundaries() {
        let m = matches("foo foobar", "foo", false, true, false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start_col, 0);
        assert_eq!(m[0].end_col, 3);
    }

    #[test]
    fn regex_syntax_error() {
        assert!(CompiledQuery::build("(", false, false, true).is_err());
    }

    #[test]
    fn multiple_matches_one_line() {
        let m = matches("a b a b a", "a", false, false, false);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].start_col, 0);
        assert_eq!(m[1].start_col, 4);
        assert_eq!(m[2].start_col, 8);
    }

    #[test]
    fn matches_across_lines() {
        let text = "foo\nbar\nfoo\n";
        let m = matches(text, "foo", false, false, false);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].line, 1);
        assert_eq!(m[1].line, 3);
    }

    #[test]
    fn regex_dot_does_not_cross_lines() {
        // `.` should not match `\n` in default regex; verify nothing weird
        // happens with a multi-line pattern.
        let text = "abc\ndef";
        let m = matches(text, "c.d", false, false, true);
        assert!(m.is_empty());
    }

    #[test]
    fn multi_line_regex_clipped() {
        // `(?s).+` matches everything including newlines; we expect one
        // Match per affected line.
        let text = "ab\ncd";
        let m = matches(text, "(?s)a.+d", false, false, true);
        // Two lines covered.
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].line, 1);
        assert_eq!(m[1].line, 2);
    }

    #[test]
    fn unicode_char_indices() {
        // "é" is multibyte; ensure char-index columns are correct.
        let text = "café latte";
        let m = matches(text, "latte", false, false, false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start_col, 5);
        assert_eq!(m[0].end_col, 10);
    }
}
