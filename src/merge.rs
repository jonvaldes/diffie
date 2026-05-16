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
                    // Unresolved: emit conflict markers so the user sees them in the result.
                    let mut v = vec!["<<<<<<< LOCAL".to_string()];
                    v.extend(local.clone());
                    v.push("||||||| BASE".to_string());
                    v.extend(base.clone());
                    v.push("=======".to_string());
                    v.extend(remote.clone());
                    v.push(">>>>>>> REMOTE".to_string());
                    v
                }
            },
        };
        out.extend(lines);
    }
    out.join("\n")
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
}
