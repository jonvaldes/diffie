//! 2-way diff view.
//!
//! Side-by-side virtualized panes, a bezier-ribbon connector strip, inline
//! per-hunk decision buttons, center-anchored scroll sync, and click-to-anchor
//! line correspondence. Pending: char-level highlights (step 9).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use imgui::{FontId, ListClipper, StyleVar, Ui};

use super::char_diff::{char_diff, left_segments, right_segments, Segment};
use crate::diff::{Anchor, DiffOp, Hunk};
use crate::session::{SessionId, SessionStore, TwoWaySide};

/// Tall enough for the 1.5x Roboto Mono used in code rows at zoom=1.0.
const ROW_H_BASE: f32 = 24.0;
/// Width of the line-number gutter, sized for ~4 digits in the code-row mono.
const GUTTER_W_BASE: f32 = 60.0;

const CONNECTOR_W: f32 = 60.0;

fn row_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}

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
    /// Active text selection. `side` is the pane the anchor was set in; the
    /// selection is always confined to that one pane.
    pub selection: Option<Selection>,
    /// In-place row editor. While `Some`, the corresponding row is rendered
    /// as an `input_text` widget; Enter commits, Escape cancels.
    pub editing: Option<EditState>,
}

#[derive(Clone)]
pub struct EditState {
    pub side: Side,
    pub row_idx: usize,
    pub line_no: u32,
    pub buffer: String,
    /// First-frame flag so we only `set_keyboard_focus_here` once when the
    /// editor becomes active.
    pub just_started: bool,
}

#[derive(Clone)]
pub struct Selection {
    pub side: Side,
    pub anchor: (usize, usize),
    pub caret: (usize, usize),
    pub dragging: bool,
}

pub fn normalize_selection(sel: &Selection) -> (usize, usize, usize, usize) {
    let (a, b) = (sel.anchor, sel.caret);
    if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
        (a.0, a.1, b.0, b.1)
    } else {
        (b.0, b.1, a.0, a.1)
    }
}

impl Side {
    pub fn as_focused_pane(self) -> crate::app::FocusedPane {
        match self {
            Side::Left => crate::app::FocusedPane::TwoWayA,
            Side::Right => crate::app::FocusedPane::TwoWayB,
        }
    }
}

/// Build the pane's text for `sel.side` and slice out the selected range.
/// Returns an empty string if the session isn't 2-way or the selection
/// references rows that no longer exist (e.g. after a hunk recompute).
pub fn extract_selection_text(snap: &crate::session::DiffSession, sel: &Selection) -> String {
    let crate::session::SessionMode::TwoWay { hunks, .. } = &snap.mode else {
        return String::new();
    };
    let pane = build_pane(hunks, sel.side);
    if pane.rows.is_empty() {
        return String::new();
    }
    let (s_row, s_col, e_row, e_col) = normalize_selection(sel);
    let last = pane.rows.len() - 1;
    let s_row = s_row.min(last);
    let e_row = e_row.min(last);
    let mut out = String::new();
    for r in s_row..=e_row {
        let row = &pane.rows[r];
        let line: String = row.segments.iter().map(|s| s.text.as_str()).collect();
        let chars: Vec<char> = line.chars().collect();
        let l = if r == s_row { s_col } else { 0 }.min(chars.len());
        let h = if r == e_row { e_col } else { chars.len() }.min(chars.len());
        out.extend(chars[l..h].iter());
        if r < e_row {
            out.push('\n');
        }
    }
    out
}

