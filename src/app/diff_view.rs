//! 2-way diff view.
//!
//! Side-by-side virtualized panes, a bezier-ribbon connector strip, inline
//! per-hunk decision buttons, center-anchored scroll sync, and click-to-anchor
//! line correspondence. Pending: char-level highlights (step 9).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use imgui::{ListClipper, StyleVar, Ui};

use super::char_diff::{char_diff, left_segments, right_segments, Segment};
use crate::diff::{Anchor, DiffOp, Hunk};
use crate::session::{HunkDecision, SessionId, SessionStore};

pub const ROW_H: f32 = 20.0;

const CONNECTOR_W: f32 = 60.0;

/// Per-session view state that must persist across frames.
#[derive(Default)]
pub struct DiffViewState {
    last_left: f32,
    last_right: f32,
    written_left: Option<f32>,
    written_right: Option<f32>,
    pending_left: Option<f32>,
    pending_right: Option<f32>,
    /// Two-click anchor creation: line picked on side A awaiting partner on B.
    pending_a: Option<u32>,
    pending_b: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
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
    segments: Vec<Segment>,
    cls: Cls,
}

#[derive(Clone)]
enum Entry {
    Control { hunk_id: u32 },
    ControlPlaceholder,
    Row(Row),
}

struct Pane {
    entries: Vec<Entry>,
    /// (hunk_id, top_y, bot_y) per hunk in content-pixel coordinates.
    ranges: Vec<(u32, f32, f32)>,
    /// Line number → y in content-pixel coordinates.
    line_ys: HashMap<u32, f32>,
}

fn is_change_hunk(h: &Hunk) -> bool {
    h.ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }))
}

fn plain(text: &str) -> Vec<Segment> {
    vec![Segment {
        text: text.to_string(),
        hl: false,
    }]
}

fn build_pane(hunks: &[Hunk], side: Side) -> Pane {
    let mut entries: Vec<Entry> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut line_ys: HashMap<u32, f32> = HashMap::new();
    let mut y: f32 = 0.0;
    for h in hunks {
        let start_y = y;
        if is_change_hunk(h) {
            entries.push(match side {
                Side::Left => Entry::Control { hunk_id: h.id },
                Side::Right => Entry::ControlPlaceholder,
            });
            y += ROW_H;

            // For change hunks, pair deletes with inserts so we can show
            // character-level differences on the paired rows.
            let dels: Vec<(u32, &str)> = h
                .ops
                .iter()
                .filter_map(|op| match op {
                    DiffOp::Delete { a, text } => Some((*a, text.as_str())),
                    _ => None,
                })
                .collect();
            let inss: Vec<(u32, &str)> = h
                .ops
                .iter()
                .filter_map(|op| match op {
                    DiffOp::Insert { b, text } => Some((*b, text.as_str())),
                    _ => None,
                })
                .collect();
            let n_pairs = dels.len().min(inss.len());

            match side {
                Side::Left => {
                    for i in 0..n_pairs {
                        let runs = char_diff(dels[i].1, inss[i].1);
                        let segments = left_segments(&runs);
                        entries.push(Entry::Row(Row {
                            line_no: Some(dels[i].0),
                            segments,
                            cls: Cls::Delete,
                        }));
                        line_ys.insert(dels[i].0, y);
                        y += ROW_H;
                    }
                    for i in n_pairs..dels.len() {
                        entries.push(Entry::Row(Row {
                            line_no: Some(dels[i].0),
                            segments: vec![Segment {
                                text: dels[i].1.to_string(),
                                hl: true,
                            }],
                            cls: Cls::Delete,
                        }));
                        line_ys.insert(dels[i].0, y);
                        y += ROW_H;
                    }
                }
                Side::Right => {
                    for i in 0..n_pairs {
                        let runs = char_diff(dels[i].1, inss[i].1);
                        let segments = right_segments(&runs);
                        entries.push(Entry::Row(Row {
                            line_no: Some(inss[i].0),
                            segments,
                            cls: Cls::Insert,
                        }));
                        line_ys.insert(inss[i].0, y);
                        y += ROW_H;
                    }
                    for i in n_pairs..inss.len() {
                        entries.push(Entry::Row(Row {
                            line_no: Some(inss[i].0),
                            segments: vec![Segment {
                                text: inss[i].1.to_string(),
                                hl: true,
                            }],
                            cls: Cls::Insert,
                        }));
                        line_ys.insert(inss[i].0, y);
                        y += ROW_H;
                    }
                }
            }
        } else {
            // Equal hunks: just mirror text on both sides.
            for op in &h.ops {
                if let DiffOp::Equal { a, b, text } = op {
                    let (line_no, segments) = match side {
                        Side::Left => (*a, plain(text)),
                        Side::Right => (*b, plain(text)),
                    };
                    entries.push(Entry::Row(Row {
                        line_no: Some(line_no),
                        segments,
                        cls: Cls::Equal,
                    }));
                    line_ys.insert(line_no, y);
                    y += ROW_H;
                }
            }
        }
        if y > start_y {
            ranges.push((h.id, start_y, y));
        }
    }
    Pane { entries, ranges, line_ys }
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[Hunk],
    anchors: &[Anchor],
    status: &mut String,
    state: &mut DiffViewState,
) {
    let left = build_pane(hunks, Side::Left);
    let right = build_pane(hunks, Side::Right);
    let anchored_a: HashSet<u32> = anchors.iter().map(|a| a.a).collect();
    let anchored_b: HashSet<u32> = anchors.iter().map(|a| a.b).collect();

    // Click capture: rows accumulate into these cells; we apply after the
    // pane render so we can mutate `state` and call into `store` without
    // overlapping borrows.
    let left_click: Cell<Option<u32>> = Cell::new(None);
    let right_click: Cell<Option<u32>> = Cell::new(None);

    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    let left_scroll = Cell::new(0.0_f32);
    let right_scroll = Cell::new(0.0_f32);
    let left_origin = Cell::new([0.0_f32, 0.0_f32]);
    let right_origin = Cell::new([0.0_f32, 0.0_f32]);
    let left_visible = Cell::new(avail[1]);
    let right_visible = Cell::new(avail[1]);

    let apply_left = state.pending_left.take();
    let apply_right = state.pending_right.take();

    ui.child_window("diffie_left")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| {
            if let Some(y) = apply_left {
                ui.set_scroll_y(y);
                state.written_left = Some(y);
            }
            left_scroll.set(ui.scroll_y());
            left_origin.set(ui.window_pos());
            left_visible.set(ui.content_region_avail()[1]);
            draw_pane(
                ui,
                &left.entries,
                Side::Left,
                store,
                session_id,
                status,
                &anchored_a,
                &left_click,
            );
        });

    ui.same_line_with_spacing(0.0, 0.0);
    let connector_origin = ui.cursor_screen_pos();
    ui.dummy([CONNECTOR_W, avail[1]]);
    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_right")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| {
            if let Some(y) = apply_right {
                ui.set_scroll_y(y);
                state.written_right = Some(y);
            }
            right_scroll.set(ui.scroll_y());
            right_origin.set(ui.window_pos());
            right_visible.set(ui.content_region_avail()[1]);
            draw_pane(
                ui,
                &right.entries,
                Side::Right,
                store,
                session_id,
                status,
                &anchored_b,
                &right_click,
            );
        });

    handle_anchor_clicks(
        store,
        session_id,
        state,
        status,
        left_click.get(),
        right_click.get(),
    );

    let l = left_scroll.get();
    let r = right_scroll.get();
    let l_view = left_visible.get();
    let r_view = right_visible.get();
    sync_scrolls(state, l, r, l_view, r_view, &left.ranges, &right.ranges);

    draw_connector(
        ui,
        connector_origin,
        CONNECTOR_W,
        avail[1],
        left_origin.get()[1],
        right_origin.get()[1],
        l,
        r,
        &left.ranges,
        &right.ranges,
        &left.line_ys,
        &right.line_ys,
        anchors,
        hunks,
    );
}

