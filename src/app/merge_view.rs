//! 3-way merge view.
//!
//! Three `input_text_multiline` panes (BASE / LOCAL / REMOTE) — all
//! **editable** — separated by bezier connector strips. Edits emit
//! `DiffEdit::SetSide { side: SideRef::ThreeWay(...) }` so a re-merge runs
//! on the next frame. Resolution buttons live in a small hover overlay
//! anchored to the top of each non-stable hunk.
//!
//! This module mirrors `diff_view::render_pane` / `paint_row_overlays`
//! generalized to three panes; the row-painting logic is inlined here
//! rather than abstracted because the colour palette and per-pane hunk
//! semantics are different from the 2-way view.

use std::cell::Cell;
use std::collections::HashMap;

use imgui::{FontId, StyleVar, Ui};

use super::theme;
use super::undo_stack::DiffEdit;
use crate::app::syntax::LineSpans;
use crate::app::syntax_paint;
use crate::merge::{MergeAnchor, MergeHunk, Resolution};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, ThreeWaySide};

/// Match diff_view: tall enough for the 1.5x Roboto Mono used in code rows
/// at zoom=1.0.
/// Max line pixel width across `buf` under the active imgui font.
fn compute_max_line_w(ui: &Ui, buf: &str) -> f32 {
    let mut max = 0.0_f32;
    for line in buf.lines() {
        let w = ui.calc_text_size(line)[0];
        if w > max {
            max = w;
        }
    }
    max
}

const ROW_H_BASE: f32 = 24.0;
const GUTTER_W_BASE: f32 = 60.0;
const CONNECTOR_W: f32 = 56.0;
const ECHO_TOLERANCE: f32 = 1.0;
const SCROLL_LINES_PER_WHEEL_TICK: f32 = 3.0;
const SCROLL_SMOOTH_SPEED: f32 = 25.0;
const SCROLL_SNAP_EPSILON: f32 = 0.5;

/// Deprecated: use `ui.text_line_height()` inside the mono font scope.
/// Kept for any callers we missed.
#[allow(dead_code)]
fn row_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}

#[derive(Default)]
pub struct MergeViewState {
    /// Buffer mirrors of `session.base_text`/`local_text`/`remote_text`.
    /// Synced at frame start; written-back on every `input_text_multiline`
    /// change.
    base_buf: String,
    local_buf: String,
    remote_buf: String,
    /// Last *displayed* scroll_y per pane — the eased value pushed to imgui
    /// last frame. Used by overlay paint and as the start of the next ease.
    last: [f32; 3],
    /// Where each pane is scrolling toward. Wheel, sync, and jump update
    /// this; `last` eases toward it each frame.
    target: [f32; 3],
    /// Outer-scroll-window horizontal scroll position per pane, captured
    /// each frame from imgui via igGetScrollX. Each multiline is wrapped
    /// in an outer child window with HORIZONTAL_SCROLLBAR; imgui handles
    /// user-driven horizontal scrolling natively and we just mirror the
    /// value for overlay alignment.
    scroll_x: [f32; 3],
    /// Max line pixel width per pane in the active mono font. Cached so
    /// we only re-measure on buffer change — used to size the inner
    /// multiline wide enough that no internal horizontal caret-tracking
    /// scroll ever triggers (the outer wrapper owns horizontal scroll).
    max_line_w: [f32; 3],
    /// Last observed caret byte per pane. Caret-tracking horizontal
    /// scroll only fires when the byte position changes, so the user
    /// can wheel-scroll away from the caret without it snapping back.
    last_caret: [Option<i32>; 3],
    /// Pending scroll value to apply next frame on a given pane.
    pending: [Option<f32>; 3],
    /// First-frame focus line (1-based) per pane. Set by the open path to
    /// the first non-Stable hunk's start so the user lands at the first
    /// difference. Resolved to a `pending` scroll inside the pane render
    /// once `lh` is known, then cleared.
    pub(crate) pending_initial_line: [Option<u32>; 3],
    /// Bumped on external buffer mutations (undo/redo, Apply Local/Base/Remote);
    /// mixed into the widget ID so imgui re-initialises stb_textedit from `buf`.
    pub input_epoch: u32,
    /// Active drag offset for each pane's custom vertical scrollbar thumb.
    /// `Some(off)` means mid-drag — `off` is the pixel distance from thumb top
    /// to the mouse cursor at drag start. See diff_view for the rationale: the
    /// inner multiline's native scrollbar sits past the horizontally-scrolling
    /// viewport's right edge and gets clipped, so we paint our own at the
    /// outer's fixed right edge.
    vbar_drag: [Option<f32>; 3],
}