/// Select all rows on `side` for the active diff session. Returns a
/// `Selection` the caller can drop into `DiffViewState.selection`.
pub fn select_all(snap: &crate::session::DiffSession, side: Side) -> Option<Selection> {
    let crate::session::SessionMode::TwoWay { hunks, .. } = &snap.mode else {
        return None;
    };
    let pane = build_pane(hunks, side);
    if pane.rows.is_empty() {
        return None;
    }
    let last_idx = pane.rows.len() - 1;
    let last_chars: usize = pane.rows[last_idx]
        .segments
        .iter()
        .map(|s| s.text.chars().count())
        .sum();
    Some(Selection {
        side,
        anchor: (0, 0),
        caret: (last_idx, last_chars),
        dragging: false,
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
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
    /// Hunk this row belongs to. Used so the hover overlay knows which hunk
    /// the user is interacting with without re-scanning the list.
    hunk_id: u32,
    /// True iff this row sits inside a change hunk (i.e., a hunk the
    /// decision buttons can act on).
    is_change: bool,
    /// Index of the first row of `hunk_id` inside the pane's `rows` Vec.
    /// Used to position the hover overlay at the top of the hunk rather
    /// than at the cursor.
    hunk_first_row: usize,
}

struct Pane {
    rows: Vec<Row>,
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
    let mut rows: Vec<Row> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut line_ys: HashMap<u32, f32> = HashMap::new();
    let mut y: f32 = 0.0;
    for h in hunks {
        let start_y = y;
        let hunk_first_row = rows.len();
        let is_change = is_change_hunk(h);
        if is_change {
            // Pair deletes with inserts to drive character-level highlights.
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
                        rows.push(Row {
                            line_no: Some(dels[i].0),
                            segments,
                            cls: Cls::Delete,
                            hunk_id: h.id,
                            is_change: true,
                            hunk_first_row,
                        });
                        line_ys.insert(dels[i].0, y);
                        y += row_h();
                    }
                    for i in n_pairs..dels.len() {
                        rows.push(Row {
                            line_no: Some(dels[i].0),
                            segments: vec![Segment {
                                text: dels[i].1.to_string(),
                                hl: true,
                            }],
                            cls: Cls::Delete,
                            hunk_id: h.id,
                            is_change: true,
                            hunk_first_row,
                        });
                        line_ys.insert(dels[i].0, y);
                        y += row_h();
                    }
                }
                Side::Right => {
                    for i in 0..n_pairs {
                        let runs = char_diff(dels[i].1, inss[i].1);
                        let segments = right_segments(&runs);
                        rows.push(Row {
                            line_no: Some(inss[i].0),
                            segments,
                            cls: Cls::Insert,
                            hunk_id: h.id,
                            is_change: true,
                            hunk_first_row,
                        });
                        line_ys.insert(inss[i].0, y);
                        y += row_h();
                    }
                    for i in n_pairs..inss.len() {
                        rows.push(Row {
                            line_no: Some(inss[i].0),
                            segments: vec![Segment {
                                text: inss[i].1.to_string(),
                                hl: true,
                            }],
                            cls: Cls::Insert,
                            hunk_id: h.id,
                            is_change: true,
                            hunk_first_row,
                        });
                        line_ys.insert(inss[i].0, y);
                        y += row_h();
                    }
                }
            }
        } else {
            for op in &h.ops {
                if let DiffOp::Equal { a, b, text } = op {
                    let (line_no, segments) = match side {
                        Side::Left => (*a, plain(text)),
                        Side::Right => (*b, plain(text)),
                    };
                    rows.push(Row {
                        line_no: Some(line_no),
                        segments,
                        cls: Cls::Equal,
                        hunk_id: h.id,
                        is_change: false,
                        hunk_first_row,
                    });
                    line_ys.insert(line_no, y);
                    y += row_h();
                }
            }
        }
        if y > start_y {
            ranges.push((h.id, start_y, y));
        }
    }
    Pane { rows, ranges, line_ys }
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
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
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

    // Pane focus event from selection mouse-down. The right pane writes
    // here too — last wins, matching imgui's standard last-clicked focus.
    let focus_event: Cell<Option<crate::app::FocusedPane>> = Cell::new(None);
    // (side, line_no, new_text) the row editor just produced.
    let edit_commit: Cell<Option<(Side, u32, String)>> = Cell::new(None);

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
            left_origin.set(ui.cursor_screen_pos());
            left_visible.set(ui.content_region_avail()[1]);
            draw_pane(
                ui,
                &left.rows,
                Side::Left,
                store,
                session_id,
                status,
                &anchored_a,
                &left_click,
                mono_font,
                &mut state.selection,
                &mut state.editing,
                &focus_event,
                &edit_commit,
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
            right_origin.set(ui.cursor_screen_pos());
            right_visible.set(ui.content_region_avail()[1]);
            draw_pane(
                ui,
                &right.rows,
                Side::Right,
                store,
                session_id,
                status,
                &anchored_b,
                &right_click,
                mono_font,
                &mut state.selection,
                &mut state.editing,
                &focus_event,
                &edit_commit,
            );
        });

    if let Some(p) = focus_event.get() {
        *focus_request = Some(p);
    }
    // Clear drag flag once the mouse button is released anywhere.
    if !ui.is_mouse_down(imgui::MouseButton::Left) {
        if let Some(sel) = state.selection.as_mut() {
            sel.dragging = false;
        }
    }
    // Apply any in-place row edit by writing back to the session's A/B
    // file lines. The diff hunks get recomputed inside SessionStore.
    if let Some((side, line_no, text)) = edit_commit.take() {
        let side = match side {
            Side::Left => crate::session::TwoWaySide::A,
            Side::Right => crate::session::TwoWaySide::B,
        };
        if let Err(e) = store.set_two_way_line(session_id, side, line_no, text) {
            *status = format!("edit error: {e}");
        }
    }

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

