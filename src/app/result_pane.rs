//! Editable result pane.
//!
//! Shows `SessionStore::compute_result` for the active session and lets the
//! user override it. Edits flow back through `update_manual_result`, so the
//! session's `manual_result` field is the source of truth once the user
//! types. When the user is not editing, decision/resolution changes upstream
//! recompute the result and refresh the buffer here.
//!
//! Step 11 uses imgui's built-in `input_text_multiline`. The originally
//! agreed `imgui-text-edit-rs` (ColorTextEdit) would add syntax highlighting
//! and line numbers; see TODO at the end of this file.

use imgui::Ui;

use crate::session::{SessionId, SessionStore};

#[derive(Default)]
pub struct ResultState {
    buffer: String,
    was_active_last_frame: bool,
    initialized: bool,
}

pub fn render(ui: &Ui, store: &SessionStore, session_id: SessionId, state: &mut ResultState) {
    // Sync from the session unless the user is actively typing — otherwise
    // the cursor would jump every time decisions change upstream.
    if !state.was_active_last_frame {
        if let Ok(text) = store.compute_result(session_id) {
            if text != state.buffer {
                state.buffer = text;
            }
        }
        state.initialized = true;
    }

    if !state.initialized {
        ui.text_disabled("Computing…");
        return;
    }

    let avail = ui.content_region_avail();
    let changed = ui
        .input_text_multiline("##diffie_result", &mut state.buffer, avail)
        .build();
    let active = ui.is_item_active();

    if changed {
        let _ = store.update_manual_result(session_id, state.buffer.clone());
    }
    state.was_active_last_frame = active;
}

// TODO(step 11+): consider swapping in `imgui-text-edit-rs` (ColorTextEdit)
// once we've confirmed the crate's API. Wants: line numbers, syntax
// highlighting (Rust/JS/etc.), and inline error markers as we know the
// session's lint state.
