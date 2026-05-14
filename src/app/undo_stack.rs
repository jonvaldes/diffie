//! Per-tab undo/redo on top of the `undo` crate.
//!
//! A single `Record<DiffEdit>` per session captures every buffer-modifying
//! operation made through the diff view (line edits, hunk-side replacements).
//! Result-pane keystrokes are intentionally NOT tracked here — imgui's
//! `input_text_multiline` handles its own undo for text input.

use undo::{Edit, Merged};

use crate::session::{lines_of, SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};

/// Operations the undo stack knows how to apply / reverse.
#[derive(Clone)]
pub enum DiffEdit {
    /// Replace one line of A or B (2-way row editor).
    SetTwoWayLine {
        session_id: SessionId,
        side: TwoWaySide,
        line_no: u32,
        new_text: String,
        /// Snapshot of the line as it was before `edit()` ran. Populated
        /// inside `edit()` the first time it's called; reused for `undo()`.
        old_text: Option<String>,
    },
    /// Replace the entire contents of one side for the given hunk (the
    /// "Apply A → B" / "B → A" overlay buttons).
    ReplaceHunkSide {
        session_id: SessionId,
        hunk_id: u32,
        target: TwoWaySide,
        /// Snapshot of the whole `target` side's text BEFORE the replace,
        /// captured inside `edit()`. Restored by `undo()`.
        old_target_text: Option<String>,
    },
    /// Range-replace within one side's line vector. Used for line deletion
    /// (replacement empty), line insertion (range empty), and selection-
    /// based multi-line deletes.
    SpliceTwoWayLines {
        session_id: SessionId,
        side: TwoWaySide,
        start: usize,
        end: usize,
        replacement: Vec<String>,
        old_target_text: Option<String>,
    },
}

impl Edit for DiffEdit {
    type Target = SessionStore;
    type Output = ();

    fn edit(&mut self, store: &mut SessionStore) {
        match self {
            DiffEdit::SetTwoWayLine {
                session_id,
                side,
                line_no,
                new_text,
                old_text,
            } => {
                let Ok(snap) = store.snapshot(*session_id) else { return };
                let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode else { return };
                let cur_text = match side {
                    TwoWaySide::A => a_text,
                    TwoWaySide::B => b_text,
                };
                let lines = lines_of(cur_text);
                let idx = (*line_no as usize).checked_sub(1).unwrap_or(0);
                if old_text.is_none() {
                    *old_text = Some(lines.get(idx).copied().unwrap_or("").to_string());
                }
                if idx >= lines.len() {
                    return;
                }
                let mut new_lines: Vec<&str> = lines.clone();
                new_lines[idx] = new_text.as_str();
                let new_full = new_lines.join("\n");
                let _ = store.set_side_text(
                    *session_id,
                    SideRef::TwoWay(*side),
                    new_full,
                );
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                hunk_id,
                target,
                old_target_text,
            } => {
                if old_target_text.is_none() {
                    if let Ok(snap) = store.snapshot(*session_id) {
                        if let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode {
                            let text = match target {
                                TwoWaySide::A => a_text,
                                TwoWaySide::B => b_text,
                            };
                            *old_target_text = Some(text.clone());
                        }
                    }
                }
                let _ = store.replace_hunk_side(*session_id, *hunk_id, *target);
            }
            DiffEdit::SpliceTwoWayLines {
                session_id,
                side,
                start,
                end,
                replacement,
                old_target_text,
            } => {
                let Ok(snap) = store.snapshot(*session_id) else { return };
                let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode else { return };
                let cur_text = match side {
                    TwoWaySide::A => a_text,
                    TwoWaySide::B => b_text,
                };
                if old_target_text.is_none() {
                    *old_target_text = Some(cur_text.clone());
                }
                let lines = lines_of(cur_text);
                let s = (*start).min(lines.len());
                let e = (*end).min(lines.len()).max(s);
                let mut out: Vec<String> = Vec::new();
                out.extend(lines[..s].iter().map(|x| x.to_string()));
                out.extend(replacement.iter().cloned());
                out.extend(lines[e..].iter().map(|x| x.to_string()));
                let new_full = out.join("\n");
                let _ = store.set_side_text(
                    *session_id,
                    SideRef::TwoWay(*side),
                    new_full,
                );
            }
        }
    }

    fn undo(&mut self, store: &mut SessionStore) {
        match self {
            DiffEdit::SetTwoWayLine {
                session_id,
                side,
                line_no,
                old_text,
                ..
            } => {
                let Some(old) = old_text.clone() else { return };
                let Ok(snap) = store.snapshot(*session_id) else { return };
                let SessionMode::TwoWay { a_text, b_text, .. } = &snap.mode else { return };
                let cur_text = match side {
                    TwoWaySide::A => a_text,
                    TwoWaySide::B => b_text,
                };
                let lines = lines_of(cur_text);
                let idx = (*line_no as usize).checked_sub(1).unwrap_or(0);
                if idx >= lines.len() {
                    return;
                }
                let mut new_lines: Vec<&str> = lines.clone();
                new_lines[idx] = old.as_str();
                let new_full = new_lines.join("\n");
                let _ = store.set_side_text(
                    *session_id,
                    SideRef::TwoWay(*side),
                    new_full,
                );
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                target,
                old_target_text,
                ..
            } => {
                if let Some(old) = old_target_text.clone() {
                    let _ = store.set_side_text(*session_id, SideRef::TwoWay(*target), old);
                }
            }
            DiffEdit::SpliceTwoWayLines {
                session_id,
                side,
                old_target_text,
                ..
            } => {
                if let Some(old) = old_target_text.clone() {
                    let _ = store.set_side_text(*session_id, SideRef::TwoWay(*side), old);
                }
            }
        }
    }

    /// Coalesce successive same-line edits into a single undo entry so that
    /// typing into a row doesn't produce one entry per keystroke. The merged
    /// edit keeps the FIRST edit's `old_text` (so undo reverts the entire
    /// typing run) and adopts the latest `new_text`.
    fn merge(&mut self, other: Self) -> Merged<Self>
    where
        Self: Sized,
    {
        match (self, other) {
            (
                DiffEdit::SetTwoWayLine {
                    session_id: a_sid,
                    side: a_side,
                    line_no: a_line,
                    new_text: a_new,
                    ..
                },
                DiffEdit::SetTwoWayLine {
                    session_id: b_sid,
                    side: b_side,
                    line_no: b_line,
                    new_text: b_new,
                    ..
                },
            ) if *a_sid == b_sid && *a_side == b_side && *a_line == b_line => {
                *a_new = b_new;
                Merged::Yes
            }
            (_, other) => Merged::No(other),
        }
    }
}

pub type Stack = undo::Record<DiffEdit>;
