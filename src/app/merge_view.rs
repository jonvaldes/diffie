//! 3-way merge view.
//!
//! Three virtualized panes (BASE, LOCAL, REMOTE) separated by two bezier
//! connector strips. Inline resolution buttons in the LOCAL pane drive
//! `SessionStore::set_three_way_resolution`. Scroll sync mirrors the 2-way
//! center-anchored algorithm extended to a designated driver among 3.

use std::cell::Cell;
use std::collections::HashMap;

use imgui::{ListClipper, StyleVar, Ui};

use crate::merge::{MergeAnchor, MergeHunk, Resolution};
use crate::session::{SessionId, SessionStore};

pub const ROW_H: f32 = 20.0;
const CONNECTOR_W: f32 = 56.0;
const ECHO_TOLERANCE: f32 = 0.5;

#[derive(Default)]
pub struct MergeViewState {
    last: [f32; 3],
    written: [Option<f32>; 3],
    pending: [Option<f32>; 3],
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pane {
    Base = 0,
    Local = 1,
    Remote = 2,
}

#[derive(Clone, Copy)]
enum Cls {
    Equal,
    Stable,
    LocalOnly,
    RemoteOnly,
    Conflict,
}

#[derive(Clone)]
struct Row {
    line_no: u32,
    text: String,
    cls: Cls,
}

#[derive(Clone)]
enum Entry {
    Control { hunk_id: u32, kind: HunkKind },
    ControlPlaceholder,
    Row(Row),
}

#[derive(Clone, Copy)]
enum HunkKind {
    LocalOnly,
    RemoteOnly,
    Conflict,
}

struct PaneLayout {
    entries: Vec<Entry>,
    ranges: Vec<(u32, f32, f32)>,
    line_ys: HashMap<u32, f32>,
}

fn cls_for(h: &MergeHunk) -> Cls {
    match h {
        MergeHunk::Stable { .. } => Cls::Stable,
        MergeHunk::LocalOnly { .. } => Cls::LocalOnly,
        MergeHunk::RemoteOnly { .. } => Cls::RemoteOnly,
        MergeHunk::Conflict { .. } => Cls::Conflict,
    }
}

fn select_text<'a>(h: &'a MergeHunk, pane: Pane) -> &'a [String] {
    match (h, pane) {
        (MergeHunk::Stable { text, .. }, _) => text,
        (MergeHunk::LocalOnly { base, .. }, Pane::Base) => base,
        (MergeHunk::LocalOnly { local, .. }, Pane::Local) => local,
        (MergeHunk::LocalOnly { base, .. }, Pane::Remote) => base,
        (MergeHunk::RemoteOnly { base, .. }, Pane::Base) => base,
        (MergeHunk::RemoteOnly { base, .. }, Pane::Local) => base,
        (MergeHunk::RemoteOnly { remote, .. }, Pane::Remote) => remote,
        (MergeHunk::Conflict { base, .. }, Pane::Base) => base,
        (MergeHunk::Conflict { local, .. }, Pane::Local) => local,
        (MergeHunk::Conflict { remote, .. }, Pane::Remote) => remote,
    }
}

fn hunk_kind(h: &MergeHunk) -> Option<HunkKind> {
    match h {
        MergeHunk::Stable { .. } => None,
        MergeHunk::LocalOnly { .. } => Some(HunkKind::LocalOnly),
        MergeHunk::RemoteOnly { .. } => Some(HunkKind::RemoteOnly),
        MergeHunk::Conflict { .. } => Some(HunkKind::Conflict),
    }
}

