//! 2-way diff view.
//!
//! Side-by-side virtualized panes with a connector strip between them that
//! draws filled bezier ribbons linking matching hunks. Pending: char-level
//! highlights, anchor lines/clicks, scroll sync.

use std::cell::Cell;

use imgui::{ListClipper, StyleColor, StyleVar, Ui};

use crate::diff::{DiffOp, Hunk};
use crate::session::{HunkDecision, SessionId, SessionStore};

pub const ROW_H: f32 = 20.0;

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
    Control { hunk_id: u32 },
    ControlPlaceholder,
    Row(Row),
}

#[derive(Clone)]
struct Pane {
    entries: Vec<Entry>,
    /// (hunk_id, top_y, bot_y) per hunk in content-pixel coordinates.
    ranges: Vec<(u32, f32, f32)>,
}

fn is_change_hunk(h: &Hunk) -> bool {
    h.ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }))
}

fn build_pane(hunks: &[Hunk], side: Side) -> Pane {
    let mut entries: Vec<Entry> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut y: f32 = 0.0;
    for h in hunks {
        let start_y = y;
        if is_change_hunk(h) {
            entries.push(match side {
                Side::Left => Entry::Control { hunk_id: h.id },
                Side::Right => Entry::ControlPlaceholder,
            });
            y += ROW_H;
        }
        for op in &h.ops {
            match (op, side) {
                (DiffOp::Equal { a, text, .. }, Side::Left) => {
                    entries.push(Entry::Row(Row {
                        line_no: Some(*a),
                        text: text.clone(),
                        cls: Cls::Equal,
                    }));
                    y += ROW_H;
                }
                (DiffOp::Equal { b, text, .. }, Side::Right) => {
                    entries.push(Entry::Row(Row {
                        line_no: Some(*b),
                        text: text.clone(),
                        cls: Cls::Equal,
                    }));
                    y += ROW_H;
                }
                (DiffOp::Delete { a, text }, Side::Left) => {
                    entries.push(Entry::Row(Row {
                        line_no: Some(*a),
                        text: text.clone(),
                        cls: Cls::Delete,
                    }));
                    y += ROW_H;
                }
                (DiffOp::Insert { b, text }, Side::Right) => {
                    entries.push(Entry::Row(Row {
                        line_no: Some(*b),
                        text: text.clone(),
                        cls: Cls::Insert,
                    }));
                    y += ROW_H;
                }
                _ => {}
            }
        }
        if y > start_y {
            ranges.push((h.id, start_y, y));
        }
    }
    Pane { entries, ranges }
}

pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[Hunk],
    status: &mut String,
) {
    let left = build_pane(hunks, Side::Left);
    let right = build_pane(hunks, Side::Right);
    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    // Snapshot scroll positions of each pane so the connector pass (rendered
    // last) can convert content-y → screen-y for every hunk pair.
    let left_scroll = Cell::new(0.0_f32);
    let right_scroll = Cell::new(0.0_f32);
    let left_origin = Cell::new([0.0_f32, 0.0_f32]);
    let right_origin = Cell::new([0.0_f32, 0.0_f32]);

    ui.child_window("diffie_left")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| {
            left_scroll.set(ui.scroll_y());
            // window_pos is the top-left of this child's content area (after border).
            left_origin.set(ui.window_pos());
            draw_pane(ui, &left.entries, store, session_id, status);
        });

    ui.same_line_with_spacing(0.0, 0.0);

    let connector_origin = ui.cursor_screen_pos();
    ui.dummy([CONNECTOR_W, avail[1]]);

    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_right")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| {
            right_scroll.set(ui.scroll_y());
            right_origin.set(ui.window_pos());
            draw_pane(ui, &right.entries, store, session_id, status);
        });

    draw_connector(
        ui,
        connector_origin,
        CONNECTOR_W,
        avail[1],
        left_origin.get()[1],
        right_origin.get()[1],
        left_scroll.get(),
        right_scroll.get(),
        &left.ranges,
        &right.ranges,
        hunks,
    );
}

fn ribbon_color(is_change: bool) -> [f32; 4] {
    if is_change {
        // Accent blue, semi-transparent.
        [0.42, 0.66, 1.0, 0.28]
    } else {
        // Faint neutral so equal stretches still imply correspondence.
        [0.55, 0.60, 0.70, 0.10]
    }
}

/// Number of segments each bezier curve is tessellated into. Higher = smoother
/// curve at the cost of more triangles per ribbon.
const BEZIER_SEGMENTS: usize = 16;

fn cubic_bezier(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let uu = u * u;
    let uuu = uu * u;
    let tt = t * t;
    let ttt = tt * t;
    [
        uuu * p0[0] + 3.0 * uu * t * p1[0] + 3.0 * u * tt * p2[0] + ttt * p3[0],
        uuu * p0[1] + 3.0 * uu * t * p1[1] + 3.0 * u * tt * p2[1] + ttt * p3[1],
    ]
}

fn sample_curve(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) -> Vec<[f32; 2]> {
    (0..=BEZIER_SEGMENTS)
        .map(|i| cubic_bezier(p0, p1, p2, p3, i as f32 / BEZIER_SEGMENTS as f32))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_connector(
    ui: &Ui,
    origin: [f32; 2],
    w: f32,
    h: f32,
    left_top_screen_y: f32,
    right_top_screen_y: f32,
    left_scroll: f32,
    right_scroll: f32,
    left_ranges: &[(u32, f32, f32)],
    right_ranges: &[(u32, f32, f32)],
    hunks: &[Hunk],
) {
    let dl = ui.get_window_draw_list();
    dl.with_clip_rect_intersect(origin, [origin[0] + w, origin[1] + h], || {
        let x_l = origin[0];
        let x_r = origin[0] + w;
        let cx = origin[0] + w * 0.5;
        let band_top = origin[1];
        let band_bot = origin[1] + h;
        for h_obj in hunks {
            let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let a1 = left_top_screen_y + lr.1 - left_scroll;
            let a2 = left_top_screen_y + lr.2 - left_scroll;
            let b1 = right_top_screen_y + rr.1 - right_scroll;
            let b2 = right_top_screen_y + rr.2 - right_scroll;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            let color = ribbon_color(is_change_hunk(h_obj));
            // Tessellate top + bottom bezier curves, then emit one quad per
            // segment as two triangles. imgui-rs 0.12 does not expose the
            // path_* fill API, so we render the ribbon explicitly.
            let top = sample_curve([x_l, a1], [cx, a1], [cx, b1], [x_r, b1]);
            let bot = sample_curve([x_l, a2], [cx, a2], [cx, b2], [x_r, b2]);
            for i in 0..top.len() - 1 {
                dl.add_triangle(top[i], top[i + 1], bot[i + 1], color)
                    .filled(true)
                    .build();
                dl.add_triangle(top[i], bot[i + 1], bot[i], color)
                    .filled(true)
                    .build();
            }
        }
    });
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
                Entry::Control { hunk_id } => {
                    draw_control_row(ui, store, session_id, *hunk_id, status)
                }
                Entry::ControlPlaceholder => draw_placeholder(ui),
                Entry::Row(r) => draw_row(ui, r),
            }
        }
    }
}

fn draw_placeholder(ui: &Ui) {
    ui.dummy([0.0, ROW_H]);
}

fn draw_control_row(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    status: &mut String,
) {
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