fn pack_color(c: [f32; 4]) -> u32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to8(c[0]) | (to8(c[1]) << 8) | (to8(c[2]) << 16) | (to8(c[3]) << 24)
}

fn v2(x: f32, y: f32) -> imgui::sys::ImVec2 {
    imgui::sys::ImVec2 { x, y }
}

const BEZIER_SEGMENTS: usize = 24;

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

/// Fill an arbitrary (possibly concave) polygon by triangulating it with
/// earcut, then submitting the resulting triangles directly via imgui's
/// low-level primitive API. imgui's own `AddConvexPolyFilled` / `PathFillConvex`
/// fans from vertex 0, which only works for convex shapes; this bypass
/// guarantees correct fill regardless of polygon shape.
fn fill_polygon(pts: &[[f32; 2]], color: [f32; 4]) {
    if pts.len() < 3 {
        return;
    }
    let mut flat: Vec<f32> = Vec::with_capacity(pts.len() * 2);
    for p in pts {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let tris = match earcutr::earcut(&flat, &[], 2) {
        Ok(t) => t,
        Err(_) => return,
    };
    if tris.is_empty() {
        return;
    }
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        let mut uv = imgui::sys::ImVec2 { x: 0.0, y: 0.0 };
        imgui::sys::igGetFontTexUvWhitePixel(&mut uv);
        let vtx_count = pts.len() as i32;
        let idx_count = tris.len() as i32;
        imgui::sys::ImDrawList_PrimReserve(dl, idx_count, vtx_count);
        let base = (*dl)._VtxCurrentIdx;
        for p in pts {
            imgui::sys::ImDrawList_PrimWriteVtx(dl, v2(p[0], p[1]), uv, col);
        }
        for &idx in &tris {
            imgui::sys::ImDrawList_PrimWriteIdx(
                dl,
                (base + idx as u32) as imgui::sys::ImDrawIdx,
            );
        }
    }
}

/// Bezier-bounded ribbon. Samples the top + bottom curves into points, builds
/// a closed outline, and runs that through `fill_polygon` so any concavity is
/// triangulated correctly. The outline is then stroked thinly so the curve
/// edges look smooth (AA only on the boundary).
fn fill_bezier_ribbon(x_l: f32, x_r: f32, a1: f32, a2: f32, b1: f32, b2: f32, color: [f32; 4]) {
    let cx = (x_l + x_r) * 0.5;
    let top = sample_curve([x_l, a1], [cx, a1], [cx, b1], [x_r, b1]);
    let bot = sample_curve([x_l, a2], [cx, a2], [cx, b2], [x_r, b2]);
    // Top forward, then bottom reversed: closed polygon going around the
    // ribbon perimeter.
    let mut outline: Vec<[f32; 2]> = top;
    outline.extend(bot.into_iter().rev());
    fill_polygon(&outline, color);
    // Thin AA stroke along the same outline so the curve edges appear
    // smooth even though the fill itself isn't anti-aliased.
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        for p in &outline {
            imgui::sys::ImDrawList_PathLineTo(dl, v2(p[0], p[1]));
        }
        imgui::sys::ImDrawList_PathStroke(
            dl,
            col,
            imgui::sys::ImDrawFlags_Closed as i32,
            1.0,
        );
    }
}