fn build_layout(hunks: &[MergeHunk], pane: Pane) -> PaneLayout {
    let mut entries: Vec<Entry> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut line_ys: HashMap<u32, f32> = HashMap::new();
    let mut y: f32 = 0.0;
    let mut line_n: u32 = 1;
    for h in hunks {
        let start_y = y;
        // Control row appears only in LOCAL pane for non-stable hunks.
        // Other panes reserve the same height with a placeholder so hunk
        // y-ranges stay aligned across panes for ribbon drawing.
        if let Some(kind) = hunk_kind(h) {
            entries.push(match pane {
                Pane::Local => Entry::Control { hunk_id: h.id(), kind },
                _ => Entry::ControlPlaceholder,
            });
            y += ROW_H;
        }
        let cls = cls_for(h);
        let cls_for_row = match cls {
            Cls::Stable => Cls::Equal,
            other => other,
        };
        for t in select_text(h, pane) {
            entries.push(Entry::Row(Row {
                line_no: line_n,
                text: t.clone(),
                cls: cls_for_row,
            }));
            line_ys.insert(line_n, y);
            line_n += 1;
            y += ROW_H;
        }
        if y > start_y {
            ranges.push((h.id(), start_y, y));
        }
    }
    PaneLayout { entries, ranges, line_ys }
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunks: &[MergeHunk],
    anchors: &[MergeAnchor],
    status: &mut String,
    state: &mut MergeViewState,
) {
    let base = build_layout(hunks, Pane::Base);
    let local = build_layout(hunks, Pane::Local);
    let remote = build_layout(hunks, Pane::Remote);

    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W * 2.0) / 3.0).max(80.0);

    let scrolls: [Cell<f32>; 3] =
        [Cell::new(0.0), Cell::new(0.0), Cell::new(0.0)];
    let origins: [Cell<[f32; 2]>; 3] = [
        Cell::new([0.0; 2]),
        Cell::new([0.0; 2]),
        Cell::new([0.0; 2]),
    ];
    let visibles: [Cell<f32>; 3] =
        [Cell::new(avail[1]), Cell::new(avail[1]), Cell::new(avail[1])];

    let applies: [Option<f32>; 3] = [
        state.pending[0].take(),
        state.pending[1].take(),
        state.pending[2].take(),
    ];

    render_pane(
        ui,
        "diffie_base",
        pane_w,
        avail[1],
        &base.entries,
        Pane::Base,
        store,
        session_id,
        status,
        applies[0],
        &mut state.written[Pane::Base as usize],
        &scrolls[Pane::Base as usize],
        &origins[Pane::Base as usize],
        &visibles[Pane::Base as usize],
    );

    ui.same_line_with_spacing(0.0, 0.0);
    let connector_bl = ui.cursor_screen_pos();
    ui.dummy([CONNECTOR_W, avail[1]]);
    ui.same_line_with_spacing(0.0, 0.0);

    render_pane(
        ui,
        "diffie_local",
        pane_w,
        avail[1],
        &local.entries,
        Pane::Local,
        store,
        session_id,
        status,
        applies[1],
        &mut state.written[Pane::Local as usize],
        &scrolls[Pane::Local as usize],
        &origins[Pane::Local as usize],
        &visibles[Pane::Local as usize],
    );

    ui.same_line_with_spacing(0.0, 0.0);
    let connector_lr = ui.cursor_screen_pos();
    ui.dummy([CONNECTOR_W, avail[1]]);
    ui.same_line_with_spacing(0.0, 0.0);

    render_pane(
        ui,
        "diffie_remote",
        pane_w,
        avail[1],
        &remote.entries,
        Pane::Remote,
        store,
        session_id,
        status,
        applies[2],
        &mut state.written[Pane::Remote as usize],
        &scrolls[Pane::Remote as usize],
        &origins[Pane::Remote as usize],
        &visibles[Pane::Remote as usize],
    );

    let s = [scrolls[0].get(), scrolls[1].get(), scrolls[2].get()];
    let v = [visibles[0].get(), visibles[1].get(), visibles[2].get()];
    sync_scrolls(state, s, v, &base.ranges, &local.ranges, &remote.ranges);

    draw_connector(
        ui,
        connector_bl,
        CONNECTOR_W,
        avail[1],
        origins[0].get()[1],
        origins[1].get()[1],
        s[0],
        s[1],
        &base.ranges,
        &local.ranges,
        &base.line_ys,
        &local.line_ys,
        anchors.iter().map(|a| (a.base, a.local)).collect::<Vec<_>>().as_slice(),
        hunks,
    );
    draw_connector(
        ui,
        connector_lr,
        CONNECTOR_W,
        avail[1],
        origins[1].get()[1],
        origins[2].get()[1],
        s[1],
        s[2],
        &local.ranges,
        &remote.ranges,
        &local.line_ys,
        &remote.line_ys,
        anchors.iter().map(|a| (a.local, a.remote)).collect::<Vec<_>>().as_slice(),
        hunks,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    ui: &Ui,
    id: &str,
    w: f32,
    h: f32,
    entries: &[Entry],
    pane: Pane,
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
    apply: Option<f32>,
    written: &mut Option<f32>,
    scroll_out: &Cell<f32>,
    origin_out: &Cell<[f32; 2]>,
    visible_out: &Cell<f32>,
) {
    ui.child_window(id).size([w, h]).border(true).build(|| {
        if let Some(y) = apply {
            ui.set_scroll_y(y);
            *written = Some(y);
        }
        scroll_out.set(ui.scroll_y());
        origin_out.set(ui.window_pos());
        visible_out.set(ui.content_region_avail()[1]);
        draw_pane(ui, entries, pane, store, session_id, status);
    });
}

