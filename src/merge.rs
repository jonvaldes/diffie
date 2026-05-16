use serde::{Deserialize, Serialize};

use crate::diff::{anchored::AnchoredDiff, Anchor, DiffEngine, DiffOp, DiffOptions, LineNo};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeAnchor {
    pub base: LineNo,
    pub local: LineNo,
    pub remote: LineNo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeHunk {
    /// `base` is the original base text for the hunk (used to lay out the
    /// BASE pane). `text` is the merged result shown on Local/Remote panes
    /// and used by `apply_resolutions`. For "no change" Stable runs the two
    /// are equal; they diverge when both sides made the same change to base
    /// (e.g., both deleted the same lines).
    Stable { id: u32, base: Vec<String>, text: Vec<String> },
    LocalOnly { id: u32, base: Vec<String>, local: Vec<String> },
    RemoteOnly { id: u32, base: Vec<String>, remote: Vec<String> },
    Conflict { id: u32, base: Vec<String>, local: Vec<String>, remote: Vec<String> },
}

impl MergeHunk {
    pub fn id(&self) -> u32 {
        match self {
            MergeHunk::Stable { id, .. }
            | MergeHunk::LocalOnly { id, .. }
            | MergeHunk::RemoteOnly { id, .. }
            | MergeHunk::Conflict { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Local,
    Remote,
    Base,
    Custom { text: Vec<String> },
}

/// Per-base-line classification of changes from each side.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SideOp {
    Keep(String),                  // base line preserved (text from base)
    Replace(Vec<String>, String),  // base line replaced; (new_lines, old_base_line)
    Delete(String),                // base line removed
}

/// Walk a base→side diff and bucket ops by base line. Inserts that occur
/// before any base line (or after the last) are folded into the adjacent
/// base position as part of the replacement.
fn bucket_by_base(ops: &[DiffOp], base_len: usize) -> Vec<SideOp> {
    // We index buckets by base line 1..=base_len.
    // For each base line we may have: a sequence of Inserts that landed
    // "before" it (between previous base line and this one), and then either
    // an Equal (Keep) or a Delete (Delete). Inserts after the last base line
    // attach to the last base line as a Replace.
    //
    // A cleaner approach: build a pending insert buffer as we scan. When we
    // hit an Equal for base line N, flush pending inserts as a Replace into
    // bucket[N] alongside Keep — but Keep means "no change", and we need to
    // distinguish. So we promote: pending+Equal → Replace (insert lines,
    // then keep base line as last? no — anchor inserts to *previous* base
    // line so they land in the correct hunk).

    let mut buckets: Vec<SideOp> = Vec::with_capacity(base_len);
    // Initialize with placeholders; we'll fill on first touch.
    for _ in 0..base_len {
        buckets.push(SideOp::Keep(String::new())); // placeholder, overwritten
    }
    let mut filled = vec![false; base_len];

    let mut pending_inserts: Vec<String> = Vec::new();
    let mut last_base_index: Option<usize> = None; // 0-based

    for op in ops {
        match op {
            DiffOp::Insert { text, .. } => {
                pending_inserts.push(text.clone());
            }
            DiffOp::Equal { a, text, .. } => {
                let idx = (*a as usize) - 1;
                if !pending_inserts.is_empty() {
                    // Attach inserts to the previous base line (if any) as a Replace,
                    // OR to this base line as part of a Replace alongside the kept text.
                    if let Some(prev) = last_base_index {
                        // Convert prev's Keep/Replace to include these inserts.
                        let prev_op = std::mem::replace(&mut buckets[prev], SideOp::Keep(String::new()));
                        buckets[prev] = match prev_op {
                            SideOp::Keep(orig) => {
                                let mut v = vec![orig.clone()];
                                v.extend(pending_inserts.drain(..));
                                SideOp::Replace(v, orig)
                            }
                            SideOp::Replace(mut v, orig) => {
                                v.extend(pending_inserts.drain(..));
                                SideOp::Replace(v, orig)
                            }
                            SideOp::Delete(orig) => {
                                // Delete + following inserts == replacement of the deleted line
                                let new_lines: Vec<String> = pending_inserts.drain(..).collect();
                                SideOp::Replace(new_lines, orig)
                            }
                        };
                    } else {
                        // No prior base line; attach to current as Replace whose new
                        // lines are inserts followed by the kept text.
                        let mut v = pending_inserts.drain(..).collect::<Vec<_>>();
                        v.push(text.clone());
                        buckets[idx] = SideOp::Replace(v, text.clone());
                        filled[idx] = true;
                        last_base_index = Some(idx);
                        continue;
                    }
                }
                buckets[idx] = SideOp::Keep(text.clone());
                filled[idx] = true;
                last_base_index = Some(idx);
            }
            DiffOp::Delete { a, text, .. } => {
                let idx = (*a as usize) - 1;
                if !pending_inserts.is_empty() {
                    // Inserts immediately followed by a delete: classic replace.
                    let new_lines: Vec<String> = pending_inserts.drain(..).collect();
                    buckets[idx] = SideOp::Replace(new_lines, text.clone());
                } else {
                    buckets[idx] = SideOp::Delete(text.clone());
                }
                filled[idx] = true;
                last_base_index = Some(idx);
            }
        }
    }

    // Trailing inserts: attach to last base line (if any), else this means
    // base was empty — handled by callers.
    if !pending_inserts.is_empty() {
        if let Some(prev) = last_base_index {
            let prev_op = std::mem::replace(&mut buckets[prev], SideOp::Keep(String::new()));
            buckets[prev] = match prev_op {
                SideOp::Keep(orig) => {
                    let mut v = vec![orig.clone()];
                    v.extend(pending_inserts.drain(..));
                    SideOp::Replace(v, orig)
                }
                SideOp::Replace(mut v, orig) => {
                    v.extend(pending_inserts.drain(..));
                    SideOp::Replace(v, orig)
                }
                SideOp::Delete(orig) => {
                    let new_lines: Vec<String> = pending_inserts.drain(..).collect();
                    SideOp::Replace(new_lines, orig)
                }
            };
        }
    }

    debug_assert!(filled.iter().all(|f| *f) || base_len == 0);
    buckets
}

/// 3-way merge built atop a `DiffEngine`. Anchors are applied to both
/// base→local and base→remote diffs so the alignment matches user intent.
pub struct ThreeWayMerge<E: DiffEngine + Clone> {
    pub engine: E,
}

impl<E: DiffEngine + Clone> ThreeWayMerge<E> {
    pub fn new(engine: E) -> Self {
        Self { engine }
    }

    pub fn merge(
        &self,
        base: &[&str],
        local: &[&str],
        remote: &[&str],
        anchors: &[MergeAnchor],
        opts: &DiffOptions,
    ) -> Vec<MergeHunk> {
        let local_anchors: Vec<Anchor> = anchors.iter().map(|a| Anchor { a: a.base, b: a.local }).collect();
        let remote_anchors: Vec<Anchor> = anchors.iter().map(|a| Anchor { a: a.base, b: a.remote }).collect();

        let local_engine = AnchoredDiff::new(self.engine.clone(), local_anchors);
        let remote_engine = AnchoredDiff::new(self.engine.clone(), remote_anchors);

        let local_ops = local_engine.diff(base, local, opts);
        let remote_ops = remote_engine.diff(base, remote, opts);

        let local_buckets = bucket_by_base(&local_ops, base.len());
        let remote_buckets = bucket_by_base(&remote_ops, base.len());

        // Walk by base line, emitting hunks. Group consecutive base lines that
        // share the same "shape" (both stable / one-sided / conflicting) into
        // a single hunk for nicer UI.
        let mut hunks = Vec::new();
        let mut next_id: u32 = 0;
        let mut i = 0;
        while i < base.len() {
            let shape_i = classify(&local_buckets[i], &remote_buckets[i]);
            let mut j = i + 1;
            while j < base.len() && classify(&local_buckets[j], &remote_buckets[j]) == shape_i {
                j += 1;
            }
            let id = next_id; next_id += 1;
            let hunk = build_hunk(id, shape_i, &local_buckets[i..j], &remote_buckets[i..j]);
            hunks.push(hunk);
            i = j;
        }

        hunks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Stable,
    LocalOnly,
    RemoteOnly,
    Conflict,
}

fn classify(local: &SideOp, remote: &SideOp) -> Shape {
    let l_changed = !matches!(local, SideOp::Keep(_));
    let r_changed = !matches!(remote, SideOp::Keep(_));
    match (l_changed, r_changed) {
        (false, false) => Shape::Stable,
        (true, false) => Shape::LocalOnly,
        (false, true) => Shape::RemoteOnly,
        (true, true) => {
            if same_change(local, remote) { Shape::Stable } else { Shape::Conflict }
        }
    }
}

fn same_change(a: &SideOp, b: &SideOp) -> bool {
    match (a, b) {
        (SideOp::Delete(_), SideOp::Delete(_)) => true,
        (SideOp::Replace(av, _), SideOp::Replace(bv, _)) => av == bv,
        _ => false,
    }
}

fn lines_of(op: &SideOp) -> Vec<String> {
    match op {
        SideOp::Keep(t) => vec![t.clone()],
        SideOp::Replace(v, _) => v.clone(),
        SideOp::Delete(_) => vec![],
    }
}

fn base_lines_of(op: &SideOp) -> Vec<String> {
    match op {
        SideOp::Keep(t) | SideOp::Delete(t) => vec![t.clone()],
        SideOp::Replace(_, orig) => vec![orig.clone()],
    }
}

fn build_hunk(id: u32, shape: Shape, local: &[SideOp], remote: &[SideOp]) -> MergeHunk {
    let mut base_text = Vec::new();
    let mut local_text = Vec::new();
    let mut remote_text = Vec::new();
    for (l, r) in local.iter().zip(remote.iter()) {
        base_text.extend(base_lines_of(l));
        local_text.extend(lines_of(l));
        remote_text.extend(lines_of(r));
    }
    match shape {
        // For Stable: if both sides made the same change to base, the resulting
        // text is the changed text (which == local_text == remote_text), not base.
        Shape::Stable => {
            let any_change = local.iter().any(|o| !matches!(o, SideOp::Keep(_)));
            let text = if any_change { local_text } else { base_text.clone() };
            MergeHunk::Stable { id, base: base_text, text }
        }
        Shape::LocalOnly => MergeHunk::LocalOnly { id, base: base_text, local: local_text },
        Shape::RemoteOnly => MergeHunk::RemoteOnly { id, base: base_text, remote: remote_text },
        Shape::Conflict => MergeHunk::Conflict { id, base: base_text, local: local_text, remote: remote_text },
    }
}

/// Apply user resolutions to merge hunks and produce the final text.
/// `resolutions` maps hunk id → chosen `Resolution`. For non-conflict hunks
/// the resolution is implicit (Stable=base, LocalOnly=local, RemoteOnly=remote)
/// unless the user overrides with a Custom.
pub fn apply_resolutions(
    hunks: &[MergeHunk],
    resolutions: &std::collections::HashMap<u32, Resolution>,
) -> String {
    let mut out: Vec<String> = Vec::new();
    for h in hunks {
        let lines: Vec<String> = match h {
            MergeHunk::Stable { id, text, .. } => match resolutions.get(id) {
                Some(Resolution::Custom { text: t }) => t.clone(),
                _ => text.clone(),
            },
            MergeHunk::LocalOnly { id, base, local } => match resolutions.get(id) {
                Some(Resolution::Base) => base.clone(),
                Some(Resolution::Custom { text: t }) => t.clone(),
                _ => local.clone(),
            },
            MergeHunk::RemoteOnly { id, base, remote } => match resolutions.get(id) {
                Some(Resolution::Base) => base.clone(),
                Some(Resolution::Custom { text: t }) => t.clone(),
                _ => remote.clone(),
            },
            MergeHunk::Conflict { id, base, local, remote } => match resolutions.get(id) {
                Some(Resolution::Local) => local.clone(),
                Some(Resolution::Remote) => remote.clone(),
                Some(Resolution::Base) => base.clone(),
                Some(Resolution::Custom { text: t }) => t.clone(),
                None => {
                    // Unresolved: emit local then base then remote, no markers.
                    // The result pane tints each line by its origin so the user
                    // can still distinguish the three groups visually.
                    let mut v = Vec::with_capacity(local.len() + base.len() + remote.len());
                    v.extend(local.clone());
                    v.extend(base.clone());
                    v.extend(remote.clone());
                    v
                }
            },
        };
        out.extend(lines);
    }
    out.join("\n")
}

/// For each hunk, return `(hunk_id, first_line_1based, last_line_1based)` of
/// the lines the hunk will contribute to the merged output, given the current
/// resolutions. Mirrors `apply_resolutions`' line accounting exactly. Hunks
/// that resolve to zero lines are skipped.
pub fn hunk_output_ranges(
    hunks: &[MergeHunk],
    resolutions: &std::collections::HashMap<u32, Resolution>,
) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::with_capacity(hunks.len());
    let mut line_n: u32 = 1;
    for h in hunks {
        let count: u32 = match h {
            MergeHunk::Stable { id, text, .. } => match resolutions.get(id) {
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => text.len() as u32,
            },
            MergeHunk::LocalOnly { id, base, local } => match resolutions.get(id) {
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => local.len() as u32,
            },
            MergeHunk::RemoteOnly { id, base, remote } => match resolutions.get(id) {
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                _ => remote.len() as u32,
            },
            MergeHunk::Conflict { id, base, local, remote } => match resolutions.get(id) {
                Some(Resolution::Local) => local.len() as u32,
                Some(Resolution::Remote) => remote.len() as u32,
                Some(Resolution::Base) => base.len() as u32,
                Some(Resolution::Custom { text: t }) => t.len() as u32,
                None => (local.len() + base.len() + remote.len()) as u32,
            },
        };
        if count == 0 {
            continue;
        }
        let first = line_n;
        let last = line_n + count - 1;
        out.push((h.id(), first, last));
        line_n += count;
    }
    out
}

/// Origin of a single output line in the merged result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    /// Common content shared by all sides — paint with no tint.
    Stable,
    Local,
    Remote,
    Base,
    /// User-supplied text via `Resolution::Custom`.
    Custom,
}

/// One `LineOrigin` per output line of `apply_resolutions`. Length matches
/// `apply_resolutions(...).lines().count()` exactly. Used by the result pane
/// to tint each line by the side it came from.
pub fn result_line_origins(
    hunks: &[MergeHunk],
    resolutions: &std::collections::HashMap<u32, Resolution>,
) -> Vec<LineOrigin> {
    let mut out: Vec<LineOrigin> = Vec::new();
    for h in hunks {
        match h {
            MergeHunk::Stable { id, base: _, text } => {
                let (lines, origin) = match resolutions.get(id) {
                    Some(Resolution::Custom { text: t }) => (t.len(), LineOrigin::Custom),
                    _ => (text.len(), LineOrigin::Stable),
                };
                for _ in 0..lines { out.push(origin); }
            }
            MergeHunk::LocalOnly { id, base, local } => {
                let (lines, origin) = match resolutions.get(id) {
                    Some(Resolution::Base) => (base.len(), LineOrigin::Base),
                    Some(Resolution::Custom { text: t }) => (t.len(), LineOrigin::Custom),
                    _ => (local.len(), LineOrigin::Local),
                };
                for _ in 0..lines { out.push(origin); }
            }
            MergeHunk::RemoteOnly { id, base, remote } => {
                let (lines, origin) = match resolutions.get(id) {
                    Some(Resolution::Base) => (base.len(), LineOrigin::Base),
                    Some(Resolution::Custom { text: t }) => (t.len(), LineOrigin::Custom),
                    _ => (remote.len(), LineOrigin::Remote),
                };
                for _ in 0..lines { out.push(origin); }
            }
            MergeHunk::Conflict { id, base, local, remote } => match resolutions.get(id) {
                Some(Resolution::Local) => for _ in 0..local.len() { out.push(LineOrigin::Local); },
                Some(Resolution::Remote) => for _ in 0..remote.len() { out.push(LineOrigin::Remote); },
                Some(Resolution::Base) => for _ in 0..base.len() { out.push(LineOrigin::Base); },
                Some(Resolution::Custom { text: t }) => for _ in 0..t.len() { out.push(LineOrigin::Custom); },
                None => {
                    for _ in 0..local.len() { out.push(LineOrigin::Local); }
                    for _ in 0..base.len() { out.push(LineOrigin::Base); }
                    for _ in 0..remote.len() { out.push(LineOrigin::Remote); }
                }
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{myers::MyersDiff, split_lines};

    fn opts() -> DiffOptions { DiffOptions::default() }

    #[test]
    fn stable_when_no_changes() {
        let base = split_lines("a\nb\nc\n");
        let m = ThreeWayMerge::new(MyersDiff);
        let hunks = m.merge(&base, &base, &base, &[], &opts());
        assert!(hunks.iter().all(|h| matches!(h, MergeHunk::Stable { .. })));
    }

    #[test]
    fn local_only_change() {
        let base = split_lines("a\nb\nc\n");
        let local = split_lines("a\nB\nc\n");
        let remote = split_lines("a\nb\nc\n");
        let m = ThreeWayMerge::new(MyersDiff);
        let hunks = m.merge(&base, &local, &remote, &[], &opts());
        assert!(hunks.iter().any(|h| matches!(h, MergeHunk::LocalOnly { .. })));

        let mut res = std::collections::HashMap::new();
        let merged = apply_resolutions(&hunks, &res);
        assert_eq!(merged, "a\nB\nc");

        let local_id = hunks.iter().find_map(|h| match h {
            MergeHunk::LocalOnly { id, .. } => Some(*id),
            _ => None,
        }).unwrap();
        res.insert(local_id, Resolution::Base);
        let merged = apply_resolutions(&hunks, &res);
        assert_eq!(merged, "a\nb\nc");
    }

    #[test]
    fn conflict_detection_and_resolution() {
        let base = split_lines("a\nb\nc\n");
        let local = split_lines("a\nL\nc\n");
        let remote = split_lines("a\nR\nc\n");
        let m = ThreeWayMerge::new(MyersDiff);
        let hunks = m.merge(&base, &local, &remote, &[], &opts());
        let conflict_id = hunks.iter().find_map(|h| match h {
            MergeHunk::Conflict { id, .. } => Some(*id),
            _ => None,
        }).expect("expected a conflict");

        let mut res = std::collections::HashMap::new();
        res.insert(conflict_id, Resolution::Local);
        assert_eq!(apply_resolutions(&hunks, &res), "a\nL\nc");

        res.insert(conflict_id, Resolution::Remote);
        assert_eq!(apply_resolutions(&hunks, &res), "a\nR\nc");

        res.insert(conflict_id, Resolution::Custom { text: vec!["X".into()] });
        assert_eq!(apply_resolutions(&hunks, &res), "a\nX\nc");
    }

    #[test]
    fn same_change_on_both_sides_is_stable() {
        let base = split_lines("a\nb\nc\n");
        let same = split_lines("a\nB\nc\n");
        let m = ThreeWayMerge::new(MyersDiff);
        let hunks = m.merge(&base, &same, &same, &[], &opts());
        // Either Stable for the whole thing, or no Conflict hunks.
        assert!(hunks.iter().all(|h| !matches!(h, MergeHunk::Conflict { .. })));
        let merged = apply_resolutions(&hunks, &std::collections::HashMap::new());
        assert_eq!(merged, "a\nB\nc");
    }

    #[test]
    fn hunk_output_ranges_stable_uses_text_lines() {
        let hunks = vec![MergeHunk::Stable {
            id: 0,
            base: vec!["b1".into(), "b2".into()],
            text: vec!["t1".into(), "t2".into(), "t3".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 3)]);
    }

    #[test]
    fn hunk_output_ranges_local_only_default_uses_local() {
        let hunks = vec![MergeHunk::LocalOnly {
            id: 0,
            base: vec!["b".into()],
            local: vec!["L1".into(), "L2".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 2)]);
    }

    #[test]
    fn hunk_output_ranges_local_only_base_resolution_uses_base() {
        let hunks = vec![MergeHunk::LocalOnly {
            id: 7,
            base: vec!["b1".into(), "b2".into(), "b3".into()],
            local: vec!["L".into()],
        }];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(7, Resolution::Base);
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(7, 1, 3)]);
    }

    #[test]
    fn hunk_output_ranges_conflict_unresolved_includes_markers() {
        let hunks = vec![MergeHunk::Conflict {
            id: 2,
            base: vec!["b".into()],
            local: vec!["L".into()],
            remote: vec!["R".into()],
        }];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        // Unresolved conflict emits local + base + remote, no markers: 1+1+1 = 3.
        assert_eq!(ranges, vec![(2, 1, 3)]);
    }

    #[test]
    fn hunk_output_ranges_conflict_resolved_to_local() {
        let hunks = vec![MergeHunk::Conflict {
            id: 2,
            base: vec!["b".into()],
            local: vec!["L1".into(), "L2".into()],
            remote: vec!["R".into()],
        }];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(2, Resolution::Local);
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(2, 1, 2)]);
    }

    #[test]
    fn hunk_output_ranges_skips_zero_line_hunks() {
        // A custom resolution with zero lines: hunk emits nothing, must be skipped.
        let hunks = vec![
            MergeHunk::Stable {
                id: 0,
                base: vec!["b".into()],
                text: vec!["b".into()],
            },
            MergeHunk::Conflict {
                id: 1,
                base: vec!["b".into()],
                local: vec!["L".into()],
                remote: vec!["R".into()],
            },
        ];
        let mut resolutions = std::collections::HashMap::new();
        resolutions.insert(1, Resolution::Custom { text: vec![] });
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        assert_eq!(ranges, vec![(0, 1, 1)]); // hunk 1 skipped
    }

    #[test]
    fn hunk_output_ranges_multiple_hunks_total_matches_apply_resolutions() {
        let hunks = vec![
            MergeHunk::Stable {
                id: 0,
                base: vec!["a".into(), "b".into()],
                text: vec!["a".into(), "b".into()],
            },
            MergeHunk::LocalOnly {
                id: 1,
                base: vec!["c".into()],
                local: vec!["c'".into(), "c''".into()],
            },
            MergeHunk::Stable {
                id: 2,
                base: vec!["d".into()],
                text: vec!["d".into()],
            },
        ];
        let resolutions = std::collections::HashMap::new();
        let ranges = hunk_output_ranges(&hunks, &resolutions);
        let total: u32 = ranges.iter().map(|(_, lo, hi)| hi - lo + 1).sum();
        let out = apply_resolutions(&hunks, &resolutions);
        assert_eq!(total as usize, out.lines().count());
    }
}
