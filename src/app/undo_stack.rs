//! Per-tab undo/redo on top of the `undo` crate.
//!
//! A single `Record<DiffEdit>` per session captures every buffer-modifying
//! operation made through the diff view (whole-side replacements driven by
//! `input_text_multiline` and "Apply A→B" / "B→A" hunk replacements).
//! Result-pane keystrokes are intentionally NOT tracked here — imgui's
//! `input_text_multiline` handles its own undo for text input.

use undo::{Edit, Merged};

use crate::session::{SessionId, SessionMode, SessionStore, SideRef, ThreeWaySide, TwoWaySide};

/// Operations the undo stack knows how to apply / reverse.
#[derive(Clone)]
pub enum DiffEdit {
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
    /// Replace the entire text of one side (2-way `A` or `B`, or 3-way
    /// `Base` / `Local` / `Remote`). Emitted on every `input_text_multiline`
    /// `changed = true` return; consecutive entries on the same
    /// `(session_id, side)` coalesce via `DiffEdit::merge`.
    SetSide {
        session_id: SessionId,
        side: SideRef,
        new_text: String,
        old_text: Option<String>,
    },
}

impl Edit for DiffEdit {
    type Target = SessionStore;
    type Output = ();

    fn edit(&mut self, store: &mut SessionStore) {
        match self {
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
            DiffEdit::SetSide { session_id, side, new_text, old_text } => {
                if old_text.is_none() {
                    if let Ok(snap) = store.snapshot(*session_id) {
                        *old_text = current_side_text(&snap.mode, *side);
                    }
                }
                let _ = store.set_side_text(*session_id, *side, new_text.clone());
            }
        }
    }

    fn undo(&mut self, store: &mut SessionStore) {
        match self {
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
            DiffEdit::SetSide { session_id, side, old_text, .. } => {
                if let Some(old) = old_text.clone() {
                    let _ = store.set_side_text(*session_id, *side, old);
                }
            }
        }
    }

    /// Coalesce successive same-side `SetSide` edits into a single undo
    /// entry so that typing into a pane doesn't produce one entry per
    /// keystroke. The merged edit keeps the FIRST edit's `old_text` (so
    /// undo reverts the entire typing run) and adopts the latest
    /// `new_text`.
    fn merge(&mut self, other: Self) -> Merged<Self>
    where
        Self: Sized,
    {
        match (self, other) {
            (
                DiffEdit::SetSide { session_id: a_sid, side: a_side, new_text: a_new, .. },
                DiffEdit::SetSide { session_id: b_sid, side: b_side, new_text: b_new, .. },
            ) if *a_sid == b_sid && *a_side == b_side => {
                *a_new = b_new;
                Merged::Yes
            }
            (_, other) => Merged::No(other),
        }
    }
}

fn current_side_text(mode: &SessionMode, side: SideRef) -> Option<String> {
    match (mode, side) {
        (SessionMode::TwoWay { a_text, .. }, SideRef::TwoWay(TwoWaySide::A)) => Some(a_text.clone()),
        (SessionMode::TwoWay { b_text, .. }, SideRef::TwoWay(TwoWaySide::B)) => Some(b_text.clone()),
        (SessionMode::ThreeWay { base_text, .. }, SideRef::ThreeWay(ThreeWaySide::Base)) => Some(base_text.clone()),
        (SessionMode::ThreeWay { local_text, .. }, SideRef::ThreeWay(ThreeWaySide::Local)) => Some(local_text.clone()),
        (SessionMode::ThreeWay { remote_text, .. }, SideRef::ThreeWay(ThreeWaySide::Remote)) => Some(remote_text.clone()),
        _ => None,
    }
}

pub type Stack = undo::Record<DiffEdit>;

#[cfg(test)]
mod set_side_tests {
    use super::*;
    use crate::session::{SessionStore, SideRef, TwoWaySide};
    use undo::Record;

    #[test]
    fn set_side_edit_then_undo_round_trips() {
        let store = SessionStore::new();
        let id = store.open_two_way("hello\n", "hello\n", None).unwrap();
        let mut rec: Record<DiffEdit> = Record::new();
        let mut store = store;
        rec.edit(&mut store, DiffEdit::SetSide {
            session_id: id,
            side: SideRef::TwoWay(TwoWaySide::A),
            new_text: "world".into(),
            old_text: None,
        });
        let snap = store.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "world");
        rec.undo(&mut store);
        let snap = store.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "hello");
    }

    #[test]
    fn consecutive_set_side_same_side_coalesce() {
        let store = SessionStore::new();
        let id = store.open_two_way("a\n", "a\n", None).unwrap();
        let mut rec: Record<DiffEdit> = Record::new();
        let mut store = store;
        for new in ["b", "bc", "bcd"] {
            rec.edit(&mut store, DiffEdit::SetSide {
                session_id: id,
                side: SideRef::TwoWay(TwoWaySide::A),
                new_text: new.into(),
                old_text: None,
            });
        }
        // One undo reverts back to "a", not stepping through "bc" and "b".
        rec.undo(&mut store);
        let snap = store.snapshot(id).unwrap();
        let crate::session::SessionMode::TwoWay { a_text, .. } = &snap.mode else { panic!() };
        assert_eq!(a_text, "a");
        // Confirm no second undo to step through is available.
        assert!(!rec.can_undo());
    }
}