fn draw_pane(
    ui: &Ui,
    entries: &[Entry],
    _pane: Pane,
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
                Entry::Control { hunk_id, kind } => {
                    draw_control_row(ui, store, session_id, *hunk_id, *kind, status)
                }
                Entry::ControlPlaceholder => {
                    ui.dummy([0.0, ROW_H]);
                }
                Entry::Row(r) => draw_row(ui, r, i),
            }
        }
    }
}

fn draw_control_row(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    kind: HunkKind,
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

    match kind {
        HunkKind::LocalOnly => {
            if ui.small_button(format!("Use Local##L{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Local, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##B{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
        }
        HunkKind::RemoteOnly => {
            if ui.small_button(format!("Use Remote##R{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Remote, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##B{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
        }
        HunkKind::Conflict => {
            if ui.small_button(format!("Use Local##L{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Local, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##B{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Remote##R{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Remote, status);
            }
        }
    }
}

fn apply_res(
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    res: Resolution,
    status: &mut String,
) {
    let label = match &res {
        Resolution::Local => "local",
        Resolution::Base => "base",
        Resolution::Remote => "remote",
        Resolution::Custom { .. } => "custom",
    };
    match store.set_three_way_resolution(session_id, hunk_id, res) {
        Ok(()) => *status = format!("hunk {hunk_id}: use {label}"),
        Err(e) => *status = format!("hunk {hunk_id}: {e}"),
    }
}

fn draw_row(ui: &Ui, row: &Row, idx: i32) {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + ROW_H];

    let _ = ui.invisible_button(format!("mrow_{idx}"), [row_w, ROW_H]);

    let dl = ui.get_window_draw_list();
    let bg = match row.cls {
        Cls::Equal => None,
        Cls::Stable => None,
        Cls::LocalOnly => Some([0.15, 0.30, 0.60, 0.22]),
        Cls::RemoteOnly => Some([0.40, 0.20, 0.55, 0.22]),
        Cls::Conflict => Some([0.55, 0.34, 0.10, 0.30]),
    };
    if let Some(bg_rgba) = bg {
        dl.add_rect(p0, p1, bg_rgba).filled(true).build();
    }
    let line_text = format!("{:>4}", row.line_no);
    let text_y = p0[1] + 3.0;
    dl.add_text([p0[0] + 4.0, text_y], [0.55, 0.60, 0.70, 1.0], &line_text);
    let fg = match row.cls {
        Cls::Equal | Cls::Stable => [0.90, 0.92, 0.96, 1.0],
        Cls::LocalOnly => [0.78, 0.88, 1.0, 1.0],
        Cls::RemoteOnly => [0.92, 0.80, 1.0, 1.0],
        Cls::Conflict => [1.0, 0.84, 0.62, 1.0],
    };
    let display = if row.text.is_empty() {
        " "
    } else {
        row.text.as_str()
    };
    dl.add_text([p0[0] + 44.0, text_y], fg, display);
}

// ----------------- connector & sync -----------------

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

fn ribbon_color(h: &MergeHunk) -> [f32; 4] {
    match h {
        MergeHunk::Stable { .. } => [0.55, 0.60, 0.70, 0.10],
        MergeHunk::LocalOnly { .. } => [0.15, 0.45, 0.92, 0.28],
        MergeHunk::RemoteOnly { .. } => [0.50, 0.30, 0.85, 0.28],
        MergeHunk::Conflict { .. } => [0.85, 0.45, 0.15, 0.32],
    }
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
    anchor_lines: &[(u32, u32)],
    hunks: &[MergeHunk],
) {
    let dl = ui.get_window_draw_list();
    dl.with_clip_rect_intersect(origin, [origin[0] + w, origin[1] + h], || {
        let x_l = origin[0];
        let x_r = origin[0] + w;
        let cx = origin[0] + w * 0.5;
        let band_top = origin[1];
        let band_bot = origin[1] + h;

        for h_obj in hunks {
            let id = h_obj.id();
            let Some(lr) = left_ranges.iter().find(|r| r.0 == id) else {
                continue;
            };
            let Some(rr) = right_ranges.iter().find(|r| r.0 == id) else {
                continue;
            };
            let a1 = left_top_screen_y + lr.1 - left_scroll;
            let a2 = left_top_screen_y + lr.2 - left_scroll;
            let b1 = right_top_screen_y + rr.1 - right_scroll;
            let b2 = right_top_screen_y + rr.2 - right_scroll;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            let color = ribbon_color(h_obj);
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

        for (l_line, r_line) in anchor_lines {
            let Some(ly_content) = left_line_ys.get(l_line) else {
                continue;
            };
            let Some(ry_content) = right_line_ys.get(r_line) else {
                continue;
            };
            let ly = left_top_screen_y + ly_content + ROW_H * 0.5 - left_scroll;
            let ry = right_top_screen_y + ry_content + ROW_H * 0.5 - right_scroll;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            let pts = sample_curve([x_l, ly], [cx, ly], [cx, ry], [x_r, ry]);
            for i in 0..pts.len() - 1 {
                dl.add_line(pts[i], pts[i + 1], [0.0, 0.0, 0.0, 1.0])
                    .thickness(3.0)
                    .build();
            }
        }
    });
}

fn sync_scrolls(
    state: &mut MergeViewState,
    curr: [f32; 3],
    view_h: [f32; 3],
    base_ranges: &[(u32, f32, f32)],
    local_ranges: &[(u32, f32, f32)],
    remote_ranges: &[(u32, f32, f32)],
) {
    let ranges = [base_ranges, local_ranges, remote_ranges];
    let mut driver: Option<usize> = None;
    for i in 0..3 {
        let changed = (curr[i] - state.last[i]).abs() > ECHO_TOLERANCE;
        let echo = state.written[i].map_or(false, |w| (curr[i] - w).abs() < ECHO_TOLERANCE);
        if changed && !echo {
            driver = Some(i);
            break;
        }
    }
    if let Some(src) = driver {
        for dst in 0..3 {
            if dst == src {
                continue;
            }
            if let Some(target) =
                target_scroll(curr[src], view_h[src], view_h[dst], ranges[src], ranges[dst])
            {
                state.pending[dst] = Some(target);
            }
        }
    }
    state.last = curr;
    for i in 0..3 {
        if let Some(w) = state.written[i] {
            if (curr[i] - w).abs() < ECHO_TOLERANCE {
                state.written[i] = None;
            }
        }
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