impl MergeViewState {
    /// True when any pane's eased scroll hasn't yet reached its target — used
    /// by the event loop to keep redrawing while the animation runs.
    pub fn is_animating(&self) -> bool {
        const EPS: f32 = 0.5;
        (0..3).any(|i| (self.target[i] - self.last[i]).abs() > EPS)
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

// ---------- layout (per-pane line ranges, used by connector + sync) ----------

#[derive(Clone, Copy)]
enum HunkKind {
    LocalOnly,
    RemoteOnly,
    Conflict,
}

fn hunk_kind(h: &MergeHunk) -> Option<HunkKind> {
    match h {
        MergeHunk::Stable { .. } => None,
        MergeHunk::LocalOnly { .. } => Some(HunkKind::LocalOnly),
        MergeHunk::RemoteOnly { .. } => Some(HunkKind::RemoteOnly),
        MergeHunk::Conflict { .. } => Some(HunkKind::Conflict),
    }
}

fn pane_text<'a>(h: &'a MergeHunk, pane: Pane) -> &'a [String] {
    match (h, pane) {
        (MergeHunk::Stable { base, .. }, Pane::Base) => base,
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

/// (hunk_id, kind, first_line_1based, last_line_1based) for one pane.
/// Stable hunks have `kind = None`.
struct PaneLayout {
    /// Per-hunk row layout: id, kind, first line (1-based), last line (1-based)
    hunks: Vec<(u32, Option<HunkKind>, u32, u32)>,
    /// Content-y ranges per hunk in pane content space; used by scroll-sync
    /// and the connector.
    ranges: Vec<(u32, f32, f32)>,
    /// Content y of the *top* of a given 1-based line, used by the connector
    /// to draw per-anchor curves.
    line_ys: HashMap<u32, f32>,
}

/// Walk the merge-hunk list and return the 1-based start line of the first
/// non-Stable hunk on each pane, or `None` when everything is Stable.
/// Per-pane line accumulation mirrors `pane_text`: Base pane sees `base`,
/// Local/Remote panes see the merged `text` for Stable hunks.
pub fn first_change_lines(hunks: &[MergeHunk]) -> Option<(u32, u32, u32)> {
    let mut base_line: u32 = 1;
    let mut local_line: u32 = 1;
    let mut remote_line: u32 = 1;
    for h in hunks {
        if !matches!(h, MergeHunk::Stable { .. }) {
            return Some((base_line, local_line, remote_line));
        }
        if let MergeHunk::Stable { base, text, .. } = h {
            base_line += base.len() as u32;
            local_line += text.len() as u32;
            remote_line += text.len() as u32;
        }
    }
    None
}

fn build_layout(hunks: &[MergeHunk], pane: Pane, lh: f32) -> PaneLayout {
    let mut hunks_out: Vec<(u32, Option<HunkKind>, u32, u32)> = Vec::new();
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    let mut line_ys: HashMap<u32, f32> = HashMap::new();
    let mut y: f32 = 0.0;
    let mut line_n: u32 = 1;
    for h in hunks {
        let start_y = y;
        let start_line = line_n;
        let kind = hunk_kind(h);
        for _t in pane_text(h, pane) {
            line_ys.insert(line_n, y);
            line_n += 1;
            y += lh;
        }
        let end_line = line_n.saturating_sub(1);
        if y > start_y {
            ranges.push((h.id(), start_y, y));
            hunks_out.push((h.id(), kind, start_line, end_line));
        }
    }
    PaneLayout { hunks: hunks_out, ranges, line_ys }
}

// ---------------------------------- render -----------------------------------

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
    pending_edits: &mut Vec<DiffEdit>,
    base_highlights: &[LineSpans],
    local_highlights: &[LineSpans],
    remote_highlights: &[LineSpans],
) {
    // Sync buffers from session at frame start.
    let snap = match store.snapshot(session_id) {
        Ok(s) => s,
        Err(_) => return,
    };
    let SessionMode::ThreeWay { base_text, local_text, remote_text, .. } = &snap.mode else {
        return;
    };
    let base_changed = state.base_buf != *base_text;
    if base_changed {
        state.base_buf = base_text.clone();
    }
    let local_changed = state.local_buf != *local_text;
    if local_changed {
        state.local_buf = local_text.clone();
    }
    let remote_changed = state.remote_buf != *remote_text;
    if remote_changed {
        state.remote_buf = remote_text.clone();
    }
    // External buffer changes (file load, undo/redo, Apply-side) need a fresh
    // widget id so imgui re-initialises stb_textedit from the new buffer.
    // Callers already bump on the known paths, but bump again here so any
    // future entry point that mutates session text stays correct.
    if base_changed || local_changed || remote_changed {
        state.input_epoch = state.input_epoch.wrapping_add(1);
    }

    let avail = ui.content_region_avail();
    let total_w = avail[0];
    let pane_w = ((total_w - CONNECTOR_W * 2.0) / 3.0).max(80.0);
    let pane_h = avail[1].max(100.0);

    let _font_tok = mono_font.map(|f| ui.push_font(f));
    // Use imgui's actual line height (from the active/mono font metrics) for
    // all overlay positioning so our draw-list text aligns with what
    // input_text_multiline renders.
    let lh = ui.text_line_height();

    // Recompute max line widths under the active mono font when a
    // buffer changes; the outer horizontal-scroll wrapper uses these to
    // size each inner multiline wide enough that its own caret-tracking
    // horizontal scroll never triggers.
    if base_changed {
        state.max_line_w[Pane::Base as usize] = compute_max_line_w(ui, &state.base_buf);
    }
    if local_changed {
        state.max_line_w[Pane::Local as usize] = compute_max_line_w(ui, &state.local_buf);
    }
    if remote_changed {
        state.max_line_w[Pane::Remote as usize] = compute_max_line_w(ui, &state.remote_buf);
    }

    let base_layout = build_layout(hunks, Pane::Base, lh);
    let local_layout = build_layout(hunks, Pane::Local, lh);
    let remote_layout = build_layout(hunks, Pane::Remote, lh);

    // Layout: Remote | connector_rb | Base | connector_bl | Local. Putting
    // Base in the middle makes each connector a true pair (Remote↔Base on the
    // left, Base↔Local on the right), so per-pair ribbon coloring lines up
    // naturally with what each adjacent pane shows.
    let panes_top_left = ui.cursor_screen_pos();
    let remote_pos = panes_top_left;
    let connector_rb_pos = [remote_pos[0] + pane_w, remote_pos[1]];
    let base_pos = [connector_rb_pos[0] + CONNECTOR_W, remote_pos[1]];
    let connector_bl_pos = [base_pos[0] + pane_w, base_pos[1]];
    let local_pos = [connector_bl_pos[0] + CONNECTOR_W, base_pos[1]];

    let hover_panes: [Cell<Option<(u32, HunkKind, [f32; 2])>>; 3] =
        [Cell::new(None), Cell::new(None), Cell::new(None)];
    let focus_event: Cell<Option<crate::app::FocusedPane>> = Cell::new(None);

    // Snapshot last frame's targets before any render_pane call mutates
    // them. Sync detection compares targets (not eased displayed scroll)
    // so a single user gesture fires sync exactly once instead of every
    // animation frame.
    let prev_targets = state.target;

    let (_remote_rect, remote_scroll, remote_origin) = render_pane(
        ui, state, remote_pos, pane_w, pane_h, Pane::Remote, session_id,
        pending_edits, &remote_layout, &hover_panes[2], &focus_event,
        remote_highlights, lh,
    );

    ui.set_cursor_screen_pos(connector_rb_pos);
    ui.invisible_button("merge_connector_rb", [CONNECTOR_W, pane_h]);

    let (_base_rect, base_scroll, base_origin) = render_pane(
        ui, state, base_pos, pane_w, pane_h, Pane::Base, session_id,
        pending_edits, &base_layout, &hover_panes[0], &focus_event,
        base_highlights, lh,
    );

    ui.set_cursor_screen_pos(connector_bl_pos);
    ui.invisible_button("merge_connector_bl", [CONNECTOR_W, pane_h]);

    let (_local_rect, local_scroll, local_origin) = render_pane(
        ui, state, local_pos, pane_w, pane_h, Pane::Local, session_id,
        pending_edits, &local_layout, &hover_panes[1], &focus_event,
        local_highlights, lh,
    );

    if let Some(p) = focus_event.get() {
        *focus_request = Some(p);
    }

    let view_hs = [pane_h, pane_h, pane_h];
    sync_scrolls(
        state,
        prev_targets,
        view_hs,
        &base_layout.ranges,
        &local_layout.ranges,
        &remote_layout.ranges,
    );

    // Draw bezier connectors on top, *after* the panes have all rendered.
    // origin y is the top of each pane's text widget in screen space; the
    // y0 of each pane's connector strip is the same. Add the multiline's
    // frame_padding.y so ribbons line up with the text rows (matching the
    // offset `paint_text_lines` and `paint_gutter` apply).
    let pane_text_padding_y = ui.clone_style().frame_padding[1];
    draw_connector(
        ui,
        connector_rb_pos,
        CONNECTOR_W,
        pane_h,
        remote_origin[1] + pane_text_padding_y - remote_scroll,
        base_origin[1] + pane_text_padding_y - base_scroll,
        &remote_layout.ranges,
        &base_layout.ranges,
        &remote_layout.line_ys,
        &base_layout.line_ys,
        anchors.iter().map(|a| (a.remote, a.base)).collect::<Vec<_>>().as_slice(),
        hunks,
        lh,
    );
    draw_connector(
        ui,
        connector_bl_pos,
        CONNECTOR_W,
        pane_h,
        base_origin[1] + pane_text_padding_y - base_scroll,
        local_origin[1] + pane_text_padding_y - local_scroll,
        &base_layout.ranges,
        &local_layout.ranges,
        &base_layout.line_ys,
        &local_layout.line_ys,
        anchors.iter().map(|a| (a.base, a.local)).collect::<Vec<_>>().as_slice(),
        hunks,
        lh,
    );

    // Hover overlay panels. Drawn last so they sit above panes + connectors.
    // The hover_panes index maps to the role of the pane the cursor is over;
    // each overlay shows a single icon-button that picks *that* pane's
    // version as the resolution.
    for (i, cell) in hover_panes.iter().enumerate() {
        if let Some((hunk_id, kind, pos)) = cell.get() {
            let pane_side = match i {
                0 => crate::session::ThreeWaySide::Base,
                1 => crate::session::ThreeWaySide::Local,
                2 => crate::session::ThreeWaySide::Remote,
                _ => continue,
            };
            draw_control_overlay(
                ui, store, session_id, hunk_id, kind, pane_side, status, pos, lh,
            );
        }
    }

    // Reserve space so subsequent widgets land below the panes.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + pane_h]);
}

/// Returns (widget_rect, scroll_y, content_origin_screen_pos).
#[allow(clippy::too_many_arguments)]
fn render_pane(
    ui: &Ui,
    state: &mut MergeViewState,
    pane_pos: [f32; 2],
    pane_w: f32,
    pane_h: f32,
    pane: Pane,
    session_id: SessionId,
    pending_edits: &mut Vec<DiffEdit>,
    layout: &PaneLayout,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    highlights: &[LineSpans],
    lh: f32,
) -> ([f32; 4], f32, [f32; 2]) {
    let g_w = gutter_w();
    let widget_pos = [pane_pos[0] + g_w, pane_pos[1]];
    let widget_w = (pane_w - g_w).max(20.0);

    let pending_scroll = state.pending[pane as usize].take();

    let buf_ref: &str = match pane {
        Pane::Base => &state.base_buf,
        Pane::Local => &state.local_buf,
        Pane::Remote => &state.remote_buf,
    };
    let n = buf_ref.lines().count().max(1);
    let trailing = buf_ref.is_empty() || buf_ref.ends_with('\n');
    let buf_line_count = n + if trailing { 1 } else { 0 };
    let content_h = (buf_line_count as f32) * lh;
    let max_scroll = (content_h - pane_h).max(0.0);

    // First-frame focus: resolve the stored initial line to a pending
    // scroll now that `lh` and `max_scroll` are known. Overrides whatever
    // else was pending on this pane (the view was just created — nothing
    // else should be).
    let pending_scroll = if let Some(line) = state.pending_initial_line[pane as usize].take() {
        const TOP_MARGIN_LINES: f32 = 2.0;
        let y = ((line.max(1) - 1) as f32 - TOP_MARGIN_LINES).max(0.0) * lh;
        Some(y.min(max_scroll))
    } else {
        pending_scroll
    };

    // Split wheel into vertical (smooth, fed to inner multiline) and
    // horizontal (pinned onto the outer scroll child). See diff_view
    // for the reasoning — imgui's UpdateMouseWheel won't bubble through
    // the inner multiline's child window to scroll our outer wrapper.
    let hovered = ui.is_mouse_hovering_rect(
        [widget_pos[0], widget_pos[1]],
        [widget_pos[0] + widget_w, widget_pos[1] + pane_h],
    );
    let (wheel, h_wheel) = if hovered {
        let raw_v = ui.io().mouse_wheel;
        let raw_h = ui.io().mouse_wheel_h;
        if ui.io().key_shift && raw_v != 0.0 {
            (0.0, raw_h + raw_v)
        } else {
            (raw_v, raw_h)
        }
    } else {
        (0.0, 0.0)
    };
    let prev_target = state.target[pane as usize];
    let prev_displayed = state.last[pane as usize];

    // Custom vertical scrollbar — drag handling. The inner multiline's native
    // scrollbar lives at x = inner_pos + inner_w, which clips out whenever
    // inner_w > widget_w. We hide it (ScrollbarSize=0 below) and paint our own
    // at widget_rect's right edge. See diff_view::render_pane for the same
    // pattern.
    let track_top = widget_pos[1];
    let track_h = pane_h;
    let (prev_thumb_y, thumb_h) = crate::app::diff_view::vbar_thumb_geom(
        track_top, track_h, prev_displayed, content_h,
    );
    let vbar_x_r = widget_pos[0] + widget_w;
    let vbar_x_l = vbar_x_r - crate::app::diff_view::VBAR_W;
    let mouse = ui.io().mouse_pos;
    let in_x = mouse[0] >= vbar_x_l && mouse[0] <= vbar_x_r;
    let in_thumb = in_x && mouse[1] >= prev_thumb_y && mouse[1] <= prev_thumb_y + thumb_h;
    let in_track = in_x && mouse[1] >= track_top && mouse[1] <= track_top + track_h;
    let mouse_down = ui.is_mouse_down(imgui::MouseButton::Left);
    let mouse_clicked = ui.is_mouse_clicked(imgui::MouseButton::Left);
    let drag_slot = &mut state.vbar_drag[pane as usize];
    let mut drag_override: Option<f32> = None;
    if content_h > track_h {
        if let Some(off) = *drag_slot {
            if mouse_down {
                drag_override = Some(crate::app::diff_view::vbar_scroll_for_thumb_y(
                    mouse[1] - off,
                    track_top,
                    track_h,
                    thumb_h,
                    content_h,
                ));
            } else {
                *drag_slot = None;
            }
        } else if mouse_clicked && in_thumb {
            *drag_slot = Some(mouse[1] - prev_thumb_y);
        } else if mouse_clicked && in_track {
            let off = thumb_h * 0.5;
            *drag_slot = Some(off);
            drag_override = Some(crate::app::diff_view::vbar_scroll_for_thumb_y(
                mouse[1] - off,
                track_top,
                track_h,
                thumb_h,
                content_h,
            ));
        }
    } else {
        *drag_slot = None;
    }
    let dragging = drag_slot.is_some();
    // True for any frame the user is interacting with the custom scrollbar.
    // Used below to suppress the multiline's mouse input — `set_item_allow_overlap`
    // on a later invisible_button can't out-race the multiline for ActiveID,
    // so we stomp io.MouseDown/Clicked around the multiline build instead.
    let scrollbar_grabbing = dragging || (mouse_clicked && in_track);

    let mut target = if let Some(s) = drag_override {
        s
    } else {
        pending_scroll.unwrap_or(prev_target - wheel * lh * SCROLL_LINES_PER_WHEEL_TICK)
    };
    if target < 0.0 {
        target = 0.0;
    }
    if target > max_scroll {
        target = max_scroll;
    }
    state.target[pane as usize] = target;

    let displayed = if drag_override.is_some() {
        target
    } else {
        // Clamp dt to ~one 30 fps frame so the wake-up frame after an idle
        // event-loop park doesn't collapse the easing into a single jump.
        // See diff_view::render_pane for the longer explanation.
        let dt = ui.io().delta_time.max(0.0).min(0.033);
        let k = 1.0 - (-dt * SCROLL_SMOOTH_SPEED).exp();
        let mut d = prev_displayed + (target - prev_displayed) * k;
        if (target - d).abs() < SCROLL_SNAP_EPSILON {
            d = target;
        }
        d
    };

    let max_line_w = state.max_line_w[pane as usize];
    let style = ui.clone_style();
    let inner_w = (max_line_w + style.frame_padding[0] * 2.0 + 8.0).max(widget_w);

    ui.set_cursor_screen_pos(widget_pos);

    let widget_id = format!("##merge_pane_{:?}_e{}", pane, state.input_epoch);
    let outer_id = format!("##merge_pane_outer_{:?}_e{}", pane, state.input_epoch);

    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + pane_h,
    ];

