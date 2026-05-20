use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

use serde::{Deserialize, Serialize};

use crate::diff::{anchored::AnchoredDiff, build_engine as build_diff_engine, group_into_hunks, myers::MyersDiff, Anchor, DiffEngine, DiffOp, DiffOptions, Hunk};
use crate::merge::{apply_resolutions, MergeAnchor, MergeHunk, Resolution, ThreeWayMerge};

pub type SessionId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TwoWaySide {
    A,
    B,
}

/// Which side is being addressed in a 3-way diff/merge session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeWaySide {
    Base,
    Local,
    Remote,
}

/// Side reference unifying 2-way and 3-way edits. Used by the new
/// `DiffEdit::SetSide` variant so a single edit type can target any
/// editable pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SideRef {
    TwoWay(TwoWaySide),
    ThreeWay(ThreeWaySide),
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
        a_text: String,
        b_text: String,
        a_trailing_newline: bool,
        b_trailing_newline: bool,
        anchors: Vec<Anchor>,
        hunks: Vec<Hunk>,
        decisions: HashMap<u32, HunkDecision>,
    },
    ThreeWay {
        base_text: String,
        local_text: String,
        remote_text: String,
        base_trailing_newline: bool,
        local_trailing_newline: bool,
        remote_trailing_newline: bool,
        anchors: Vec<MergeAnchor>,
        hunks: Vec<MergeHunk>,
        resolutions: HashMap<u32, Resolution>,
    },
}

/// Split a side's `String` into the `&[&str]` shape the diff engine
/// wants. Empty strings produce one empty line; otherwise splits on
/// `'\n'`. Cheap; do not memoize.
pub(crate) fn lines_of(s: &str) -> Vec<&str> {
    if s.is_empty() {
        vec![""]
    } else {
        s.split('\n').collect()
    }
}

