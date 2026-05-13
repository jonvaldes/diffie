use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

use serde::{Deserialize, Serialize};

use crate::diff::{anchored::AnchoredDiff, build_engine as build_diff_engine, group_into_hunks, myers::MyersDiff, split_lines, Anchor, DiffEngine, DiffOp, DiffOptions, Hunk};
use crate::merge::{apply_resolutions, MergeAnchor, MergeHunk, Resolution, ThreeWayMerge};

pub type SessionId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TwoWaySide {
    A,
    B,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HunkDecision {
    AcceptA,
    AcceptB,
    Both,
    Neither,
    Custom { text: Vec<String> },
    /// Per-line keep/skip mask. Length must equal the hunk's op count.
    PerLine { keep: Vec<bool> },
}

#[derive(Debug, Clone)]
pub enum SessionMode {
    TwoWay {
        a_lines: Vec<String>,
        b_lines: Vec<String>,
        anchors: Vec<Anchor>,
        hunks: Vec<Hunk>,
        decisions: HashMap<u32, HunkDecision>,
    },
    ThreeWay {
        base_lines: Vec<String>,
        local_lines: Vec<String>,
        remote_lines: Vec<String>,
        anchors: Vec<MergeAnchor>,
        hunks: Vec<MergeHunk>,
        resolutions: HashMap<u32, Resolution>,
    },
}

#[derive(Debug, Clone)]
pub struct DiffSession {
    pub id: SessionId,
    pub engine: String,
    pub options: DiffOptions,
    pub mode: SessionMode,
    /// User-edited result buffer (overrides computed result when set).
    pub manual_result: Option<String>,
}

#[derive(Default)]
pub struct SessionStore {
    next_id: AtomicU64,
    sessions: Mutex<HashMap<SessionId, DiffSession>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("unknown session id: {0}")]
    UnknownSession(SessionId),
    #[error("unknown engine: {0}")]
    UnknownEngine(String),
    #[error("wrong session mode for this operation")]
    WrongMode,
    #[error("anchor error: {0}")]
    Anchor(#[from] crate::diff::anchored::AnchorError),
}

fn build_engine(name: &str) -> Result<Box<dyn DiffEngine>, SessionError> {
    build_diff_engine(name).ok_or_else(|| SessionError::UnknownEngine(name.to_string()))
}

pub fn available_engines() -> Vec<String> {
    crate::diff::available_engines().into_iter().map(|(n, _)| n).collect()
}

pub fn engine_capabilities(name: &str) -> Option<crate::diff::EngineCapabilities> {
    crate::diff::registry().get(name).map(|e| e.capabilities)
}

/// First registered engine name, used as the default when callers don't
/// specify one.
fn default_engine_name() -> String {
    crate::diff::available_engines()
        .into_iter()
        .next()
        .map(|(n, _)| n)
        .unwrap_or_else(|| "myers".to_string())
}

/// Walk all hunks in order and emit the lines that should make up the
/// rebuilt `target` side: untouched hunks keep their current target-side
/// content (Equal + the target's own change-op text), but the targeted
/// hunk uses the OTHER side's content. Used by `replace_hunk_side`.
fn rebuild_for_replace(hunks: &[Hunk], target_hunk: u32, target: TwoWaySide) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in hunks {
        let is_target = h.id == target_hunk;
        for op in &h.ops {
            let (include, text) = match op {
                DiffOp::Equal { text, .. } => (true, text.clone()),
                DiffOp::Delete { text, .. } => {
                    let keep = if is_target {
                        target == TwoWaySide::B
                    } else {
                        target == TwoWaySide::A
                    };
                    (keep, text.clone())
                }
                DiffOp::Insert { text, .. } => {
                    let keep = if is_target {
                        target == TwoWaySide::A
                    } else {
                        target == TwoWaySide::B
                    };
                    (keep, text.clone())
                }
            };
            if include {
                out.push(text);
            }
        }
    }
    out
}

fn refs<'a>(v: &'a [String]) -> Vec<&'a str> {
    v.iter().map(|s| s.as_str()).collect()
}