fn stroke_bezier_curve(
    x_l: f32,
    x_r: f32,
    y1: f32,
    y2: f32,
    color: [f32; 4],
    thickness: f32,
) {
    let cx = (x_l + x_r) * 0.5;
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        imgui::sys::ImDrawList_PathLineTo(dl, v2(x_l, y1));
        imgui::sys::ImDrawList_PathBezierCubicCurveTo(
            dl,
            v2(cx, y1),
            v2(cx, y2),
            v2(x_r, y2),
            0,
        );
        imgui::sys::ImDrawList_PathStroke(dl, col, imgui::sys::ImDrawFlags_None as i32, thickness);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_connector(
    ui: &Ui,
    origin: [f32; 2],
    w: f32,
    h: f32,
    left_origin_y: f32,
    right_origin_y: f32,
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
        let band_top = origin[1];
        let band_bot = origin[1] + h;

        for h_obj in hunks {
            let Some(lr) = left_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == h_obj.id) else {
                continue;
            };
            // left/right_origin_y already account for scroll (captured via
            // cursor_screen_pos inside the scrolling pane), so content-y
            // maps directly to screen-y by addition.
            let a1 = left_origin_y + lr.1;
            let a2 = left_origin_y + lr.2;
            let b1 = right_origin_y + rr.1;
            let b2 = right_origin_y + rr.2;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            let color = ribbon_color(is_change_hunk(h_obj));
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, color);
        }

        for anc in anchors {
            let Some(ly_content) = left_line_ys.get(&anc.a) else {
                continue;
            };
            let Some(ry_content) = right_line_ys.get(&anc.b) else {
                continue;
            };
            let ly = left_origin_y + ly_content + row_h() * 0.5;
            let ry = right_origin_y + ry_content + row_h() * 0.5;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            stroke_bezier_curve(x_l, x_r, ly, ry, [0.0, 0.0, 0.0, 1.0], 3.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &Ui,
    rows: &[Row],
    side: Side,
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
    anchored: &HashSet<u32>,
    click_out: &Cell<Option<u32>>,
    mono_font: Option<FontId>,
    selection: &mut Option<Selection>,
    editing: &mut Option<EditState>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    edit_commit: &Cell<Option<(Side, u32, String)>>,
) {
    let total = rows.len() as i32;
    if total == 0 {
        return;
    }

    // Captured before the clipper / rows render so we have an accurate
    // pane origin (post-scroll screen y of content_y=0) for auto-scroll.
    let pane_origin = ui.cursor_screen_pos();
    let visible_h = ui.content_region_avail()[1];
    let cur_scroll = ui.scroll_y();

    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([0.0, 0.0]));
    let hover: Cell<Option<(u32, [f32; 2])>> = Cell::new(None);
    let mut clipper = ListClipper::new(total).items_height(row_h()).begin(ui);
    while clipper.step() {
        for i in clipper.display_start()..clipper.display_end() {
            let r = &rows[i as usize];
            if let Some(clicked_line) = draw_row(
                ui,
                r,
                side,
                i,
                anchored,
                mono_font,
                &hover,
                selection,
                editing,
                focus_event,
                edit_commit,
            ) {
                click_out.set(Some(clicked_line));
            }
        }
    }
    drop(_spacing);

    // Drag auto-scroll: while LMB is held and the mouse is past the pane's
    // visible band, scroll proportionally and extend the caret to the new
    // boundary so the selection keeps tracking the mouse.
    if let Some(sel) = selection.as_mut() {
        if sel.dragging
            && sel.side == side
            && ui.is_mouse_down(imgui::MouseButton::Left)
        {
            let mouse_y = ui.io().mouse_pos[1];
            let pane_top = pane_origin[1] + cur_scroll;
            let pane_bot = pane_top + visible_h;
            let max_scroll = (rows.len() as f32 * row_h() - visible_h).max(0.0);
            let new_scroll = if mouse_y < pane_top {
                let dist = (pane_top - mouse_y).min(160.0);
                let speed = 8.0 + dist * 0.5;
                Some((cur_scroll - speed).max(0.0))
            } else if mouse_y > pane_bot {
                let dist = (mouse_y - pane_bot).min(160.0);
                let speed = 8.0 + dist * 0.5;
                Some((cur_scroll + speed).min(max_scroll))
            } else {
                None
            };
            if let Some(s) = new_scroll {
                ui.set_scroll_y(s);
                // Snap the caret to the row that will be at the relevant edge
                // after scrolling, with column 0 going up and end-of-line
                // going down — mirrors how text editors extend selection
                // during edge-drag.
                if mouse_y < pane_top {
                    let row_idx = ((s / row_h()) as usize).min(rows.len().saturating_sub(1));
                    sel.caret = (row_idx, 0);
                } else {
                    let bot_content = s + visible_h;
                    let row_idx = ((bot_content / row_h()) as usize)
                        .saturating_sub(1)
                        .min(rows.len().saturating_sub(1));
                    let last_col: usize = rows[row_idx]
                        .segments
                        .iter()
                        .map(|seg| seg.text.chars().count())
                        .sum();
                    sel.caret = (row_idx, last_col);
                }
            }
        }
    }

    if let Some((hunk_id, pos)) = hover.get() {
        draw_control_overlay(ui, store, session_id, hunk_id, status, pos);
    }
}

