//! Shared correctness tests run against every registered engine.

#![cfg(test)]

use super::{
    available_engines, build_engine, split_lines, DiffOp, DiffOptions, EngineCapabilities,
};

fn each_engine(case: impl Fn(&str, &dyn super::DiffEngine)) {
    for (name, _caps) in available_engines() {
        let engine = build_engine(&name).expect("engine builds");
        case(&name, engine.as_ref());
    }
}

fn run(engine: &dyn super::DiffEngine, a: &str, b: &str) -> Vec<DiffOp> {
    let al = split_lines(a);
    let bl = split_lines(b);
    engine.diff(&al, &bl, &DiffOptions::default())
}

#[test]
fn identity_yields_only_equals() {
    each_engine(|name, e| {
        let ops = run(e, "alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n");
        assert_eq!(ops.len(), 3, "engine={name}");
        assert!(
            ops.iter().all(|o| matches!(o, DiffOp::Equal { .. })),
            "engine={name}: expected all Equal, got {ops:?}"
        );
    });
}

#[test]
fn pure_insertion_keeps_existing_lines_equal() {
    each_engine(|name, e| {
        let ops = run(e, "a\nc\n", "a\nb\nc\n");
        let inserts = ops.iter().filter(|o| matches!(o, DiffOp::Insert { .. })).count();
        let deletes = ops.iter().filter(|o| matches!(o, DiffOp::Delete { .. })).count();
        let equals = ops.iter().filter(|o| matches!(o, DiffOp::Equal { .. })).count();
        assert_eq!(inserts, 1, "engine={name}");
        assert_eq!(deletes, 0, "engine={name}");
        assert_eq!(equals, 2, "engine={name}");
    });
}

#[test]
fn pure_deletion_keeps_existing_lines_equal() {
    each_engine(|name, e| {
        let ops = run(e, "a\nb\nc\n", "a\nc\n");
        let inserts = ops.iter().filter(|o| matches!(o, DiffOp::Insert { .. })).count();
        let deletes = ops.iter().filter(|o| matches!(o, DiffOp::Delete { .. })).count();
        let equals = ops.iter().filter(|o| matches!(o, DiffOp::Equal { .. })).count();
        assert_eq!(inserts, 0, "engine={name}");
        assert_eq!(deletes, 1, "engine={name}");
        assert_eq!(equals, 2, "engine={name}");
    });
}

#[test]
fn line_numbers_are_one_based_and_increasing() {
    each_engine(|name, e| {
        let ops = run(e, "a\nb\nc\nd\n", "a\nB\nc\nD\n");
        let mut last_a: u32 = 0;
        let mut last_b: u32 = 0;
        for op in &ops {
            match op {
                DiffOp::Equal { a, b, .. } => {
                    assert!(*a >= 1 && *b >= 1, "engine={name}: zero line no");
                    assert!(*a >= last_a, "engine={name}: a not monotonic");
                    assert!(*b >= last_b, "engine={name}: b not monotonic");
                    last_a = *a;
                    last_b = *b;
                }
                DiffOp::Delete { a, .. } => {
                    assert!(*a >= 1);
                    assert!(*a >= last_a, "engine={name}: a not monotonic in delete");
                    last_a = *a;
                }
                DiffOp::Insert { b, .. } => {
                    assert!(*b >= 1);
                    assert!(*b >= last_b, "engine={name}: b not monotonic in insert");
                    last_b = *b;
                }
            }
        }
    });
}

#[test]
fn every_a_line_appears_once_per_side() {
    // Each `a` line should appear in exactly one Equal or Delete op; each
    // `b` line in exactly one Equal or Insert op.
    each_engine(|name, e| {
        let a = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let b = "alpha\nBETA\ngamma\ndelta\nEPSILON\n";
        let al = split_lines(a);
        let bl = split_lines(b);
        let ops = e.diff(&al, &bl, &DiffOptions::default());

        let mut a_seen = vec![0u32; al.len()];
        let mut b_seen = vec![0u32; bl.len()];
        for op in &ops {
            match op {
                DiffOp::Equal { a, b, .. } => {
                    a_seen[(*a - 1) as usize] += 1;
                    b_seen[(*b - 1) as usize] += 1;
                }
                DiffOp::Delete { a, .. } => a_seen[(*a - 1) as usize] += 1,
                DiffOp::Insert { b, .. } => b_seen[(*b - 1) as usize] += 1,
            }
        }
        assert!(a_seen.iter().all(|c| *c == 1), "engine={name}: a counts {a_seen:?}");
        assert!(b_seen.iter().all(|c| *c == 1), "engine={name}: b counts {b_seen:?}");
    });
}

#[test]
fn whitespace_ignore_all_treats_indentation_change_as_equal() {
    let opts = DiffOptions { whitespace: super::Whitespace::IgnoreAll, ..Default::default() };
    each_engine(|name, e| {
        let a = split_lines("fn x() {\n    return 1;\n}\n");
        let b = split_lines("fn x() {\n\t\treturn 1;\n}\n");
        let ops = e.diff(&a, &b, &opts);
        // Every line treated as equal under IgnoreAll.
        assert!(
            ops.iter().all(|o| matches!(o, DiffOp::Equal { .. })),
            "engine={name}: got {ops:?}"
        );
        // Original text must be preserved (B's tabs, not A's spaces) in the
        // emitted Equal ops — engines use A's text for matched lines.
        let texts: Vec<&str> = ops.iter().filter_map(|o| match o {
            DiffOp::Equal { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert!(texts.iter().any(|t| t.contains("    return")), "engine={name}: original A text lost");
    });
}

#[test]
fn whitespace_ignore_trailing_eol_folds_crlf() {
    let opts = DiffOptions { whitespace: super::Whitespace::IgnoreTrailingEol, ..Default::default() };
    each_engine(|name, e| {
        let a_text = "hello\nworld\n";
        let b_text = "hello\r\nworld\r\n";
        let a = split_lines(a_text);
        let b = split_lines(b_text);
        let ops = e.diff(&a, &b, &opts);
        assert!(
            ops.iter().all(|o| matches!(o, DiffOp::Equal { .. })),
            "engine={name}: got {ops:?}"
        );
    });
}

#[test]
fn capability_matrix_matches_expectations() {
    use std::collections::HashMap;
    let expected: HashMap<&str, bool> = [
        ("myers", false),
        ("patience", false),
        ("histogram", true),
    ]
    .into_iter()
    .collect();
    for (name, caps) in available_engines() {
        let want = *expected
            .get(name.as_str())
            .unwrap_or_else(|| panic!("unexpected engine in registry: {name}"));
        assert_eq!(
            caps,
            EngineCapabilities { supports_moves: want },
            "engine={name}"
        );
    }
}

#[test]
fn engines_without_move_capability_never_emit_move_ids() {
    use crate::diff::DiffOp;
    let a = ["alpha", "beta", "gamma", "delta"];
    let b = ["delta", "gamma", "beta", "alpha"];
    let opts = DiffOptions {
        detect_moves: true,
        move_min_lines: 1,
        ..DiffOptions::default()
    };
    for (name, caps) in available_engines() {
        if caps.supports_moves {
            continue;
        }
        let engine = build_engine(&name).expect("registered engine");
        let ops = engine.diff(&a, &b, &opts);
        for op in &ops {
            let mid = match op {
                DiffOp::Delete { move_id, .. } | DiffOp::Insert { move_id, .. } => *move_id,
                _ => None,
            };
            assert_eq!(mid, None, "engine={name} emitted move_id={mid:?}");
        }
    }
}
