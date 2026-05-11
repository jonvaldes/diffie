//! Character-level diff between two strings via LCS.
//!
//! Used to highlight the specific characters that actually changed between a
//! paired delete/insert line in a change hunk. Capped at MAX_LEN per side so
//! pathologically long lines fall back to row-level highlighting only.

const MAX_LEN: usize = 600;

#[derive(Clone, Copy, PartialEq)]
pub enum CharKind {
    Equal,
    Del,
    Ins,
}

pub struct CharRun {
    pub kind: CharKind,
    pub text: String,
}

pub fn char_diff(a: &str, b: &str) -> Vec<CharRun> {
    if a == b {
        return if a.is_empty() {
            Vec::new()
        } else {
            vec![CharRun {
                kind: CharKind::Equal,
                text: a.to_string(),
            }]
        };
    }
    if a.is_empty() {
        return vec![CharRun {
            kind: CharKind::Ins,
            text: b.to_string(),
        }];
    }
    if b.is_empty() {
        return vec![CharRun {
            kind: CharKind::Del,
            text: a.to_string(),
        }];
    }
    if a.chars().count() > MAX_LEN || b.chars().count() > MAX_LEN {
        return vec![
            CharRun {
                kind: CharKind::Del,
                text: a.to_string(),
            },
            CharRun {
                kind: CharKind::Ins,
                text: b.to_string(),
            },
        ];
    }

    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let m = av.len();
    let n = bv.len();
    let w = n + 1;
    // dp[i][j] = LCS length of a[i..] and b[j..]. Stored row-major in 1D.
    let mut dp = vec![0u32; (m + 1) * w];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * w + j] = if av[i] == bv[j] {
                dp[(i + 1) * w + (j + 1)] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + (j + 1)])
            };
        }
    }

    let mut runs: Vec<CharRun> = Vec::new();
    let mut cur_kind: Option<CharKind> = None;
    let mut cur_text = String::new();
    let push = |k: CharKind, c: char, runs: &mut Vec<CharRun>, cur_kind: &mut Option<CharKind>, cur_text: &mut String| {
        if *cur_kind == Some(k) {
            cur_text.push(c);
        } else {
            if let Some(prev) = *cur_kind {
                runs.push(CharRun {
                    kind: prev,
                    text: std::mem::take(cur_text),
                });
            }
            *cur_kind = Some(k);
            cur_text.push(c);
        }
    };

    let mut i = 0;
    let mut j = 0;
    while i < m && j < n {
        if av[i] == bv[j] {
            push(CharKind::Equal, av[i], &mut runs, &mut cur_kind, &mut cur_text);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + (j + 1)] {
            push(CharKind::Del, av[i], &mut runs, &mut cur_kind, &mut cur_text);
            i += 1;
        } else {
            push(CharKind::Ins, bv[j], &mut runs, &mut cur_kind, &mut cur_text);
            j += 1;
        }
    }
    while i < m {
        push(CharKind::Del, av[i], &mut runs, &mut cur_kind, &mut cur_text);
        i += 1;
    }
    while j < n {
        push(CharKind::Ins, bv[j], &mut runs, &mut cur_kind, &mut cur_text);
        j += 1;
    }
    if let Some(k) = cur_kind {
        runs.push(CharRun {
            kind: k,
            text: cur_text,
        });
    }
    runs
}

#[derive(Clone)]
pub struct Segment {
    pub text: String,
    pub hl: bool,
}

/// Segments for the left/delete side of a paired change.
pub fn left_segments(runs: &[CharRun]) -> Vec<Segment> {
    runs.iter()
        .filter(|r| r.kind != CharKind::Ins)
        .map(|r| Segment {
            text: r.text.clone(),
            hl: r.kind == CharKind::Del,
        })
        .collect()
}

/// Segments for the right/insert side of a paired change.
pub fn right_segments(runs: &[CharRun]) -> Vec<Segment> {
    runs.iter()
        .filter(|r| r.kind != CharKind::Del)
        .map(|r| Segment {
            text: r.text.clone(),
            hl: r.kind == CharKind::Ins,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(runs: &[CharRun]) -> String {
        runs.iter()
            .map(|r| {
                let tag = match r.kind {
                    CharKind::Equal => "=",
                    CharKind::Del => "-",
                    CharKind::Ins => "+",
                };
                format!("{}{}", tag, r.text)
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn equal_strings() {
        let r = char_diff("hello", "hello");
        assert_eq!(flat(&r), "=hello");
    }

    #[test]
    fn pure_insert() {
        let r = char_diff("", "abc");
        assert_eq!(flat(&r), "+abc");
    }

    #[test]
    fn pure_delete() {
        let r = char_diff("abc", "");
        assert_eq!(flat(&r), "-abc");
    }

    #[test]
    fn middle_change() {
        // foo_bar  →  foo_baz: last char changes.
        let r = char_diff("foo_bar", "foo_baz");
        assert_eq!(flat(&r), "=foo_ba|-r|+z");
    }

    #[test]
    fn left_segments_drops_inserts() {
        let r = char_diff("foo_bar", "foo_baz");
        let s = left_segments(&r);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text, "foo_ba");
        assert!(!s[0].hl);
        assert_eq!(s[1].text, "r");
        assert!(s[1].hl);
    }
}