fn recompute_two_way(
    engine_name: &str,
    a_lines: &[String],
    b_lines: &[String],
    anchors: &[Anchor],
    opts: &DiffOptions,
) -> Result<Vec<Hunk>, SessionError> {
    let inner = build_engine(engine_name)?;
    let a = refs(a_lines);
    let b = refs(b_lines);
    let ops: Vec<DiffOp> = if anchors.is_empty() {
        inner.diff(&a, &b, opts)
    } else {
        // Adapter to wrap a Box<dyn DiffEngine> inside AnchoredDiff (which is
        // generic over E: DiffEngine).
        struct DynEngine<'a>(&'a dyn DiffEngine);
        impl<'a> DiffEngine for DynEngine<'a> {
            fn name(&self) -> &'static str { "dyn" }
            fn capabilities(&self) -> crate::diff::EngineCapabilities {
                self.0.capabilities()
            }
            fn diff(&self, a: &[&str], b: &[&str], opts: &DiffOptions) -> Vec<DiffOp> {
                self.0.diff(a, b, opts)
            }
        }
        let wrapper = AnchoredDiff::new(DynEngine(inner.as_ref()), anchors.to_vec());
        wrapper.diff_checked(&a, &b, opts)?
    };
    Ok(group_into_hunks(&split_trivial_equals(ops)))
}

/// Reject Myers matches on whitespace-only lines. Such "matches" between
/// distant blanks (or coincidental `}` / `{` sections) produce tiny equal
/// hunks that drag the connector ribbons across huge vertical distances and
/// leave blank rows on each side that look out of place. Treating them as
/// independent delete+insert keeps the diff visually local and gives each
/// blank its own insert/delete background.
fn split_trivial_equals(ops: Vec<DiffOp>) -> Vec<DiffOp> {
    let mut out: Vec<DiffOp> = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            DiffOp::Equal { a, b, text } if text.trim().is_empty() => {
                out.push(DiffOp::delete(a, text.clone()));
                out.push(DiffOp::insert(b, text));
            }
            other => out.push(other),
        }
    }
    out
}

fn recompute_three_way(
    engine_name: &str,
    base: &[String],
    local: &[String],
    remote: &[String],
    anchors: &[MergeAnchor],
    opts: &DiffOptions,
) -> Result<Vec<MergeHunk>, SessionError> {
    // Three-way merge is generic over a concrete engine type for performance,
    // so we dispatch by name. Initial set: myers, patience, histogram.
    match engine_name {
        "myers" => {
            let m = ThreeWayMerge::new(MyersDiff);
            Ok(m.merge(&refs(base), &refs(local), &refs(remote), anchors, opts))
        }
        "patience" => {
            let m = ThreeWayMerge::new(crate::diff::patience::PatienceDiff);
            Ok(m.merge(&refs(base), &refs(local), &refs(remote), anchors, opts))
        }
        "histogram" => {
            let m = ThreeWayMerge::new(crate::diff::histogram::HistogramDiff);
            Ok(m.merge(&refs(base), &refs(local), &refs(remote), anchors, opts))
        }
        other => Err(SessionError::UnknownEngine(other.to_string())),
    }
}

impl SessionStore {
    pub fn new() -> Self { Self::default() }

    fn alloc_id(&self) -> SessionId { self.next_id.fetch_add(1, Ordering::Relaxed) + 1 }

    pub fn open_two_way(
        &self,
        a_text: &str,
        b_text: &str,
        engine: Option<String>,
    ) -> Result<SessionId, SessionError> {
        self.open_two_way_with(a_text, b_text, engine, DiffOptions::default())
    }

    pub fn open_two_way_with(
        &self,
        a_text: &str,
        b_text: &str,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let a_lines: Vec<String> = split_lines(a_text).into_iter().map(|s| s.to_string()).collect();
        let b_lines: Vec<String> = split_lines(b_text).into_iter().map(|s| s.to_string()).collect();
        let hunks = recompute_two_way(&engine, &a_lines, &b_lines, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::TwoWay {
                a_lines, b_lines, anchors: vec![], hunks, decisions: HashMap::new(),
            },
            manual_result: None,
        };
        self.sessions.lock().unwrap().insert(id, s);
        Ok(id)
    }

    pub fn open_three_way(
        &self,
        base_text: &str,
        local_text: &str,
        remote_text: &str,
        engine: Option<String>,
    ) -> Result<SessionId, SessionError> {
        self.open_three_way_with(base_text, local_text, remote_text, engine, DiffOptions::default())
    }