/// Floating panel with the four decision buttons, rendered on top of the
/// hovered row. Takes no space in the row layout because it sets the cursor
/// to an absolute screen position and we ignore the cursor advance afterwards.
fn draw_control_overlay(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    status: &mut String,
    pos: [f32; 2],
) {
    let _pad = ui.push_style_var(StyleVar::FramePadding([6.0, 2.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([4.0, 0.0]));

    let panel_x = pos[0] + 4.0;
    let panel_y = pos[1] + 2.0;
    let panel_w = 200.0;
    let panel_h = row_h() - 4.0;

    let dl = ui.get_window_draw_list();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        [0.10, 0.13, 0.18, 0.95],
    )
    .filled(true)
    .rounding(4.0)
    .build();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        [0.42, 0.66, 1.0, 1.0],
    )
    .rounding(4.0)
    .thickness(1.0)
    .build();

    ui.set_cursor_screen_pos([panel_x + 6.0, panel_y + 3.0]);
    // 2-way edit mode: these copy this hunk's content from one side to the
    // other, directly modifying the underlying file lines.
    if ui.small_button(format!("Apply A → B##ov{hunk_id}_atob")) {
        apply_replace(store, session_id, hunk_id, TwoWaySide::B, status);
    }
    ui.same_line();
    if ui.small_button(format!("B → A##ov{hunk_id}_btoa")) {
        apply_replace(store, session_id, hunk_id, TwoWaySide::A, status);
    }
}

fn apply_replace(
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    target: TwoWaySide,
    status: &mut String,
) {
    let label = match target {
        TwoWaySide::A => "A ← B",
        TwoWaySide::B => "A → B",
    };
    match store.replace_hunk_side(session_id, hunk_id, target) {
        Ok(()) => *status = format!("hunk {hunk_id}: {label}"),
        Err(e) => *status = format!("hunk {hunk_id}: {e}"),
    }
}