#[derive(Debug, Clone)]
pub struct DiffSession {
    pub id: SessionId,
    pub engine: String,
    pub options: DiffOptions,
    pub mode: SessionMode,
    /// User-edited result buffer (overrides computed result when set).
    pub manual_result: Option<String>,
    pub read_only: bool,
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
    #[error("session is read-only")]
    ReadOnly,
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

/// Return the substring of `text` covering the line range
/// `(start_line..=end_line)`, 1-based, inclusive on both ends.
/// If either endpoint is 0, returns "".
fn extract_line_range(text: &str, range: (u32, u32)) -> String {
    if range.0 == 0 || range.1 == 0 || range.0 > range.1 {
        return String::new();
    }
    let lines: Vec<&str> = lines_of(text);
    let lo = (range.0 as usize).saturating_sub(1).min(lines.len());
    let hi = (range.1 as usize).min(lines.len());
    if lo >= hi {
        return String::new();
    }
    lines[lo..hi].join("\n")
}

/// Replace lines in `text` covering `range` (1-based inclusive) with
/// `replacement`. If range is (0, 0), insert at the end.
fn replace_line_range_in_text(text: &mut String, range: (u32, u32), replacement: &str) {
    if range.0 == 0 || range.1 == 0 {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(replacement);
        return;
    }
    let lines: Vec<&str> = lines_of(text);
    let lo = (range.0 as usize).saturating_sub(1).min(lines.len());
    let hi = (range.1 as usize).min(lines.len());
    let mut out: Vec<&str> = Vec::new();
    out.extend(lines[..lo].iter().copied());
    if !replacement.is_empty() {
        out.extend(replacement.split('\n'));
    }
    out.extend(lines[hi..].iter().copied());
    *text = out.join("\n");
}

fn recompute_two_way(
    engine_name: &str,
    a_text: &str,
    b_text: &str,
    anchors: &[Anchor],
    opts: &DiffOptions,
) -> Result<Vec<Hunk>, SessionError> {
    let inner = build_engine(engine_name)?;
    let caps = inner.capabilities();
    let a_lines_vec = lines_of(a_text);
    let b_lines_vec = lines_of(b_text);
    let a: Vec<&str> = a_lines_vec.iter().copied().collect();
    let b: Vec<&str> = b_lines_vec.iter().copied().collect();
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
    let mut ops = split_trivial_equals(ops);
    crate::diff::sub_line::populate_pair_spans(&mut ops, opts.sub_line);
    if opts.detect_moves && caps.supports_moves {
        crate::diff::moves::detect_moves(&mut ops, opts);
    }
    Ok(group_into_hunks(&ops))
}

/// Reject Myers matches on whitespace-only lines *only* when they sit at
/// a change boundary. Distant blank matches dragging ribbons across the
/// whole pane are the failure mode the original split was guarding
/// against; those matches always show up sandwiched between Delete /
/// Insert ops. A blank with Equal neighbors on both sides is a genuine
/// local match (e.g. when the two buffers really are identical) and
/// must stay Equal — otherwise pasting one pane into the other still
/// reports every blank line as changed.
fn split_trivial_equals(ops: Vec<DiffOp>) -> Vec<DiffOp> {
    let is_equal = |o: &DiffOp| matches!(o, DiffOp::Equal { .. });
    let mut out: Vec<DiffOp> = Vec::with_capacity(ops.len());
    for i in 0..ops.len() {
        let op = ops[i].clone();
        let DiffOp::Equal { a, b, ref text } = op else {
            out.push(op);
            continue;
        };
        if !text.trim().is_empty() {
            out.push(op);
            continue;
        }
        let prev_anchored = i == 0 || is_equal(&ops[i - 1]);
        let next_anchored = i + 1 >= ops.len() || is_equal(&ops[i + 1]);
        if prev_anchored && next_anchored {
            out.push(op);
        } else {
            let text = text.clone();
            out.push(DiffOp::delete(a, text.clone()));
            out.push(DiffOp::insert(b, text));
        }
    }
    out
}

fn recompute_three_way(
    engine_name: &str,
    base_text: &str,
    local_text: &str,
    remote_text: &str,
    anchors: &[MergeAnchor],
    opts: &DiffOptions,
) -> Result<Vec<MergeHunk>, SessionError> {
    let base_vec = lines_of(base_text);
    let local_vec = lines_of(local_text);
    let remote_vec = lines_of(remote_text);
    let base: Vec<&str> = base_vec.iter().copied().collect();
    let local: Vec<&str> = local_vec.iter().copied().collect();
    let remote: Vec<&str> = remote_vec.iter().copied().collect();
    // PROTOTYPE: route all 3-way merges through the merge3 crate, which uses
    // sync-region intersection (bzr/breezy algorithm) instead of our bucket-
    // by-base attribution. Set `DIFFIE_LEGACY_3WAY=1` to fall back to the
    // old ThreeWayMerge<E> path for side-by-side comparison.
    if std::env::var("DIFFIE_LEGACY_3WAY").ok().as_deref() != Some("1") {
        let _ = engine_name; // merge3 has no engine-choice surface (yet)
        return Ok(crate::merge::merge_with_merge3(&base, &local, &remote, anchors, opts));
    }
    match engine_name {
        "myers" => {
            let m = ThreeWayMerge::new(MyersDiff);
            Ok(m.merge(&base, &local, &remote, anchors, opts))
        }
        "patience" => {
            let m = ThreeWayMerge::new(crate::diff::patience::PatienceDiff);
            Ok(m.merge(&base, &local, &remote, anchors, opts))
        }
        "histogram" => {
            let m = ThreeWayMerge::new(crate::diff::histogram::HistogramDiff);
            Ok(m.merge(&base, &local, &remote, anchors, opts))
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
        self.open_two_way_with(
            a_text.trim_end_matches('\n').to_string(),
            b_text.trim_end_matches('\n').to_string(),
            a_text.ends_with('\n'),
            b_text.ends_with('\n'),
            engine,
            DiffOptions::default(),
        )
    }

    pub fn open_two_way_with(
        &self,
        a_text: String,
        b_text: String,
        a_trailing_newline: bool,
        b_trailing_newline: bool,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let hunks = recompute_two_way(&engine, &a_text, &b_text, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::TwoWay {
                a_text,
                b_text,
                a_trailing_newline,
                b_trailing_newline,
                anchors: vec![],
                hunks,
                decisions: HashMap::new(),
            },
            manual_result: None,
            read_only: false,
        };
        self.sessions.lock().unwrap().insert(id, s);
        Ok(id)
    }

    /// Constructs a read-only 2-way session for Swarm-loaded files.
    /// `Binary`/`Empty` sides are stored as empty strings; the GUI layer
    /// overlays a placeholder message based on the per-tab display state.
    pub fn open_two_way_readonly(
        &self,
        a_text: String,
        b_text: String,
        a_trailing_newline: bool,
        b_trailing_newline: bool,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let hunks = recompute_two_way(&engine, &a_text, &b_text, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::TwoWay {
                a_text, b_text,
                a_trailing_newline, b_trailing_newline,
                anchors: vec![],
                hunks,
                decisions: HashMap::new(),
            },
            manual_result: None,
            read_only: true,
        };
        self.sessions.lock().unwrap().insert(id, s);
        Ok(id)
    }

    /// Allocates a SessionId without a backing DiffSession — used for the
    /// Swarm info tab which has no diff state.
    pub fn next_swarm_info_id(&self) -> SessionId { self.alloc_id() }

    pub fn open_three_way(
        &self,
        base_text: &str,
        local_text: &str,
        remote_text: &str,
        engine: Option<String>,
    ) -> Result<SessionId, SessionError> {
        self.open_three_way_with(
            base_text.trim_end_matches('\n').to_string(),
            local_text.trim_end_matches('\n').to_string(),
            remote_text.trim_end_matches('\n').to_string(),
            base_text.ends_with('\n'),
            local_text.ends_with('\n'),
            remote_text.ends_with('\n'),
            engine,
            DiffOptions::default(),
        )
    }

    pub fn open_three_way_with(
        &self,
        base_text: String,
        local_text: String,
        remote_text: String,
        base_trailing_newline: bool,
        local_trailing_newline: bool,
        remote_trailing_newline: bool,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let hunks = recompute_three_way(&engine, &base_text, &local_text, &remote_text, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::ThreeWay {
                base_text,
                local_text,
                remote_text,
                base_trailing_newline,
                local_trailing_newline,
                remote_trailing_newline,
                anchors: vec![],
                hunks,
                resolutions: HashMap::new(),
            },
            manual_result: None,
            read_only: false,
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
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(&id).ok_or(SessionError::UnknownSession(id))?;
        let hunk = {
            let SessionMode::TwoWay { hunks, .. } = &s.mode else {
                return Err(SessionError::WrongMode);
            };
            hunks
                .iter()
                .find(|h| h.id == hunk_id)
                .ok_or(SessionError::WrongMode)?
                .clone()
        };
        let (source_slice, target_range, target_is_b) = {
            let SessionMode::TwoWay { a_text, b_text, .. } = &s.mode else {
                unreachable!()
            };
            match target {
                TwoWaySide::B => (extract_line_range(a_text, hunk.a_range), hunk.b_range, true),
                TwoWaySide::A => (extract_line_range(b_text, hunk.b_range), hunk.a_range, false),
            }
        };
        let SessionMode::TwoWay { a_text, b_text, .. } = &mut s.mode else { unreachable!() };
        if target_is_b {
            replace_line_range_in_text(b_text, target_range, &source_slice);
        } else {
            replace_line_range_in_text(a_text, target_range, &source_slice);
        }
        let engine = s.engine.clone();
        let options = s.options;
        match &mut s.mode {
            SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                *hunks = recompute_two_way(&engine, a_text, b_text, anchors, &options)?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    /// Replace the entire text of one side and recompute hunks. The unified
    /// editor entry point — Task 11 will route all UI edits through this.
    pub fn set_side_text(
        &self,
        id: SessionId,
        side: SideRef,
        new_text: String,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(&id).ok_or(SessionError::UnknownSession(id))?;
        if s.read_only {
            return Err(SessionError::ReadOnly);
        }
        match (&mut s.mode, side) {
            (SessionMode::TwoWay { a_text, .. }, SideRef::TwoWay(TwoWaySide::A)) => *a_text = new_text,
            (SessionMode::TwoWay { b_text, .. }, SideRef::TwoWay(TwoWaySide::B)) => *b_text = new_text,
            (SessionMode::ThreeWay { base_text, .. }, SideRef::ThreeWay(ThreeWaySide::Base)) => *base_text = new_text,
            (SessionMode::ThreeWay { local_text, .. }, SideRef::ThreeWay(ThreeWaySide::Local)) => *local_text = new_text,
            (SessionMode::ThreeWay { remote_text, .. }, SideRef::ThreeWay(ThreeWaySide::Remote)) => *remote_text = new_text,
            _ => return Err(SessionError::WrongMode),
        }
        let engine = s.engine.clone();
        let options = s.options;
        match &mut s.mode {
            SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                *hunks = recompute_two_way(&engine, a_text, b_text, anchors, &options)?;
            }
            SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
                *hunks = recompute_three_way(&engine, base_text, local_text, remote_text, anchors, &options)?;
            }
        }
        Ok(())
    }

    pub fn add_anchor_two_way(&self, id: SessionId, anchor: Anchor) -> Result<(), SessionError> {
        self.with(id, |s| {
            let engine = s.engine.clone();
            let options = s.options;
            match &mut s.mode {
                SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                    let mut new_anchors = anchors.clone();
                    new_anchors.push(anchor);
                    new_anchors.sort_by_key(|a| (a.a, a.b));
                    let new_hunks = recompute_two_way(&engine, a_text, b_text, &new_anchors, &options)?;
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
                SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
                    let mut new_anchors = anchors.clone();
                    new_anchors.push(anchor);
                    new_anchors.sort_by_key(|a| a.base);
                    let new_hunks = recompute_three_way(&engine, base_text, local_text, remote_text, &new_anchors, &options)?;
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
                SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                    if idx >= anchors.len() { return Ok(()); }
                    anchors.remove(idx);
                    *hunks = recompute_two_way(&engine, a_text, b_text, anchors, &options)?;
                    Ok(())
                }
                SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
                    if idx >= anchors.len() { return Ok(()); }
                    anchors.remove(idx);
                    *hunks = recompute_three_way(&engine, base_text, local_text, remote_text, anchors, &options)?;
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
                SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                    *hunks = recompute_two_way(&engine, a_text, b_text, anchors, &options)?;
                }
                SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
                    *hunks = recompute_three_way(&engine, base_text, local_text, remote_text, anchors, &options)?;
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
                SessionMode::TwoWay { a_text, b_text, anchors, hunks, .. } => {
                    *hunks = recompute_two_way(&engine, a_text, b_text, anchors, &options)?;
                }
                SessionMode::ThreeWay { base_text, local_text, remote_text, anchors, hunks, .. } => {
                    *hunks = recompute_three_way(&engine, base_text, local_text, remote_text, anchors, &options)?;
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

    #[test]
    fn set_side_text_no_op_when_read_only() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\n", "b\n", None).unwrap();
        store.with(id, |s| { s.read_only = true; Ok(()) }).unwrap();
        let res = store.set_side_text(id, SideRef::TwoWay(TwoWaySide::A), "changed".into());
        assert!(matches!(res, Err(SessionError::ReadOnly)));
        let snap = store.snapshot(id).unwrap();
        let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
        assert_eq!(a_text, "a");
    }
}
