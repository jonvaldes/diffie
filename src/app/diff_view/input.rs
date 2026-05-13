//! Selection / anchor input handling for the 2-way diff view.
//!
//! `update_selection` is the single owner of the cross-row selection state
//! machine driven by the central mouse cursor; `handle_anchor_clicks` folds
//! row-RMB anchor picks into the session store; `build_selection_splice`
//! turns a selection range into a structural `DiffEdit` for Backspace/Delete.

use imgui::Ui;

use super::super::undo_stack::DiffEdit;
use crate::diff::Anchor;
use crate::session::{SessionId, SessionStore, TwoWaySide};

use super::common::{
    gutter_w, ordered_endpoints, row_h, side_from_input, side_to_input, DiffViewState, DragState,
    Pane, Row, SelPoint, Selection, Side,
};

/// Build a `SpliceTwoWayLines` edit that deletes the text covered by `sel`
/// from the corresponding source side. For a multi-line selection the
/// boundary lines' kept-content is merged into a single replacement line, so
/// `Delete` on the selection collapses the lines exactly the way a normal
/// text editor would. Returns `None` if the selection is zero-width or refers
/// to lines that no longer exist in the source.
pub(super) fn build_selection_splice(
    snap: &crate::session::DiffSession,
    sel: &Selection,
    session_id: SessionId,
) -> Option<DiffEdit> {
    let crate::session::SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode else {
        return None;
    };
    let (lo, hi) = ordered_endpoints(sel);
    let source = match sel.side {
        Side::Left => a_lines,
        Side::Right => b_lines,
    };
    let s_idx = lo.line_no.checked_sub(1)? as usize;
    let e_idx = hi.line_no.checked_sub(1)? as usize;
    if s_idx >= source.len() || e_idx >= source.len() {
        return None;
    }
    let first_chars: Vec<char> = source[s_idx].chars().collect();
    let last_chars: Vec<char> = source[e_idx].chars().collect();
    let s_col = lo.col.min(first_chars.len());
    let e_col = hi.col.min(last_chars.len());
    if lo.line_no == hi.line_no && s_col == e_col {
        return None;
    }
    let mut merged = String::new();
    merged.extend(first_chars[..s_col].iter());
    merged.extend(last_chars[e_col..].iter());
    let two_way = match sel.side {
        Side::Left => TwoWaySide::A,
        Side::Right => TwoWaySide::B,
    };
    Some(DiffEdit::SpliceTwoWayLines {
        session_id,
        side: two_way,
        start: s_idx,
        end: e_idx + 1,
        replacement: vec![merged],
        old_target_lines: None,
    })
}

