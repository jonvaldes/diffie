//! Editable result pane.
//!
//! Shows `SessionStore::compute_result` for the active session and lets the
//! user override it. Edits flow back through `update_manual_result`, so the
//! session's `manual_result` field is the source of truth once the user
//! types. When the user is not editing, decision/resolution changes upstream
//! recompute the result and refresh the buffer here.

use std::collections::HashMap;

use imgui::{FontId, Ui};

use crate::app::theme;
use crate::merge::{hunk_output_ranges, MergeHunk, Resolution};
use crate::session::{SessionId, SessionStore};

const STRIPE_W: f32 = 4.0;

#[derive(Default)]
pub struct ResultState {
    buffer: String,
    was_active_last_frame: bool,
    initialized: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    state: &mut ResultState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
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
    let widget_top_left = ui.cursor_screen_pos();
    let lh = ui.text_line_height();
    let changed = ui
        .input_text_multiline("##diffie_result", &mut state.buffer, avail)
        .build();
    let active = ui.is_item_active();
    let focused = ui.is_item_focused();

    paint_origin_stripes(
        ui,
        widget_top_left,
        avail,
        lh,
        hunks,
        resolutions,
    );

    drop(_font_tok);

    if active || focused {
        *focus_request = Some(crate::app::FocusedPane::Result);
    }

    if changed {
        let _ = store.update_manual_result(session_id, state.buffer.clone());
    }
    state.was_active_last_frame = active;
}

fn paint_origin_stripes(
    ui: &Ui,
    widget_top_left: [f32; 2],
    widget_size: [f32; 2],
    lh: f32,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
) {
    if lh <= 0.0 {
        return;
    }
    let widget_top = widget_top_left[1];
    let widget_bottom = widget_top + widget_size[1];
    let widget_left = widget_top_left[0];
    let dl = ui.get_window_draw_list();
    let ranges = hunk_output_ranges(hunks, resolutions);
    let hunks_by_id: HashMap<u32, &MergeHunk> = hunks.iter().map(|h| (h.id(), h)).collect();
    dl.with_clip_rect(
        [widget_left, widget_top],
        [widget_left + widget_size[0], widget_bottom],
        || {
            for (id, first, last) in ranges {
                let Some(hunk) = hunks_by_id.get(&id) else { continue };
                let Some(color) = stripe_color(hunk, resolutions.get(&id)) else { continue };
                let y0 = widget_top + (first as f32 - 1.0) * lh;
                let y1 = widget_top + (last as f32) * lh;
                if y1 < widget_top || y0 > widget_bottom {
                    continue;
                }
                let y0 = y0.max(widget_top);
                let y1 = y1.min(widget_bottom);
                dl.add_rect(
                    [widget_left, y0],
                    [widget_left + STRIPE_W, y1],
                    color,
                )
                .filled(true)
                .build();
            }
        },
    );
}

/// Stripe color for a hunk's output region, given the current resolution.
/// Returns `None` for stable hunks (no stripe drawn).
pub(crate) fn stripe_color(hunk: &MergeHunk, resolution: Option<&Resolution>) -> Option<[f32; 4]> {
    match hunk {
        MergeHunk::Stable { .. } => None,
        MergeHunk::LocalOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::GREEN(),
        }),
        MergeHunk::RemoteOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::SAPPHIRE(),
        }),
        MergeHunk::Conflict { .. } => Some(match resolution {
            None => theme::RED(),
            Some(Resolution::Local) => theme::GREEN(),
            Some(Resolution::Remote) => theme::SAPPHIRE(),
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::MergeHunk;

    fn stable() -> MergeHunk {
        MergeHunk::Stable { id: 0, base: vec![], text: vec![] }
    }
    fn local_only() -> MergeHunk {
        MergeHunk::LocalOnly { id: 1, base: vec![], local: vec!["L".into()] }
    }
    fn remote_only() -> MergeHunk {
        MergeHunk::RemoteOnly { id: 2, base: vec![], remote: vec!["R".into()] }
    }
    fn conflict() -> MergeHunk {
        MergeHunk::Conflict {
            id: 3, base: vec![], local: vec![], remote: vec![],
        }
    }

    #[test]
    fn stable_has_no_stripe() {
        assert_eq!(stripe_color(&stable(), None), None);
    }

    #[test]
    fn local_only_defaults_to_green() {
        assert_eq!(stripe_color(&local_only(), None), Some(theme::GREEN()));
    }

    #[test]
    fn local_only_base_resolution_is_yellow() {
        assert_eq!(
            stripe_color(&local_only(), Some(&Resolution::Base)),
            Some(theme::YELLOW())
        );
    }

    #[test]
    fn remote_only_defaults_to_sapphire() {
        assert_eq!(stripe_color(&remote_only(), None), Some(theme::SAPPHIRE()));
    }

    #[test]
    fn conflict_unresolved_is_red() {
        assert_eq!(stripe_color(&conflict(), None), Some(theme::RED()));
    }

    #[test]
    fn conflict_resolutions_match_chosen_side() {
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Local)), Some(theme::GREEN()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Remote)), Some(theme::SAPPHIRE()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Base)), Some(theme::YELLOW()));
        assert_eq!(
            stripe_color(&conflict(), Some(&Resolution::Custom { text: vec![] })),
            Some(theme::OVERLAY1())
        );
    }
}
