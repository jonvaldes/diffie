//! Editable result pane.
//!
//! Shows `SessionStore::compute_result` for the active session and lets the
//! user override it. Edits flow back through `update_manual_result`, so the
//! session's `manual_result` field is the source of truth once the user
//! types. When the user is not editing, decision/resolution changes upstream
//! recompute the result and refresh the buffer here.

use imgui::{FontId, Ui};

use crate::session::{SessionId, SessionStore};

#[derive(Default)]
pub struct ResultState {
    buffer: String,
    was_active_last_frame: bool,
    initialized: bool,
}

pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    state: &mut ResultState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
) {
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
    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let changed = ui
        .input_text_multiline("##diffie_result", &mut state.buffer, avail)
        .build();
    let active = ui.is_item_active();
    let focused = ui.is_item_focused();
    drop(_font_tok);

    if active || focused {
        *focus_request = Some(crate::app::FocusedPane::Result);
    }

    if changed {
        let _ = store.update_manual_result(session_id, state.buffer.clone());
    }
    state.was_active_last_frame = active;
}
