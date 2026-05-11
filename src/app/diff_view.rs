//! 2-way diff view.
//!
//! Two side-by-side child windows with virtualized row rendering. Step 5
//! adds inline per-hunk decision buttons (Accept A/B/Both/Neither). Steps
//! still pending: connector ribbons, char-level highlights, scroll sync.

use imgui::{ListClipper, StyleColor, StyleVar, Ui};

use crate::diff::{DiffOp, Hunk};
use crate::session::{HunkDecision, SessionId, SessionStore};

/// Uniform row height in screen pixels. Buttons in control rows must fit.
pub const ROW_H: f32 = 20.0;

/// Strip reserved between the two panes for the connector (step 6).
const CONNECTOR_W: f32 = 60.0;

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum Cls {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone)]
struct Row {
    line_no: Option<u32>,
    text: String,
    cls: Cls,
}

#[derive(Clone)]
enum Entry {
    /// Control row at the start of a change hunk. Only rendered on the left
    /// side; the right side renders an empty placeholder so y-positions of
    /// matching hunks stay aligned across the two panes.
    Control { hunk_id: u32 },
    ControlPlaceholder,
    Row(Row),
}

fn is_change_hunk(h: &Hunk) -> bool {
    h.ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }))
}

fn build_entries(hunks: &[Hunk], side: Side) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for h in hunks {
        if is_change_hunk(h) {
            entries.push(match side {
                Side::Left => Entry::Control { hunk_id: h.id },
                Side::Right => Entry::ControlPlaceholder,
            });
        }
        for op in &h.ops {
            match (op, side) {
                (DiffOp::Equal { a, text, .. }, Side::Left) => entries.push(Entry::Row(Row {
                    line_no: Some(*a),
                    text: text.clone(),
                    cls: Cls::Equal,
                })),
                (DiffOp::Equal { b, text, .. }, Side::Right) => entries.push(Entry::Row(Row {
                    line_no: Some(*b),
                    text: text.clone(),
                    cls: Cls::Equal,
                })),
                (DiffOp::Delete { a, text }, Side::Left) => entries.push(Entry::Row(Row {
                    line_no: Some(*a),
                    text: text.clone(),
                    cls: Cls::Delete,
                })),
                (DiffOp::Insert { b, text }, Side::Right) => entries.push(Entry::Row(Row {
                    line_no: Some(*b),
                    text: text.clone(),
                    cls: Cls::Insert,
                })),
                _ => {}
            }
        }
    }
    entries
}

pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[Hunk],
    status: &mut String,
) {
    let left = build_entries(hunks, Side::Left);
    let right = build_entries(hunks, Side::Right);
    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    ui.child_window("diffie_left")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| draw_pane(ui, &left, store, session_id, status));

    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_connector")
        .size([CONNECTOR_W, avail[1]])
        .border(true)
        .build(|| {
            // Bezier ribbons + anchor lines land in step 6.
        });

    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_right")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| draw_pane(ui, &right, store, session_id, status));
}

fn draw_pane(
    ui: &Ui,
    entries: &[Entry],
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
) {
    let total = entries.len() as i32;
    if total == 0 {
        return;
    }
    let mut clipper = ListClipper::new(total).items_height(ROW_H).begin(ui);
    while clipper.step() {
        for i in clipper.display_start()..clipper.display_end() {
            match &entries[i as usize] {
                Entry::Control { hunk_id } => draw_control_row(ui, store, session_id, *hunk_id, status),
                Entry::ControlPlaceholder => draw_placeholder(ui),
                Entry::Row(r) => draw_row(ui, r),
            }
        }
    }
}

fn draw_placeholder(ui: &Ui) {
    // Reserve exactly one row of empty space so the right pane mirrors the
    // left pane's control-row position. Use dummy() to advance the layout
    // cursor without rendering anything.
    ui.dummy([0.0, ROW_H]);
}

fn draw_control_row(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    status: &mut String,
) {
    let _bg = ui.push_style_color(StyleColor::ChildBg, [0.15, 0.18, 0.22, 1.0]);
    let _pad = ui.push_style_var(StyleVar::FramePadding([4.0, 1.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([3.0, 0.0]));

    let cursor = ui.cursor_screen_pos();
    let dl = ui.get_window_draw_list();
    let row_w = ui.content_region_avail()[0];
    dl.add_rect(
        [cursor[0], cursor[1]],
        [cursor[0] + row_w, cursor[1] + ROW_H],
        [0.20, 0.24, 0.30, 1.0],
    )
    .filled(true)
    .build();

    if ui.small_button(format!("← A##{hunk_id}_a")) {
        apply_decision(store, session_id, hunk_id, HunkDecision::AcceptA, status);
    }
    ui.same_line();
    if ui.small_button(format!("B →##{hunk_id}_b")) {
        apply_decision(store, session_id, hunk_id, HunkDecision::AcceptB, status);
    }
    ui.same_line();
    if ui.small_button(format!("Both##{hunk_id}_bo")) {
        apply_decision(store, session_id, hunk_id, HunkDecision::Both, status);
    }
    ui.same_line();
    if ui.small_button(format!("None##{hunk_id}_n")) {
        apply_decision(store, session_id, hunk_id, HunkDecision::Neither, status);
    }
}

fn apply_decision(
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    decision: HunkDecision,
    status: &mut String,
) {
    let label = match &decision {
        HunkDecision::AcceptA => "A",
        HunkDecision::AcceptB => "B",
        HunkDecision::Both => "both",
        HunkDecision::Neither => "neither",
        HunkDecision::Custom { .. } => "custom",
        HunkDecision::PerLine { .. } => "per-line",
    };
    match store.set_two_way_decision(session_id, hunk_id, decision) {
        Ok(()) => *status = format!("hunk {hunk_id}: {label}"),
        Err(e) => *status = format!("hunk {hunk_id}: {e}"),
    }
}

fn draw_row(ui: &Ui, row: &Row) {
    let (bg, fg) = match row.cls {
        Cls::Equal => (None, None),
        Cls::Delete => (Some([0.55, 0.18, 0.18, 0.30]), Some([1.0, 0.65, 0.62, 1.0])),
        Cls::Insert => (Some([0.18, 0.50, 0.22, 0.30]), Some([0.72, 1.0, 0.78, 1.0])),
    };
    if let Some(bg_rgba) = bg {
        let cursor = ui.cursor_screen_pos();
        let dl = ui.get_window_draw_list();
        let row_w = ui.content_region_avail()[0];
        dl.add_rect(
            [cursor[0], cursor[1]],
            [cursor[0] + row_w, cursor[1] + ROW_H],
            bg_rgba,
        )
        .filled(true)
        .build();
    }
    let line_text = match row.line_no {
        Some(n) => format!("{n:>4} "),
        None => "     ".to_string(),
    };
    let _line_no_style = ui.push_style_color(StyleColor::Text, [0.55, 0.60, 0.70, 1.0]);
    ui.text(&line_text);
    drop(_line_no_style);
    ui.same_line_with_spacing(0.0, 0.0);
    if let Some(fg_rgba) = fg {
        ui.text_colored(fg_rgba, &row.text);
    } else {
        ui.text(&row.text);
    }
}