    pub fn open_three_way_with(
        &self,
        base_text: &str,
        local_text: &str,
        remote_text: &str,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let base_lines: Vec<String> = split_lines(base_text).into_iter().map(|s| s.to_string()).collect();
        let local_lines: Vec<String> = split_lines(local_text).into_iter().map(|s| s.to_string()).collect();
        let remote_lines: Vec<String> = split_lines(remote_text).into_iter().map(|s| s.to_string()).collect();
        let hunks = recompute_three_way(&engine, &base_lines, &local_lines, &remote_lines, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::ThreeWay {
                base_lines, local_lines, remote_lines, anchors: vec![], hunks, resolutions: HashMap::new(),
            },
            manual_result: None,
        };
        self.sessions.lock().unwrap().insert(id, s);
        Ok(id)
    }

    pub fn with<F, R>(&self, id: SessionId, f: F) -> Result<R, SessionError>
    where
        F: FnOnce(&mut DiffSession) -> Result<R, SessionError>,
    {
        let mut g = self.sessions.lock().unwrap();
        let s = g.get_mut(&id).ok_or(SessionError::UnknownSession(id))?;
        f(s)
    }

    pub fn snapshot(&self, id: SessionId) -> Result<DiffSession, SessionError> {
        let g = self.sessions.lock().unwrap();
        let s = g.get(&id).ok_or(SessionError::UnknownSession(id))?;
        Ok(s.clone())
    }