    let caret_byte: Cell<i32> = Cell::new(-1);
    let origin_out: [f32; 2] = widget_pos;
    let scroll_x_cell: Cell<f32> = Cell::new(state.scroll_x[pane as usize]);
    let widget_active_cell: Cell<bool> = Cell::new(false);
    let new_buf_cell: Cell<Option<String>> = Cell::new(None);

    // Pin the outer child's horizontal scroll. We own it directly since
    // imgui's wheel routing won't bubble through the inner multiline.
    let char_step_x = ui.calc_text_size("m")[0].max(1.0);
    let target_scroll_x = (state.scroll_x[pane as usize]
        - h_wheel * char_step_x * SCROLL_LINES_PER_WHEEL_TICK)
        .max(0.0);
    unsafe {
        imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
            x: target_scroll_x,
            y: -1.0,
        });
    }

    let _wp = ui.push_style_var(StyleVar::WindowPadding([0.0, 0.0]));
    let _cbg = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0, 0.0, 0.0, 0.0]);
    {
        let buf: &mut String = match pane {
            Pane::Base => &mut state.base_buf,
            Pane::Local => &mut state.local_buf,
            Pane::Remote => &mut state.remote_buf,
        };
        ui.child_window(&outer_id)
            .size([widget_w, pane_h])
            .horizontal_scrollbar(true)
            .build(|| {
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
                        x: -1.0,
                        y: displayed,
                    });
                }
                let _spacing = ui.push_style_var(StyleVar::ItemSpacing([0.0, 0.0]));
                let _frame_bg = ui.push_style_color(imgui::StyleColor::FrameBg, [0.0, 0.0, 0.0, 0.0]);
                let _frame_bg_hov = ui.push_style_color(imgui::StyleColor::FrameBgHovered, [0.0, 0.0, 0.0, 0.0]);
                let _frame_bg_act = ui.push_style_color(imgui::StyleColor::FrameBgActive, [0.0, 0.0, 0.0, 0.0]);
                let _text_color = ui.push_style_color(imgui::StyleColor::Text, [0.0, 0.0, 0.0, 0.0]);

                struct CaretCapture<'a> {
                    cursor: &'a Cell<i32>,
                }
                impl<'a> imgui::InputTextCallbackHandler for CaretCapture<'a> {
                    fn on_always(&mut self, data: imgui::TextCallbackData) {
                        self.cursor.set(data.cursor_pos() as i32);
                    }
                }

                // Shrink the inner multiline's native vertical scrollbar to
                // 1px — see diff_view::render_pane for the full reasoning
                // (0.0 trips an imgui assertion; the 1px strip sits past
                // the outer's clip rect so it's invisible). We paint our
                // own thumb on the outer's fixed right edge after this
                // child closes.
                let _sb = ui.push_style_var(StyleVar::ScrollbarSize(1.0));
                // Disable the multiline entirely while the user is dragging
                // our scrollbar — see diff_view::render_pane. Visuals are
                // unaffected because we paint text/caret ourselves.
                unsafe { imgui::sys::igBeginDisabled(scrollbar_grabbing) };
                let changed = ui
                    .input_text_multiline(&widget_id, buf, [inner_w, pane_h])
                    .no_undo_redo(true)
                    .callback(
                        imgui::InputTextMultilineCallback::ALWAYS,
                        CaretCapture { cursor: &caret_byte },
                    )
                    .build();
                unsafe { imgui::sys::igEndDisabled() };
                drop(_sb);
                ui.set_item_allow_overlap();
                widget_active_cell.set(ui.is_item_active());
                if changed {
                    new_buf_cell.set(Some(buf.clone()));
                }
                unsafe {
                    scroll_x_cell.set(imgui::sys::igGetScrollX());
                }
            });
    }
    drop(_cbg);
    drop(_wp);

    let widget_active = widget_active_cell.get();
    let scroll_y_out = displayed;
    state.last[pane as usize] = displayed;
    let scroll_x_out = scroll_x_cell.get();

    // Caret-tracking horizontal scroll: only fire when the caret
    // actually moved this frame. Without this gating, wheel-scrolling
    // away from a stationary caret would snap right back every frame.
    let cur_caret = caret_byte.get();
    let prev_caret = state.last_caret[pane as usize];
    let caret_moved = widget_active && cur_caret >= 0 && prev_caret != Some(cur_caret);
    let next_scroll_x = if caret_moved {
        let buf_ref: &str = match pane {
            Pane::Base => &state.base_buf,
            Pane::Local => &state.local_buf,
            Pane::Remote => &state.remote_buf,
        };
        let caret_x = crate::app::diff_view::caret_x_in_inner(
            buf_ref,
            cur_caret as usize,
            ui,
            style.frame_padding[0],
        );
        crate::app::diff_view::track_caret_scroll_x(
            caret_x,
            scroll_x_out,
            widget_w,
            char_step_x * 2.0,
        )
    } else {
        scroll_x_out
    };
    state.scroll_x[pane as usize] = next_scroll_x;
    state.last_caret[pane as usize] = if widget_active && cur_caret >= 0 {
        Some(cur_caret)
    } else {
        None
    };
    if widget_active {
        focus_event.set(Some(pane.as_focused_pane()));
    }
    if let Some(new_text) = new_buf_cell.take() {
        let side_ref = SideRef::ThreeWay(match pane {
            Pane::Base => ThreeWaySide::Base,
            Pane::Local => ThreeWaySide::Local,
            Pane::Remote => ThreeWaySide::Remote,
        });
        pending_edits.push(DiffEdit::SetSide {
            session_id,
            side: side_ref,
            new_text,
            old_text: None,
        });
    }

    let buf_for_paint: &str = match pane {
        Pane::Base => &state.base_buf,
        Pane::Local => &state.local_buf,
        Pane::Remote => &state.remote_buf,
    };
    paint_pane_text(
        ui,
        widget_rect,
        buf_for_paint,
        layout,
        scroll_y_out,
        scroll_x_out,
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
        highlights,
    );

    // Gutter on the left of this pane (line numbers).
    paint_gutter(ui, pane_pos, g_w, pane_h, scroll_y_out, lh, buf_line_count as u32, layout);

    // Custom vertical scrollbar with a minimap layer painted from the pane's
    // hunks — band colors match the per-row tints used by `tint_for_line`.
    let bands: Vec<crate::app::diff_view::MinimapBand> = layout
        .hunks
        .iter()
        .filter_map(|(_id, kind, lo, hi)| {
            let color = match (*kind)? {
                HunkKind::LocalOnly => theme::with_alpha(theme::GREEN(), 0.85),
                HunkKind::RemoteOnly => theme::with_alpha(theme::SAPPHIRE(), 0.85),
                HunkKind::Conflict => [0.55, 0.18, 0.18, 0.85],
            };
            Some(crate::app::diff_view::MinimapBand { line_lo: *lo, line_hi: *hi, color })
        })
        .collect();
    crate::app::diff_view::paint_vbar(
        ui,
        widget_rect,
        scroll_y_out,
        content_h,
        dragging,
        &bands,
        buf_line_count as u32,
    );

    // Restore Arrow cursor over the scrollbar / while dragging it.
    if content_h > pane_h && (in_track || dragging) {
        ui.set_mouse_cursor(Some(imgui::MouseCursor::Arrow));
    }

    (widget_rect, scroll_y_out, origin_out)
}

