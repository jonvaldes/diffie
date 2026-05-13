pub mod anchored;
pub mod histogram;
pub mod myers;
pub mod normalize;
pub mod patience;
pub mod similar_runner;
pub mod sub_line;

#[cfg(test)]
mod corpus_tests;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub type LineNo = u32;

/// A sub-line span produced by the sub-line refinement post-pass.
/// Byte offsets are into the op's `text` string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubSpan {
    pub start: u32,
    pub end: u32,
    pub kind: SubSpanKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubSpanKind {
    Same,
    Changed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffOp {
    Equal {
        a: LineNo,
        b: LineNo,
        text: String,
    },
    Delete {
        a: LineNo,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spans: Option<Vec<SubSpan>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_id: Option<u32>,
    },
    Insert {
        b: LineNo,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spans: Option<Vec<SubSpan>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_id: Option<u32>,
    },
}

impl DiffOp {
    pub fn delete(a: LineNo, text: String) -> Self {
        DiffOp::Delete { a, text, spans: None, move_id: None }
    }
    pub fn insert(b: LineNo, text: String) -> Self {
        DiffOp::Insert { b, text, spans: None, move_id: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hunk {
    pub id: u32,
    pub a_range: (LineNo, LineNo),
    pub b_range: (LineNo, LineNo),
    pub ops: Vec<DiffOp>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Anchor {
    pub a: LineNo,
    pub b: LineNo,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Whitespace {
    None,
    IgnoreAll,
    IgnoreLeading,
    IgnoreTrailingEol,
}

impl Default for Whitespace {
    fn default() -> Self { Whitespace::None }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubLineGranularity {
    None,
    Word,
    Char,
    Grapheme,
}

impl Default for SubLineGranularity {
    fn default() -> Self { SubLineGranularity::None }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffOptions {
    pub whitespace: Whitespace,
    pub sub_line: SubLineGranularity,
    pub detect_moves: bool,
    pub move_min_lines: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            whitespace: Whitespace::None,
            sub_line: SubLineGranularity::None,
            detect_moves: false,
            move_min_lines: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EngineCapabilities {
    pub supports_moves: bool,
}

pub trait DiffEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::default()
    }
    fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp>;
}

/// Engine factory: produces a fresh boxed engine for a given name.
pub type EngineFactory = fn() -> Box<dyn DiffEngine>;

pub struct EngineRegistry {
    entries: Vec<EngineEntry>,
}

pub struct EngineEntry {
    pub name: &'static str,
    pub capabilities: EngineCapabilities,
    pub factory: EngineFactory,
}

impl EngineRegistry {
    fn new() -> Self {
        Self {
            entries: vec![
                EngineEntry {
                    name: "myers",
                    capabilities: EngineCapabilities::default(),
                    factory: || Box::new(myers::MyersDiff),
                },
                EngineEntry {
                    name: "patience",
                    capabilities: EngineCapabilities::default(),
                    factory: || Box::new(patience::PatienceDiff),
                },
                EngineEntry {
                    name: "histogram",
                    capabilities: EngineCapabilities::default(),
                    factory: || Box::new(histogram::HistogramDiff),
                },
            ],
        }
    }

    pub fn entries(&self) -> &[EngineEntry] { &self.entries }

    pub fn get(&self, name: &str) -> Option<&EngineEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

pub fn registry() -> &'static EngineRegistry {
    static REG: OnceLock<EngineRegistry> = OnceLock::new();
    REG.get_or_init(EngineRegistry::new)
}

/// List all registered engine names paired with their capabilities.
pub fn available_engines() -> Vec<(String, EngineCapabilities)> {
    registry()
        .entries()
        .iter()
        .map(|e| (e.name.to_string(), e.capabilities))
        .collect()
}

/// Build a fresh engine instance by name. Returns `None` if the name is
/// not registered.
pub fn build_engine(name: &str) -> Option<Box<dyn DiffEngine>> {
    registry().get(name).map(|e| (e.factory)())
}

/// Group a flat list of ops into hunks. Equal runs become their own hunks
/// so the frontend can render context cheaply; runs of Insert/Delete become
/// change hunks with deterministic ids.
pub fn group_into_hunks(ops: &[DiffOp]) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut next_id: u32 = 0;
    let mut i = 0;
    while i < ops.len() {
        let is_equal = matches!(ops[i], DiffOp::Equal { .. });
        let mut j = i + 1;
        while j < ops.len() {
            let here_equal = matches!(ops[j], DiffOp::Equal { .. });
            if here_equal != is_equal {
                break;
            }
            j += 1;
        }
        let slice = &ops[i..j];
        let (a_range, b_range) = compute_ranges(slice);
        hunks.push(Hunk {
            id: next_id,
            a_range,
            b_range,
            ops: slice.to_vec(),
        });
        next_id += 1;
        i = j;
    }
    hunks
}

fn compute_ranges(ops: &[DiffOp]) -> ((LineNo, LineNo), (LineNo, LineNo)) {
    let mut a_min = LineNo::MAX;
    let mut a_max = 0u32;
    let mut b_min = LineNo::MAX;
    let mut b_max = 0u32;
    let mut saw_a = false;
    let mut saw_b = false;
    for op in ops {
        match op {
            DiffOp::Equal { a, b, .. } => {
                a_min = a_min.min(*a); a_max = a_max.max(*a); saw_a = true;
                b_min = b_min.min(*b); b_max = b_max.max(*b); saw_b = true;
            }
            DiffOp::Delete { a, .. } => {
                a_min = a_min.min(*a); a_max = a_max.max(*a); saw_a = true;
            }
            DiffOp::Insert { b, .. } => {
                b_min = b_min.min(*b); b_max = b_max.max(*b); saw_b = true;
            }
        }
    }
    let a = if saw_a { (a_min, a_max) } else { (0, 0) };
    let b = if saw_b { (b_min, b_max) } else { (0, 0) };
    (a, b)
}

/// Split text into lines preserving order. Trailing empty line after a final
/// newline is dropped (matches typical diff semantics).
pub fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}