    /// Replace one side's content for the given hunk with the other side's
    /// content. The whole target file is reconstructed by walking all hunks
    /// in order and emitting either the current side's lines (untouched
    /// hunks) or the other side's lines (the targeted hunk). Hunks are then
    /// recomputed against the new file.
    ///
    /// `target` is the side being rewritten (e.g. `TwoWaySide::B` to make
    /// B match A for this hunk).
    pub fn replace_hunk_side(
        &self,
        id: SessionId,
        hunk_id: u32,
        target: TwoWaySide,
    ) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay {
                    a_lines,
                    b_lines,
                    anchors,
                    hunks,
                    ..
                } => {
                    let rebuilt = rebuild_for_replace(hunks, hunk_id, target);
                    match target {
                        TwoWaySide::A => *a_lines = rebuilt,
                        TwoWaySide::B => *b_lines = rebuilt,
                    }
                    let new_hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    /// Replace `target_side[range]` with `replacement` and recompute hunks.
    /// The unified "structural edit" used by line deletes, line inserts, and
    /// range deletions of selected text.
    pub fn splice_two_way_lines(
        &self,
        id: SessionId,
        side: TwoWaySide,
        range: std::ops::Range<usize>,
        replacement: Vec<String>,
    ) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay {
                    a_lines,
                    b_lines,
                    anchors,
                    hunks,
                    ..
                } => {
                    let target_len = match side {
                        TwoWaySide::A => a_lines.len(),
                        TwoWaySide::B => b_lines.len(),
                    };
                    let start = range.start.min(target_len);
                    let end = range.end.min(target_len).max(start);
                    match side {
                        TwoWaySide::A => {
                            a_lines.splice(start..end, replacement.into_iter());
                        }
                        TwoWaySide::B => {
                            b_lines.splice(start..end, replacement.into_iter());
                        }
                    }
                    let new_hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    /// Replace the entire contents of one side of a 2-way session and
    /// recompute hunks. Used by undo to restore a whole-buffer snapshot
    /// after `replace_hunk_side`.
    pub fn set_two_way_lines(
        &self,
        id: SessionId,
        side: TwoWaySide,
        new_lines: Vec<String>,
    ) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay {
                    a_lines,
                    b_lines,
                    anchors,
                    hunks,
                    ..
                } => {
                    match side {
                        TwoWaySide::A => *a_lines = new_lines,
                        TwoWaySide::B => *b_lines = new_lines,
                    }
                    let new_hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    /// Replace a single line of the underlying A or B file (1-based line
    /// number) and recompute hunks. Used by the diff view's in-place row
    /// editor in 2-way comparisons.
    pub fn set_two_way_line(
        &self,
        id: SessionId,
        side: TwoWaySide,
        line_no: u32,
        text: String,
    ) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay {
                    a_lines,
                    b_lines,
                    anchors,
                    hunks,
                    ..
                } => {
                    let idx = (line_no as usize).checked_sub(1).unwrap_or(0);
                    let target = match side {
                        TwoWaySide::A => &mut *a_lines,
                        TwoWaySide::B => &mut *b_lines,
                    };
                    if idx >= target.len() {
                        return Err(SessionError::WrongMode);
                    }
                    target[idx] = text;
                    let new_hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    pub fn add_anchor_two_way(&self, id: SessionId, anchor: Anchor) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay { a_lines, b_lines, anchors, hunks, .. } => {
                    let mut new_anchors = anchors.clone();
                    new_anchors.push(anchor);
                    new_anchors.sort_by_key(|a| (a.a, a.b));
                    let new_hunks = recompute_two_way(&engine, a_lines, b_lines, &new_anchors, &options)?;
                    *anchors = new_anchors;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    pub fn add_anchor_three_way(&self, id: SessionId, anchor: MergeAnchor) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::ThreeWay { base_lines, local_lines, remote_lines, anchors, hunks, .. } => {
                    let mut new_anchors = anchors.clone();
                    new_anchors.push(anchor);
                    new_anchors.sort_by_key(|a| a.base);
                    let new_hunks = recompute_three_way(&engine, base_lines, local_lines, remote_lines, &new_anchors, &options)?;
                    *anchors = new_anchors;
                    *hunks = new_hunks;
                    Ok(())
                }
                _ => Err(SessionError::WrongMode),
            }
        })
    }

    pub fn remove_anchor(&self, id: SessionId, idx: usize) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay { a_lines, b_lines, anchors, hunks, .. } => {
                    if idx >= anchors.len() { return Ok(()); }
                    anchors.remove(idx);
                    *hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                    Ok(())
                }
                SessionMode::ThreeWay { base_lines, local_lines, remote_lines, anchors, hunks, .. } => {
                    if idx >= anchors.len() { return Ok(()); }
                    anchors.remove(idx);
                    *hunks = recompute_three_way(&engine, base_lines, local_lines, remote_lines, anchors, &options)?;
                    Ok(())
                }
            }
        })
    }

    pub fn set_engine(&self, id: SessionId, engine: String) -> Result<(), SessionError> {
        // Validate first
        let _ = build_engine(&engine)?;
        self.with(id, |s| {
            s.engine = engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay { a_lines, b_lines, anchors, hunks, .. } => {
                    *hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                }
                SessionMode::ThreeWay { base_lines, local_lines, remote_lines, anchors, hunks, .. } => {
                    *hunks = recompute_three_way(&engine, base_lines, local_lines, remote_lines, anchors, &options)?;
                }
            }
            Ok(())
        })
    }

    pub fn set_options(&self, id: SessionId, options: DiffOptions) -> Result<(), SessionError> {
        self.with(id, |s| {
            s.options = options;
            let engine = s.engine.clone();
            match &mut s.mode {
                SessionMode::TwoWay { a_lines, b_lines, anchors, hunks, .. } => {
                    *hunks = recompute_two_way(&engine, a_lines, b_lines, anchors, &options)?;
                }
                SessionMode::ThreeWay { base_lines, local_lines, remote_lines, anchors, hunks, .. } => {
                    *hunks = recompute_three_way(&engine, base_lines, local_lines, remote_lines, anchors, &options)?;
                }
            }
            Ok(())
        })
    }

    pub fn set_two_way_decision(&self, id: SessionId, hunk_id: u32, decision: HunkDecision) -> Result<(), SessionError> {
        self.with(id, |s| match &mut s.mode {
            SessionMode::TwoWay { decisions, .. } => {
                decisions.insert(hunk_id, decision);
                Ok(())
            }
            _ => Err(SessionError::WrongMode),
        })
    }

    pub fn set_three_way_resolution(&self, id: SessionId, hunk_id: u32, resolution: Resolution) -> Result<(), SessionError> {
        self.with(id, |s| match &mut s.mode {
            SessionMode::ThreeWay { resolutions, .. } => {
                resolutions.insert(hunk_id, resolution);
                Ok(())
            }
            _ => Err(SessionError::WrongMode),
        })
    }

    pub fn update_manual_result(&self, id: SessionId, text: String) -> Result<(), SessionError> {
        self.with(id, |s| { s.manual_result = Some(text); Ok(()) })
    }

    pub fn compute_result(&self, id: SessionId) -> Result<String, SessionError> {
        let snap = self.snapshot(id)?;
        if let Some(t) = snap.manual_result.clone() {
            return Ok(t);
        }
        match snap.mode {
            SessionMode::TwoWay { hunks, decisions, .. } => {
                Ok(apply_two_way_decisions(&hunks, &decisions))
            }
            SessionMode::ThreeWay { hunks, resolutions, .. } => {
                Ok(apply_resolutions(&hunks, &resolutions))
            }
        }
    }
}