fn paint_gutter(
    ui: &Ui,
    pane_pos: [f32; 2],
    g_w: f32,
    pane_h: f32,
    scroll_y: f32,
    lh: f32,
    line_count: u32,
    layout: &PaneLayout,
) {
    let dl = ui.get_window_draw_list();
    if lh <= 0.0 {
        return;
    }
    let g_top = pane_pos[1];
    let g_bottom = pane_pos[1] + pane_h;
    let g_left = pane_pos[0];
    let g_right = g_left + g_w;
    // Match the multiline's frame_padding.y offset used by `paint_text_lines`
    // so gutter row backgrounds align with the code-row tints.
    let padding_y = ui.clone_style().frame_padding[1];
    // Match the per-row tint built by `tint_for_line` in `paint_pane_text`
    // so the gutter background flows continuously into the code row.
    let tint_for_line = |ln: u32| -> Option<[f32; 4]> {
        for (_id, kind, lo, hi) in &layout.hunks {
            let Some(kind_v) = *kind else { continue };
            if ln >= *lo && ln <= *hi {
                return Some(match kind_v {
                    HunkKind::LocalOnly => theme::with_alpha(theme::GREEN(), 0.22),
                    HunkKind::RemoteOnly => theme::with_alpha(theme::SAPPHIRE(), 0.22),
                    HunkKind::Conflict => [0.55, 0.18, 0.18, 0.30],
                });
            }
        }
        None
    };
    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + pane_h) / lh).ceil() as u32 + 1;
    // Clip to the gutter rect so partial-row line numbers don't bleed
    // into the filename header above when `scroll_y` falls between lines.
    dl.with_clip_rect_intersect([g_left, g_top], [g_right, g_bottom], || {
        for line in first_line..=last_line.min(line_count) {
            let y = g_top + padding_y + (line as f32 - 1.0) * lh - scroll_y;
            if y + lh < g_top || y > g_bottom {
                continue;
            }
            let y0 = y.max(g_top);
            let y1 = (y + lh).min(g_bottom);
            if let Some(color) = tint_for_line(line) {
                if y1 > y0 {
                    dl.add_rect([g_left, y0], [g_right, y1], color)
                        .filled(true)
                        .build();
                }
            }
            let text = format!("{line}");
            let text_w = ui.calc_text_size(&text)[0];
            dl.add_text([g_left + g_w - 4.0 - text_w, y + 2.0], theme::OVERLAY1(), &text);
        }
    });
}