/// Single owner of the selection state machine. Reads frame-global mouse
/// events plus the captured pane geometry and decides what `state.selection`
/// and `state.drag` look like at the end of this frame.
///
/// Transitions:
/// - LMB-just-clicked inside a pane: clear selection (or extend if Shift+click
///   matches existing selection's side) and arm a `DragState`.
/// - LMB-just-clicked outside both panes: clear selection and disarm.
/// - LMB-held with `DragState` armed: once the move exceeds 4 px, materialize
///   the selection. Every subsequent frame re-computes the caret from the
///   current mouse position (clamped to the drag-side pane's visible band).
/// - LMB-released: disarm `DragState` (selection persists).
#[allow(clippy::too_many_arguments)]
pub(super) fn update_selection(
    ui: &Ui,
    state: &mut DiffViewState,
    left: &Pane,
    right: &Pane,
    left_origin: [f32; 2],
    right_origin: [f32; 2],
    left_visible_h: f32,
    right_visible_h: f32,
    pane_w: f32,
    char_w: f32,
    focus_request: &mut Option<crate::app::FocusedPane>,
) {
    use super::super::input::{self, InputFrame};

    if char_w <= 0.0 {
        return;
    }

    let pane_bounds = |side: Side| -> ([f32; 2], f32) {
        match side {
            Side::Left => (left_origin, left_visible_h),
            Side::Right => (right_origin, right_visible_h),
        }
    };
    let rows_for = |side: Side| -> &[Row] {
        match side {
            Side::Left => &left.rows,
            Side::Right => &right.rows,
        }
    };

    // Strict hit-test for the initial press.
    let locate = |pos: [f32; 2]| -> Option<(input::Side, input::SelPoint)> {
        for side in [Side::Left, Side::Right] {
            let (origin, visible_h) = pane_bounds(side);
            if pos[0] < origin[0] || pos[0] >= origin[0] + pane_w { continue; }
            let dy = pos[1] - origin[1];
            if dy < 0.0 || dy >= visible_h { continue; }
            let rows = rows_for(side);
            let row_idx = (dy / row_h()) as usize;
            if row_idx >= rows.len() { continue; }
            let row = &rows[row_idx];
            let line_no = row.line_no?;
            let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();
            let text_x0 = origin[0] + gutter_w();
            let raw = ((pos[0] - text_x0) / char_w).round();
            let col = raw.clamp(0.0, char_count as f32) as usize;
            return Some((side_to_input(side), input::SelPoint { line_no, col: col as u32 }));
        }
        None
    };

    // Clamped locate for the drag tick — preserves the existing behavior
    // where dragging off the pane still extends to the last reachable
    // row/column on the active side.
    let locate_clamped = |side: input::Side, pos: [f32; 2]| -> Option<input::SelPoint> {
        let side = side_from_input(side);
        let (origin, visible_h) = pane_bounds(side);
        let rows = rows_for(side);
        if rows.is_empty() {
            return None;
        }
        let clamped_x = pos[0].clamp(origin[0] + gutter_w(), origin[0] + pane_w - 1.0);
        let clamped_y = pos[1]
            .clamp(origin[1], origin[1] + visible_h - 1.0)
            .max(origin[1]);
        let row_idx = ((clamped_y - origin[1]) / row_h()) as usize;
        let row_idx = row_idx.min(rows.len() - 1);
        let row = &rows[row_idx];
        let line_no = row.line_no?;
        let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();
        let raw = ((clamped_x - (origin[0] + gutter_w())) / char_w).round();
        let col = raw.clamp(0.0, char_count as f32) as usize;
        Some(input::SelPoint { line_no, col: col as u32 })
    };

    let frame = InputFrame::from_ui(ui);

    let prior_sel = state.selection.as_ref().map(|s| input::Selection {
        side: side_to_input(s.side),
        anchor: input::SelPoint { line_no: s.anchor.line_no, col: s.anchor.col as u32 },
        caret: input::SelPoint { line_no: s.caret.line_no, col: s.caret.col as u32 },
    });
    let prior_drag = state.drag.as_ref().map(|d| input::DragState {
        side: side_to_input(d.side),
        anchor: input::SelPoint { line_no: d.anchor.line_no, col: d.anchor.col as u32 },
        press_screen: d.press_screen,
        threshold_passed: d.threshold_passed,
    });

    let step = input::selection_step(&frame, prior_sel, prior_drag, locate, locate_clamped);

    if let Some(new_sel) = step.set_selection {
        state.selection = new_sel.map(|s| Selection {
            side: side_from_input(s.side),
            anchor: SelPoint { line_no: s.anchor.line_no, col: s.anchor.col as usize },
            caret: SelPoint { line_no: s.caret.line_no, col: s.caret.col as usize },
        });
    }
    if let Some(new_drag) = step.set_drag {
        state.drag = new_drag.map(|d| DragState {
            side: side_from_input(d.side),
            anchor: SelPoint { line_no: d.anchor.line_no, col: d.anchor.col as usize },
            press_screen: d.press_screen,
            threshold_passed: d.threshold_passed,
        });
    }
    if let Some(side) = step.focus_request {
        *focus_request = Some(side_from_input(side).as_focused_pane());
    }
}

pub(super) fn handle_anchor_clicks(
    store: &SessionStore,
    session_id: SessionId,
    state: &mut DiffViewState,
    status: &mut String,
    left_click: Option<u32>,
    right_click: Option<u32>,
) {
    if let Some(a) = left_click {
        state.pending_a = Some(a);
        *status = format!("anchor pending: A:{a} → click a row on B");
    }
    if let Some(b) = right_click {
        state.pending_b = Some(b);
        *status = format!("anchor pending: B:{b} → click a row on A");
    }
    if let (Some(a), Some(b)) = (state.pending_a, state.pending_b) {
        match store.add_anchor_two_way(session_id, Anchor { a, b }) {
            Ok(()) => *status = format!("anchor added: A:{a} ↔ B:{b}"),
            Err(e) => *status = format!("anchor error: {e}"),
        }
        state.pending_a = None;
        state.pending_b = None;
    }
}
