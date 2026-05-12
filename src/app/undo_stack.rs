//! Per-tab undo/redo on top of the `undo` crate.
//!
//! A single `Record<DiffEdit>` per session captures every buffer-modifying
//! operation made through the diff view (line edits, hunk-side replacements).
//! Result-pane keystrokes are intentionally NOT tracked here — imgui's
//! `input_text_multiline` handles its own undo for text input.

use undo::{Edit, Merged};

use crate::session::{SessionId, SessionMode, SessionStore, TwoWaySide};

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
        /// Snapshot of the whole `target` side's lines BEFORE the replace,
        /// captured inside `edit()`. Restored by `undo()`.
        old_target_lines: Option<Vec<String>>,
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
        old_target_lines: Option<Vec<String>>,
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
                if old_text.is_none() {
                    if let Ok(snap) = store.snapshot(*session_id) {
                        if let SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode {
                            let lines = match side {
                                TwoWaySide::A => a_lines,
                                TwoWaySide::B => b_lines,
                            };
                            let idx = (*line_no as usize).checked_sub(1).unwrap_or(0);
                            if idx < lines.len() {
                                *old_text = Some(lines[idx].clone());
                            }
                        }
                    }
                }
                let _ = store.set_two_way_line(
                    *session_id,
                    *side,
                    *line_no,
                    new_text.clone(),
                );
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                hunk_id,
                target,
                old_target_lines,
            } => {
                if old_target_lines.is_none() {
                    if let Ok(snap) = store.snapshot(*session_id) {
                        if let SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode {
                            let lines = match target {
                                TwoWaySide::A => a_lines,
                                TwoWaySide::B => b_lines,
                            };
                            *old_target_lines = Some(lines.clone());
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
                old_target_lines,
            } => {
                if old_target_lines.is_none() {
                    if let Ok(snap) = store.snapshot(*session_id) {
                        if let SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode {
                            let lines = match side {
                                TwoWaySide::A => a_lines,
                                TwoWaySide::B => b_lines,
                            };
                            *old_target_lines = Some(lines.clone());
                        }
                    }
                }
                let _ = store.splice_two_way_lines(
                    *session_id,
                    *side,
                    *start..*end,
                    replacement.clone(),
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
                if let Some(old) = old_text.clone() {
                    let _ = store.set_two_way_line(*session_id, *side, *line_no, old);
                }
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                target,
                old_target_lines,
                ..
            } => {
                if let Some(old) = old_target_lines.clone() {
                    let _ = store.set_two_way_lines(*session_id, *target, old);
                }
            }
            DiffEdit::SpliceTwoWayLines {
                session_id,
                side,
                old_target_lines,
                ..
            } => {
                if let Some(old) = old_target_lines.clone() {
                    let _ = store.set_two_way_lines(*session_id, *side, old);
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