/// Paint everything for one merge pane on the foreground draw list:
/// row tints (LocalOnly/RemoteOnly/Conflict), text, caret. Also detects
/// hover for the resolution overlay panel.
#[allow(clippy::too_many_arguments)]
fn paint_pane_text(
    ui: &Ui,
    widget_rect: [f32; 4],
    buf: &str,
    layout: &PaneLayout,
    scroll_y: f32,
    scroll_x: f32,
    lh: f32,
    caret_byte: i32,
    widget_active: bool,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
    highlights: &[LineSpans],
) {
    let widget_top = widget_rect[1];
    let widget_bottom = widget_rect[3];
    let widget_left = widget_rect[0];
    let widget_right = widget_rect[2];
    let widget_h = widget_bottom - widget_top;
    if widget_h <= 0.0 || lh <= 0.0 {
        return;
    }

    let style = ui.clone_style();
    let padding_x = style.frame_padding[0];
    let padding_y = style.frame_padding[1];

    // Per-line tint lookup keyed on the layout's hunk ranges.
    let tint_for_line = |ln: u32| -> Option<[f32; 4]> {
        for (_id, kind, lo, hi) in &layout.hunks {
            let Some(kind_v) = *kind else { continue };
            if ln >= *lo && ln <= *hi {
                return Some(match kind_v {
                    HunkKind::LocalOnly => theme::with_alpha(theme::GREEN(), 0.22),
                    HunkKind::RemoteOnly => theme::with_alpha(theme::SAPPHIRE(), 0.22),
                    // Match the 2-way Delete row tint so "removal" and
                    // "conflict" read as the same red across both views.
                    HunkKind::Conflict => [0.55, 0.18, 0.18, 0.30],
                });
            }
        }
        None
    };

    syntax_paint::paint_text_lines(
        ui,
        widget_rect,
        buf,
        highlights,
        scroll_x,
        scroll_y,
        padding_x,
        padding_y,
        lh,
        |dl, _line_idx, _line_text, ln, y0, y1| {
            if let Some(bg) = tint_for_line(ln) {
                if y1 > y0 {
                    dl.add_rect([widget_left, y0], [widget_right, y1], bg)
                        .filled(true)
                        .build();
                }
            }
        },
    );

    if widget_active {
        let dl = ui.get_window_draw_list();
        syntax_paint::paint_caret(
            ui,
            &dl,
            [widget_left, widget_top, widget_right, widget_bottom],
            buf,
            caret_byte,
            scroll_x,
            scroll_y,
            padding_x,
            padding_y,
            lh,
        );
    }

    // Hover detection.
    let mouse_pos = ui.io().mouse_pos;
    let mx = mouse_pos[0];
    let my = mouse_pos[1];
    if mx >= widget_left && mx <= widget_right && my >= widget_top && my <= widget_bottom {
        let content_y = (my - widget_top) + scroll_y;
        let line = (content_y / lh).floor() as i64;
        let line = line.max(0) as u32 + 1;
        for (hid, kind, lo, hi) in &layout.hunks {
            let Some(kind_v) = *kind else { continue };
            if line >= *lo && line <= *hi {
                let anchor_y = (widget_top + (*lo as f32 - 1.0) * lh - scroll_y).max(widget_top);
                hover_out.set(Some((*hid, kind_v, [widget_left, anchor_y])));
                break;
            }
        }
    }
}

