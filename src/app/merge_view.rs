//! 3-way merge view.
//!
//! Three virtualized panes (BASE, LOCAL, REMOTE) separated by two bezier
//! connector strips. Inline resolution buttons in the LOCAL pane drive
//! `SessionStore::set_three_way_resolution`. Scroll sync mirrors the 2-way
//! center-anchored algorithm extended to a designated driver among 3.

use std::cell::Cell;
use std::collections::HashMap;

use imgui::{FontId, ListClipper, StyleVar, Ui};

use crate::merge::{MergeAnchor, MergeHunk, Resolution};
use crate::session::{SessionId, SessionStore};

/// Match diff_view: tall enough for the 1.5x Roboto Mono used in code rows
/// at zoom=1.0.
const ROW_H_BASE: f32 = 24.0;
const GUTTER_W_BASE: f32 = 60.0;
const CONNECTOR_W: f32 = 56.0;

fn row_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}
const ECHO_TOLERANCE: f32 = 0.5;

#[derive(Default)]
pub struct MergeViewState {
    last: [f32; 3],
    written: [Option<f32>; 3],
    pending: [Option<f32>; 3],
    pub selection: Option<Selection>,
}

#[derive(Clone)]
pub struct Selection {
    pub pane: Pane,
    pub anchor: (usize, usize),
    pub caret: (usize, usize),
    pub dragging: bool,
}

fn normalize_selection(sel: &Selection) -> (usize, usize, usize, usize) {
    let (a, b) = (sel.anchor, sel.caret);
    if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
        (a.0, a.1, b.0, b.1)
    } else {
        (b.0, b.1, a.0, a.1)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pane {
    Base = 0,
    Local = 1,
    Remote = 2,
}

impl Pane {
    pub fn as_focused_pane(self) -> crate::app::FocusedPane {
        match self {
            Pane::Base => crate::app::FocusedPane::ThreeWayBase,
            Pane::Local => crate::app::FocusedPane::ThreeWayLocal,
            Pane::Remote => crate::app::FocusedPane::ThreeWayRemote,
        }
    }
}

pub fn extract_selection_text(snap: &crate::session::DiffSession, sel: &Selection) -> String {
    let crate::session::SessionMode::ThreeWay { hunks, .. } = &snap.mode else {
        return String::new();
    };
    let pane = build_layout(hunks, sel.pane);
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
        let chars: Vec<char> = row.text.chars().collect();
        let l = if r == s_row { s_col } else { 0 }.min(chars.len());
        let h = if r == e_row { e_col } else { chars.len() }.min(chars.len());
        out.extend(chars[l..h].iter());
        if r < e_row {
            out.push('\n');
        }
    }
    out
}

pub fn select_all(snap: &crate::session::DiffSession, pane: Pane) -> Option<Selection> {
    let crate::session::SessionMode::ThreeWay { hunks, .. } = &snap.mode else {
        return None;
    };
    let layout = build_layout(hunks, pane);
    if layout.rows.is_empty() {
        return None;
    }
    let last_idx = layout.rows.len() - 1;
    let last_chars = layout.rows[last_idx].text.chars().count();
    Some(Selection {
        pane,
        anchor: (0, 0),
        caret: (last_idx, last_chars),
        dragging: false,
    })
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
    hunk_id: u32,
    /// One of LocalOnly / RemoteOnly / Conflict if this row belongs to a
    /// non-stable hunk (i.e., a hunk the resolution overlay can act on),
    /// else None for stable hunks.
    kind: Option<HunkKind>,
}

#[derive(Clone, Copy)]
enum HunkKind {
    LocalOnly,
    RemoteOnly,
    Conflict,
}