fn handle_anchor_clicks(
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

fn ribbon_color(is_change: bool) -> [f32; 4] {
    if is_change {
        [0.42, 0.66, 1.0, 0.28]
    } else {
        [0.55, 0.60, 0.70, 0.10]
    }
}

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
    left_line_ys: &HashMap<u32, f32>,
    right_line_ys: &HashMap<u32, f32>,
    anchors: &[Anchor],
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

        // Anchor lines on top: thick black bezier from anchored row's y on
        // each side. Skip if either side is not in the layout (shouldn't
        // happen for a valid anchor).
        for anc in anchors {
            let Some(ly_content) = left_line_ys.get(&anc.a) else {
                continue;
            };
            let Some(ry_content) = right_line_ys.get(&anc.b) else {
                continue;
            };
            let ly = left_top_screen_y + ly_content + ROW_H * 0.5 - left_scroll;
            let ry = right_top_screen_y + ry_content + ROW_H * 0.5 - right_scroll;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            let pts = sample_curve([x_l, ly], [cx, ly], [cx, ry], [x_r, ry]);
            // Render as a thick line by drawing connected segments with a
            // small fill quad. Using add_line for simplicity at thickness 3.
            for i in 0..pts.len() - 1 {
                dl.add_line(pts[i], pts[i + 1], [0.0, 0.0, 0.0, 1.0])
                    .thickness(3.0)
                    .build();
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &Ui,
    entries: &[Entry],
    side: Side,
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
    anchored: &HashSet<u32>,
    click_out: &Cell<Option<u32>>,
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
                Entry::Row(r) => {
                    if let Some(clicked_line) = draw_row(ui, r, side, i, anchored) {
                        click_out.set(Some(clicked_line));
                    }
                }
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

/// Render a single row using invisible_button for hit-testing + draw list for
/// visuals. Returns Some(line_no) if the row was clicked this frame.
fn draw_row(
    ui: &Ui,
    row: &Row,
    side: Side,
    idx: i32,
    anchored: &HashSet<u32>,
) -> Option<u32> {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + ROW_H];

    let id_str = format!("row_{:?}_{idx}", side);
    let clicked = ui.invisible_button(id_str, [row_w, ROW_H]);
    let hovered = ui.is_item_hovered();

    let dl = ui.get_window_draw_list();

    let bg = match row.cls {
        Cls::Equal => None,
        Cls::Delete => Some([0.55, 0.18, 0.18, 0.30]),
        Cls::Insert => Some([0.18, 0.50, 0.22, 0.30]),
    };
    if let Some(bg_rgba) = bg {
        dl.add_rect(p0, p1, bg_rgba).filled(true).build();
    }
    if hovered {
        dl.add_rect(p0, p1, [1.0, 1.0, 1.0, 0.05])
            .filled(true)
            .build();
    }

    let line_text = match row.line_no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    };
    let text_y = p0[1] + 3.0;
    dl.add_text([p0[0] + 4.0, text_y], [0.55, 0.60, 0.70, 1.0], &line_text);

    let fg = match row.cls {
        Cls::Equal => [0.90, 0.92, 0.96, 1.0],
        Cls::Delete => [1.0, 0.65, 0.62, 1.0],
        Cls::Insert => [0.72, 1.0, 0.78, 1.0],
    };
    let hl_bg = match row.cls {
        Cls::Delete => [0.85, 0.18, 0.18, 0.55],
        Cls::Insert => [0.18, 0.70, 0.30, 0.55],
        Cls::Equal => [0.0, 0.0, 0.0, 0.0],
    };
    let mut x = p0[0] + 44.0;
    for seg in &row.segments {
        if seg.text.is_empty() {
            continue;
        }
        let w = ui.calc_text_size(&seg.text)[0];
        if seg.hl {
            dl.add_rect(
                [x, p0[1] + 1.0],
                [x + w, p0[1] + ROW_H - 1.0],
                hl_bg,
            )
            .filled(true)
            .build();
        }
        dl.add_text([x, text_y], fg, &seg.text);
        x += w;
    }

    if let Some(ln) = row.line_no {
        if anchored.contains(&ln) {
            // Black left edge marker.
            dl.add_rect(p0, [p0[0] + 3.0, p1[1]], [0.0, 0.0, 0.0, 1.0])
                .filled(true)
                .build();
        }
    }

    if clicked {
        row.line_no
    } else {
        None
    }
}

const ECHO_TOLERANCE: f32 = 0.5;

fn sync_scrolls(
    state: &mut DiffViewState,
    curr_left: f32,
    curr_right: f32,
    left_view_h: f32,
    right_view_h: f32,
    left_ranges: &[(u32, f32, f32)],
    right_ranges: &[(u32, f32, f32)],
) {
    let l_changed = (curr_left - state.last_left).abs() > ECHO_TOLERANCE;
    let r_changed = (curr_right - state.last_right).abs() > ECHO_TOLERANCE;
    let l_echo = state
        .written_left
        .map_or(false, |w| (curr_left - w).abs() < ECHO_TOLERANCE);
    let r_echo = state
        .written_right
        .map_or(false, |w| (curr_right - w).abs() < ECHO_TOLERANCE);

    if l_changed && !l_echo {
        if let Some(target) =
            target_scroll(curr_left, left_view_h, right_view_h, left_ranges, right_ranges)
        {
            state.pending_right = Some(target);
        }
    } else if r_changed && !r_echo {
        if let Some(target) = target_scroll(
            curr_right,
            right_view_h,
            left_view_h,
            right_ranges,
            left_ranges,
        ) {
            state.pending_left = Some(target);
        }
    }

    state.last_left = curr_left;
    state.last_right = curr_right;
    if l_echo {
        state.written_left = None;
    }
    if r_echo {
        state.written_right = None;
    }
}

fn target_scroll(
    src_scroll: f32,
    src_view_h: f32,
    dst_view_h: f32,
    src_ranges: &[(u32, f32, f32)],
    dst_ranges: &[(u32, f32, f32)],
) -> Option<f32> {
    let center = src_scroll + src_view_h * 0.5;
    let (hunk_id, fraction) = locate_hunk(src_ranges, center)?;
    let (_id, top, bot) = dst_ranges.iter().find(|r| r.0 == hunk_id)?;
    let dst_center = top + fraction * (bot - top);
    Some((dst_center - dst_view_h * 0.5).max(0.0))
}

fn locate_hunk(ranges: &[(u32, f32, f32)], y: f32) -> Option<(u32, f32)> {
    let mut best: Option<(u32, f32, f32)> = None;
    for r in ranges {
        if r.1 <= y && y < r.2 {
            let span = (r.2 - r.1).max(1.0);
            return Some((r.0, ((y - r.1) / span).clamp(0.0, 1.0)));
        }
        if r.1 <= y && best.map_or(true, |b| r.2 > b.2) {
            best = Some(*r);
        }
    }
    best.map(|b| {
        let span = (b.2 - b.1).max(1.0);
        (b.0, ((y - b.1) / span).clamp(0.0, 1.0))
    })
}