fn draw_control_overlay(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    hunk_id: u32,
    kind: HunkKind,
    pane_side: crate::session::ThreeWaySide,
    status: &mut String,
    pos: [f32; 2],
    lh: f32,
) {
    let _ = lh;
    let panel_x = pos[0] + 4.0;
    let panel_y = pos[1] + 2.0;
    let border_color = match kind {
        HunkKind::LocalOnly => theme::BLUE(),
        HunkKind::RemoteOnly => theme::MAUVE(),
        HunkKind::Conflict => theme::PEACH(),
    };

    // See diff_view::overlay::draw_control_overlay: the panel needs to be
    // its own top-level imgui window so its button can receive clicks
    // over the input_text_multiline pane's child window.
    let _pad = ui.push_style_var(StyleVar::FramePadding([4.0, 2.0]));
    let _win_pad = ui.push_style_var(StyleVar::WindowPadding([3.0, 3.0]));
    let _win_round = ui.push_style_var(StyleVar::WindowRounding(4.0));
    let _win_border = ui.push_style_var(StyleVar::WindowBorderSize(1.0));
    let _border_col = ui.push_style_color(imgui::StyleColor::Border, border_color);
    let _bg_col = ui.push_style_color(
        imgui::StyleColor::WindowBg,
        theme::with_alpha(theme::MANTLE(), 0.95),
    );

    let kind_tag = match kind {
        HunkKind::LocalOnly => "L",
        HunkKind::RemoteOnly => "R",
        HunkKind::Conflict => "C",
    };
    let win_name = format!("##merge_overlay_{}_{}_{}", session_id, hunk_id, kind_tag);
    let flags = imgui::WindowFlags::NO_TITLE_BAR
        | imgui::WindowFlags::NO_RESIZE
        | imgui::WindowFlags::NO_MOVE
        | imgui::WindowFlags::NO_SCROLLBAR
        | imgui::WindowFlags::NO_COLLAPSE
        | imgui::WindowFlags::ALWAYS_AUTO_RESIZE
        | imgui::WindowFlags::NO_SAVED_SETTINGS
        | imgui::WindowFlags::NO_FOCUS_ON_APPEARING
        | imgui::WindowFlags::NO_NAV;
    ui.window(&win_name)
        .position([panel_x, panel_y], imgui::Condition::Always)
        .flags(flags)
        .build(|| {
            use crate::session::ThreeWaySide;
            let resolution = match pane_side {
                ThreeWaySide::Remote => Resolution::Remote,
                ThreeWaySide::Base => Resolution::Base,
                ThreeWaySide::Local => Resolution::Local,
            };
            const ICON_HALF: f32 = 7.0;
            const BTN_W: f32 = 22.0;
            const BTN_H: f32 = 18.0;
            let btn_origin = ui.cursor_screen_pos();
            let btn_id = format!("##ovUse_{hunk_id}");
            let clicked = ui.invisible_button(btn_id, [BTN_W, BTN_H]);
            let hovered = ui.is_item_hovered();
            let active = ui.is_item_active();
            let dl = ui.get_window_draw_list();
            if hovered || active {
                let style = ui.clone_style();
                let bg_col = if active {
                    style.colors[imgui::StyleColor::ButtonActive as usize]
                } else {
                    style.colors[imgui::StyleColor::ButtonHovered as usize]
                };
                dl.add_rect(
                    btn_origin,
                    [btn_origin[0] + BTN_W, btn_origin[1] + BTN_H],
                    bg_col,
                )
                .filled(true)
                .rounding(style.frame_rounding)
                .build();
            }
            let cx = btn_origin[0] + BTN_W * 0.5;
            let cy = btn_origin[1] + BTN_H * 0.5;
            crate::app::result_pane::paint_role_icon(ui, [cx, cy], pane_side, ICON_HALF);
            if clicked {
                apply_res(store, session_id, hunk_id, resolution, status);
            }
        });
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

// ----------------- connector & sync (mostly unchanged from pre-rewrite) ------

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
        MergeHunk::Stable { .. } => theme::with_alpha(theme::OVERLAY1(), 0.10),
        MergeHunk::LocalOnly { .. } => theme::with_alpha(theme::GREEN(), 0.28),
        MergeHunk::RemoteOnly { .. } => theme::with_alpha(theme::SAPPHIRE(), 0.28),
        MergeHunk::Conflict { .. } => [0.55, 0.18, 0.18, 0.32],
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
    lh: f32,
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
            let ly = left_origin_y + ly_content + lh * 0.5;
            let ry = right_origin_y + ry_content + lh * 0.5;
            if (ly < band_top && ry < band_top) || (ly > band_bot && ry > band_bot) {
                continue;
            }
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST(), 3.0);
        }
    });
    let _ = dl;
}

fn sync_scrolls(
    state: &mut MergeViewState,
    prev_targets: [f32; 3],
    view_h: [f32; 3],
    base_ranges: &[(u32, f32, f32)],
    local_ranges: &[(u32, f32, f32)],
    remote_ranges: &[(u32, f32, f32)],
) {
    let ranges = [base_ranges, local_ranges, remote_ranges];
    let mut driver: Option<usize> = None;
    for i in 0..3 {
        if (state.target[i] - prev_targets[i]).abs() > ECHO_TOLERANCE {
            driver = Some(i);
            break;
        }
    }
    if let Some(src) = driver {
        for dst in 0..3 {
            if dst == src {
                continue;
            }
            if let Some(target) = target_scroll(
                state.target[src],
                view_h[src],
                view_h[dst],
                ranges[src],
                ranges[dst],
            ) {
                state.pending[dst] = Some(target);
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