/// Apply per-hunk decisions to a 2-way diff to produce the result text.
/// Default if no decision: keep B (the "right" side) for change hunks; equal
/// hunks always keep A (== B).
pub fn apply_two_way_decisions(
    hunks: &[Hunk],
    decisions: &HashMap<u32, HunkDecision>,
) -> String {
    let mut out: Vec<String> = Vec::new();
    for h in hunks {
        let is_equal_hunk = h.ops.iter().all(|o| matches!(o, DiffOp::Equal { .. }));
        if is_equal_hunk {
            for op in &h.ops {
                if let DiffOp::Equal { text, .. } = op {
                    out.push(text.clone());
                }
            }
            continue;
        }
        let dec = decisions.get(&h.id).cloned().unwrap_or(HunkDecision::AcceptB);
        match dec {
            HunkDecision::AcceptA => {
                for op in &h.ops {
                    match op {
                        DiffOp::Equal { text, .. } | DiffOp::Delete { text, .. } => out.push(text.clone()),
                        DiffOp::Insert { .. } => {}
                    }
                }
            }
            HunkDecision::AcceptB => {
                for op in &h.ops {
                    match op {
                        DiffOp::Equal { text, .. } | DiffOp::Insert { text, .. } => out.push(text.clone()),
                        DiffOp::Delete { .. } => {}
                    }
                }
            }
            HunkDecision::Both => {
                for op in &h.ops {
                    match op {
                        DiffOp::Equal { text, .. }
                        | DiffOp::Delete { text, .. }
                        | DiffOp::Insert { text, .. } => out.push(text.clone()),
                    }
                }
            }
            HunkDecision::Neither => {
                for op in &h.ops {
                    if let DiffOp::Equal { text, .. } = op { out.push(text.clone()); }
                }
            }
            HunkDecision::Custom { text } => {
                out.extend(text);
            }
            HunkDecision::PerLine { keep } => {
                for (op, k) in h.ops.iter().zip(keep.iter()) {
                    if !*k { continue; }
                    match op {
                        DiffOp::Equal { text, .. }
                        | DiffOp::Delete { text, .. }
                        | DiffOp::Insert { text, .. } => out.push(text.clone()),
                    }
                }
            }
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_way_default_picks_b() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\nb\nc\n", "a\nB\nc\n", None).unwrap();
        let result = store.compute_result(id).unwrap();
        assert_eq!(result, "a\nB\nc");
    }

    #[test]
    fn two_way_accept_a() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\nb\nc\n", "a\nB\nc\n", None).unwrap();
        let snap = store.snapshot(id).unwrap();
        let change_hunk_id = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.iter().find_map(|h| {
                if h.ops.iter().any(|o| !matches!(o, DiffOp::Equal { .. })) { Some(h.id) } else { None }
            }).unwrap(),
            _ => unreachable!(),
        };
        store.set_two_way_decision(id, change_hunk_id, HunkDecision::AcceptA).unwrap();
        let result = store.compute_result(id).unwrap();
        assert_eq!(result, "a\nb\nc");
    }

    #[test]
    fn manual_result_overrides() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\n", "b\n", None).unwrap();
        store.update_manual_result(id, "custom".into()).unwrap();
        assert_eq!(store.compute_result(id).unwrap(), "custom");
    }

    #[test]
    fn three_way_round_trip() {
        let store = SessionStore::new();
        let id = store.open_three_way("a\nb\nc\n", "a\nL\nc\n", "a\nR\nc\n", None).unwrap();
        let snap = store.snapshot(id).unwrap();
        let conflict_id = match &snap.mode {
            SessionMode::ThreeWay { hunks, .. } => hunks.iter().find_map(|h| match h {
                MergeHunk::Conflict { id, .. } => Some(*id),
                _ => None,
            }).unwrap(),
            _ => unreachable!(),
        };
        store.set_three_way_resolution(id, conflict_id, Resolution::Local).unwrap();
        assert_eq!(store.compute_result(id).unwrap(), "a\nL\nc");
    }
}