struct PaneLayout {
    rows: Vec<Row>,
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
    let mut rows: Vec<Row> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut line_ys: HashMap<u32, f32> = HashMap::new();
    let mut y: f32 = 0.0;
    let mut line_n: u32 = 1;
    for h in hunks {
        let start_y = y;
        let kind = hunk_kind(h);
        let cls = cls_for(h);
        let cls_for_row = match cls {
            Cls::Stable => Cls::Equal,
            other => other,
        };
        for t in select_text(h, pane) {
            rows.push(Row {
                line_no: line_n,
                text: t.clone(),
                cls: cls_for_row,
                hunk_id: h.id(),
                kind,
            });
            line_ys.insert(line_n, y);
            line_n += 1;
            y += row_h();
        }
        if y > start_y {
            ranges.push((h.id(), start_y, y));
        }
    }
    PaneLayout { rows, ranges, line_ys }
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
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
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
    let focus_event: Cell<Option<crate::app::FocusedPane>> = Cell::new(None);

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
        &base.rows,
        Pane::Base,
        store,
        session_id,
        status,
        applies[0],
        &mut state.written[Pane::Base as usize],
        &scrolls[Pane::Base as usize],
        &origins[Pane::Base as usize],
        &visibles[Pane::Base as usize],
        mono_font,
        &mut state.selection,
        &focus_event,
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
        &local.rows,
        Pane::Local,
        store,
        session_id,
        status,
        applies[1],
        &mut state.written[Pane::Local as usize],
        &scrolls[Pane::Local as usize],
        &origins[Pane::Local as usize],
        &visibles[Pane::Local as usize],
        mono_font,
        &mut state.selection,
        &focus_event,
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
        &remote.rows,
        Pane::Remote,
        store,
        session_id,
        status,
        applies[2],
        &mut state.written[Pane::Remote as usize],
        &scrolls[Pane::Remote as usize],
        &origins[Pane::Remote as usize],
        &visibles[Pane::Remote as usize],
        mono_font,
        &mut state.selection,
        &focus_event,
    );

    if let Some(p) = focus_event.get() {
        *focus_request = Some(p);
    }
    if !ui.is_mouse_down(imgui::MouseButton::Left) {
        if let Some(sel) = state.selection.as_mut() {
            sel.dragging = false;
        }
    }

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
    rows: &[Row],
    pane: Pane,
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
    apply: Option<f32>,
    written: &mut Option<f32>,
    scroll_out: &Cell<f32>,
    origin_out: &Cell<[f32; 2]>,
    visible_out: &Cell<f32>,
    mono_font: Option<FontId>,
    selection: &mut Option<Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
) {
    ui.child_window(id).size([w, h]).border(true).build(|| {
        if let Some(y) = apply {
            ui.set_scroll_y(y);
            *written = Some(y);
        }
        scroll_out.set(ui.scroll_y());
        origin_out.set(ui.cursor_screen_pos());
        visible_out.set(ui.content_region_avail()[1]);
        draw_pane(
            ui, rows, pane, store, session_id, status, mono_font, selection, focus_event,
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &Ui,
    rows: &[Row],
    pane: Pane,
    store: &SessionStore,
    session_id: SessionId,
    status: &mut String,
    mono_font: Option<FontId>,
    selection: &mut Option<Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
) {
    let total = rows.len() as i32;
    if total == 0 {
        return;
    }
    // See diff_view::draw_pane for why ItemSpacing.y must be zero: each row
    // must consume exactly row_h() so the connector's content-y model lines
    // up with the actually-rendered screen positions.
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([0.0, 0.0]));
    let hover: Cell<Option<(u32, HunkKind, [f32; 2])>> = Cell::new(None);
    let mut clipper = ListClipper::new(total).items_height(row_h()).begin(ui);
    while clipper.step() {
        for i in clipper.display_start()..clipper.display_end() {
            draw_row(
                ui,
                &rows[i as usize],
                i,
                pane,
                mono_font,
                &hover,
                selection,
                focus_event,
            );
        }
    }
    drop(_spacing);
    if let Some((hunk_id, kind, pos)) = hover.get() {
        draw_control_overlay(ui, store, session_id, hunk_id, kind, status, pos);
    }
}

fn draw_control_overlay(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    kind: HunkKind,
    status: &mut String,
    pos: [f32; 2],
) {
    let _pad = ui.push_style_var(StyleVar::FramePadding([6.0, 2.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([4.0, 0.0]));

    let panel_x = pos[0] + 4.0;
    let panel_y = pos[1] + 2.0;
    let panel_w = 260.0;
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
    let border_color = match kind {
        HunkKind::LocalOnly => [0.15, 0.45, 0.92, 1.0],
        HunkKind::RemoteOnly => [0.50, 0.30, 0.85, 1.0],
        HunkKind::Conflict => [0.85, 0.45, 0.15, 1.0],
    };
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        border_color,
    )
    .rounding(4.0)
    .thickness(1.0)
    .build();

    ui.set_cursor_screen_pos([panel_x + 6.0, panel_y + 3.0]);
    match kind {
        HunkKind::LocalOnly => {
            if ui.small_button(format!("Use Local##ovL{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Local, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##ovB{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
        }
        HunkKind::RemoteOnly => {
            if ui.small_button(format!("Use Remote##ovR{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Remote, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##ovB{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
        }
        HunkKind::Conflict => {
            if ui.small_button(format!("Use Local##ovL{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Local, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Base##ovB{hunk_id}")) {
                apply_res(store, session_id, hunk_id, Resolution::Base, status);
            }
            ui.same_line();
            if ui.small_button(format!("Use Remote##ovR{hunk_id}")) {
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

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    row: &Row,
    idx: i32,
    pane: Pane,
    mono_font: Option<FontId>,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
    selection: &mut Option<Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
) {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + row_h()];

    let _ = ui.invisible_button(format!("mrow_{idx}"), [row_w, row_h()]);
    let hovered = ui.is_item_hovered();
    let activated = ui.is_item_activated();
    if let Some(kind) = row.kind {
        if hovered {
            hover_out.set(Some((row.hunk_id, kind, p0)));
        }
    }

    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let char_w = ui.calc_text_size("m")[0].max(1.0);
    let text_start_x = p0[0] + gutter_w();
    let char_count = row.text.chars().count();

    let col_at_mouse = if hovered {
        let mx = ui.io().mouse_pos[0];
        let raw = ((mx - text_start_x) / char_w).round();
        Some(raw.clamp(0.0, char_count as f32) as usize)
    } else {
        None
    };

    if activated {
        let col = col_at_mouse.unwrap_or(0);
        let row_idx = idx as usize;
        let shift = ui.io().key_shift;
        if shift && selection.as_ref().map_or(false, |s| s.pane == pane) {
            let sel = selection.as_mut().unwrap();
            sel.caret = (row_idx, col);
            sel.dragging = true;
        } else {
            *selection = Some(Selection {
                pane,
                anchor: (row_idx, col),
                caret: (row_idx, col),
                dragging: true,
            });
        }
        focus_event.set(Some(pane.as_focused_pane()));
    }
    if hovered {
        if let Some(sel) = selection.as_mut() {
            if sel.dragging && sel.pane == pane && ui.is_mouse_down(imgui::MouseButton::Left) {
                if let Some(col) = col_at_mouse {
                    sel.caret = (idx as usize, col);
                }
            }
        }
    }

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
    // Selection background overlay.
    if let Some(sel) = selection.as_ref() {
        if sel.pane == pane {
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
    let line_text = format!("{:>4}", row.line_no);
    let text_y = p0[1] + 3.0;
    dl.add_text([p0[0] + 6.0, text_y], [0.55, 0.60, 0.70, 1.0], &line_text);
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
    dl.add_text([p0[0] + gutter_w(), text_y], fg, display);
    drop(_font_tok);
}

// ----------------- connector & sync -----------------

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

fn fill_bezier_ribbon(x_l: f32, x_r: f32, a1: f32, a2: f32, b1: f32, b2: f32, color: [f32; 4]) {
    let cx = (x_l + x_r) * 0.5;
    let top = sample_curve([x_l, a1], [cx, a1], [cx, b1], [x_r, b1]);
    let bot = sample_curve([x_l, a2], [cx, a2], [cx, b2], [x_r, b2]);
    let mut outline: Vec<[f32; 2]> = top;
    outline.extend(bot.into_iter().rev());
    fill_polygon(&outline, color);
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
    left_origin_y: f32,
    right_origin_y: f32,
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
            let a1 = left_origin_y + lr.1;
            let a2 = left_origin_y + lr.2;
            let b1 = right_origin_y + rr.1;
            let b2 = right_origin_y + rr.2;
            if (a2 < band_top && b2 < band_top) || (a1 > band_bot && b1 > band_bot) {
                continue;
            }
            fill_bezier_ribbon(x_l, x_r, a1, a2, b1, b2, ribbon_color(h_obj));
        }

        for (l_line, r_line) in anchor_lines {
            let Some(ly_content) = left_line_ys.get(l_line) else {
                continue;
            };
            let Some(ry_content) = right_line_ys.get(r_line) else {
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
    // dl is only used inside the closure for clipping; the path ops use
    // sys::igGetWindowDrawList directly. Touching the binding silences
    // unused-variable lints if the loops happen to skip everything.
    let _ = dl;
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