/// Render a single row using invisible_button for hit-testing + draw list for
/// visuals. Returns Some(line_no) if the row was right-clicked this frame
/// (anchor pick). LMB drives selection; double-click switches the row into
/// an inline editor.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    row: &Row,
    side: Side,
    idx: i32,
    anchored: &HashSet<u32>,
    mono_font: Option<FontId>,
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
    selection: &mut Option<Selection>,
    editing: &mut Option<EditState>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    edit_commit: &Cell<Option<(Side, u32, String)>>,
) -> Option<u32> {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + row_h()];

    // Editing path: if the user is editing THIS row, render an input_text
    // covering the text area and return early. Selection / hover decoration
    // stays off this row so it isn't visually confusing.
    let editing_this = editing
        .as_ref()
        .map_or(false, |e| e.side == side && e.row_idx == idx as usize);
    if editing_this {
        let _font_tok = mono_font.map(|f| ui.push_font(f));
        let dl = ui.get_window_draw_list();
        dl.add_rect(p0, p1, [0.18, 0.22, 0.30, 1.0]).filled(true).build();
        // Gutter line number stays visible to the left of the editor.
        let line_text = match row.line_no {
            Some(n) => format!("{n:>4}"),
            None => "    ".to_string(),
        };
        dl.add_text(
            [p0[0] + 6.0, p0[1] + 3.0],
            [0.55, 0.60, 0.70, 1.0],
            &line_text,
        );
        let edit_state = editing.as_mut().unwrap();
        if edit_state.just_started {
            ui.set_keyboard_focus_here();
            edit_state.just_started = false;
        }
        ui.set_cursor_screen_pos([p0[0] + gutter_w(), p0[1]]);
        ui.set_next_item_width(row_w - gutter_w());
        let _pad = ui.push_style_var(StyleVar::FramePadding([2.0, 1.0]));
        let id_for_input = format!("##edit_{:?}_{idx}", side);
        let changed = ui
            .input_text(id_for_input, &mut edit_state.buffer)
            .enter_returns_true(true)
            .build();
        let active = ui.is_item_active();
        let deactivated = ui.is_item_deactivated();
        drop(_pad);
        drop(_font_tok);

        if changed {
            edit_commit.set(Some((side, edit_state.line_no, edit_state.buffer.clone())));
            *editing = None;
        } else if ui.is_key_pressed(imgui::Key::Escape) {
            *editing = None;
        } else if deactivated && !active {
            // Lost focus without Enter: commit current buffer.
            edit_commit.set(Some((side, edit_state.line_no, edit_state.buffer.clone())));
            *editing = None;
        }
        // Pin cursor down by exactly row_h() so the layout math (used by
        // the connector) stays consistent with the non-editing rows.
        ui.set_cursor_screen_pos([p0[0], p0[1] + row_h()]);
        return None;
    }

    let id_str = format!("row_{:?}_{idx}", side);
    let _clicked_lmb = ui.invisible_button(id_str, [row_w, row_h()]);
    // `is_item_hovered` returns false for any row that isn't the active item
    // while a drag is in progress (imgui blocks hover for non-active items).
    // For drag-extend-selection we need a pure-positional hover check, so we
    // use `is_mouse_hovering_rect` (clipped by the child window).
    let mouse_in_row = ui.is_mouse_hovering_rect(p0, p1);
    let hovered = ui.is_item_hovered();
    let activated = ui.is_item_activated();
    let dbl_click = hovered && ui.is_mouse_double_clicked(imgui::MouseButton::Left);
    let rmb_anchor = hovered && ui.is_mouse_clicked(imgui::MouseButton::Right);
    if hovered && row.is_change {
        // Anchor the hover overlay at the first row of the hunk (so it
        // doesn't follow the cursor row-by-row). If that first row has
        // scrolled above the visible band, clamp to the band's top so the
        // overlay always shows for a hunk the user is inside of.
        let pane_origin_y = p0[1] - (idx as f32) * row_h();
        let pane_visible_top = pane_origin_y + ui.scroll_y();
        let first_row_y = pane_origin_y + (row.hunk_first_row as f32) * row_h();
        let anchor_y = first_row_y.max(pane_visible_top);
        hover_out.set(Some((row.hunk_id, [p0[0], anchor_y])));
    }

    // Double-click starts inline edit on rows that have a real source line.
    // Equal rows on the left/right map to their respective a/b line; delete
    // rows have only a; insert rows have only b. We allow editing any row
    // with a `line_no`.
    if dbl_click {
        if let Some(ln) = row.line_no {
            let text: String = row.segments.iter().map(|s| s.text.as_str()).collect();
            *editing = Some(EditState {
                side,
                row_idx: idx as usize,
                line_no: ln,
                buffer: text,
                just_started: true,
            });
            // Clear any in-progress selection so the editor takes over cleanly.
            *selection = None;
            focus_event.set(Some(side.as_focused_pane()));
            // Skip selection-start handling for this frame.
            return None;
        }
    }

    // Push mono for both text rendering and column hit-testing. calc_text_size
    // and the per-character width inferred from it both depend on the active
    // font, so they must be measured under the same push.
    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let char_w = ui.calc_text_size("m")[0].max(1.0);
    let text_start_x = p0[0] + gutter_w();
    let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();

    // For drag-extend selection we want the column under the mouse even when
    // another row holds the active state, so use the positional `mouse_in_row`
    // check rather than imgui's hover (which is blocked during drag).
    let col_at_mouse = if mouse_in_row {
        let mx = ui.io().mouse_pos[0];
        let raw = ((mx - text_start_x) / char_w).round();
        Some(raw.clamp(0.0, char_count as f32) as usize)
    } else {
        None
    };

    // --- Selection events ---
    if activated {
        let col = col_at_mouse.unwrap_or(0);
        let row_idx = idx as usize;
        let shift = ui.io().key_shift;
        if shift && selection.as_ref().map_or(false, |s| s.side == side) {
            let sel = selection.as_mut().unwrap();
            sel.caret = (row_idx, col);
            sel.dragging = true;
        } else {
            *selection = Some(Selection {
                side,
                anchor: (row_idx, col),
                caret: (row_idx, col),
                dragging: true,
            });
        }
        focus_event.set(Some(side.as_focused_pane()));
    }
    if mouse_in_row {
        if let Some(sel) = selection.as_mut() {
            if sel.dragging
                && sel.side == side
                && ui.is_mouse_down(imgui::MouseButton::Left)
            {
                if let Some(col) = col_at_mouse {
                    sel.caret = (idx as usize, col);
                }
            }
        }
    }

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

    // Selection background — drawn after hunk bg / hover so it overrides them,
    // but before text so glyphs remain readable.
    if let Some(sel) = selection.as_ref() {
        if sel.side == side {
            let (s_row, s_col, e_row, e_col) = normalize_selection(sel);
            let row_idx = idx as usize;
            if row_idx >= s_row && row_idx <= e_row {
                let l_col = if row_idx == s_row { s_col } else { 0 };
                let r_col = if row_idx == e_row { e_col } else { char_count };
                if r_col > l_col {
                    let sel_x0 = text_start_x + l_col as f32 * char_w;
                    let sel_x1 = text_start_x + r_col as f32 * char_w;
                    dl.add_rect([sel_x0, p0[1]], [sel_x1, p1[1]], [0.26, 0.59, 0.98, 0.40])
                        .filled(true)
                        .build();
                }
            }
        }
    }

    let line_text = match row.line_no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    };
    let text_y = p0[1] + 3.0;
    dl.add_text([p0[0] + 6.0, text_y], [0.55, 0.60, 0.70, 1.0], &line_text);

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
    let mut x = text_start_x;
    for seg in &row.segments {
        if seg.text.is_empty() {
            continue;
        }
        let w = ui.calc_text_size(&seg.text)[0];
        if seg.hl {
            dl.add_rect(
                [x, p0[1] + 2.0],
                [x + w, p0[1] + row_h() - 2.0],
                hl_bg,
            )
            .filled(true)
            .build();
        }
        dl.add_text([x, text_y], fg, &seg.text);
        x += w;
    }
    drop(_font_tok);

    if let Some(ln) = row.line_no {
        if anchored.contains(&ln) {
            // Black left edge marker.
            dl.add_rect(p0, [p0[0] + 3.0, p1[1]], [0.0, 0.0, 0.0, 1.0])
                .filled(true)
                .build();
        }
    }

    if rmb_anchor {
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
