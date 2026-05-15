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
use crate::merge::{MergeAnchor, MergeHunk, Resolution};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, ThreeWaySide};

/// Match diff_view: tall enough for the 1.5x Roboto Mono used in code rows
/// at zoom=1.0.
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
    /// Pending scroll value to apply next frame on a given pane.
    pending: [Option<f32>; 3],
    /// Bumped on external buffer mutations (undo/redo, Apply Local/Base/Remote);
    /// mixed into the widget ID so imgui re-initialises stb_textedit from `buf`.
    pub input_epoch: u32,
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
) {
    // Sync buffers from session at frame start.
    let snap = match store.snapshot(session_id) {
        Ok(s) => s,
        Err(_) => return,
    };
    let SessionMode::ThreeWay { base_text, local_text, remote_text, .. } = &snap.mode else {
        return;
    };
    if state.base_buf != *base_text {
        state.base_buf = base_text.clone();
    }
    if state.local_buf != *local_text {
        state.local_buf = local_text.clone();
    }
    if state.remote_buf != *remote_text {
        state.remote_buf = remote_text.clone();
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

    let base_layout = build_layout(hunks, Pane::Base, lh);
    let local_layout = build_layout(hunks, Pane::Local, lh);
    let remote_layout = build_layout(hunks, Pane::Remote, lh);

    let panes_top_left = ui.cursor_screen_pos();
    let base_pos = panes_top_left;
    let connector_bl_pos = [base_pos[0] + pane_w, base_pos[1]];
    let local_pos = [connector_bl_pos[0] + CONNECTOR_W, base_pos[1]];
    let connector_lr_pos = [local_pos[0] + pane_w, base_pos[1]];
    let remote_pos = [connector_lr_pos[0] + CONNECTOR_W, base_pos[1]];

    let hover_panes: [Cell<Option<(u32, HunkKind, [f32; 2])>>; 3] =
        [Cell::new(None), Cell::new(None), Cell::new(None)];
    let focus_event: Cell<Option<crate::app::FocusedPane>> = Cell::new(None);

    // Snapshot last frame's targets before any render_pane call mutates
    // them. Sync detection compares targets (not eased displayed scroll)
    // so a single user gesture fires sync exactly once instead of every
    // animation frame.
    let prev_targets = state.target;

    let (_base_rect, base_scroll, base_origin) = render_pane(
        ui, state, base_pos, pane_w, pane_h, Pane::Base, session_id,
        pending_edits, &base_layout, &hover_panes[0], &focus_event, lh,
    );

    // Connector BASE↔LOCAL: empty area for the bezier ribbons.
    ui.set_cursor_screen_pos(connector_bl_pos);
    ui.invisible_button("merge_connector_bl", [CONNECTOR_W, pane_h]);

    let (_local_rect, local_scroll, local_origin) = render_pane(
        ui, state, local_pos, pane_w, pane_h, Pane::Local, session_id,
        pending_edits, &local_layout, &hover_panes[1], &focus_event, lh,
    );

    ui.set_cursor_screen_pos(connector_lr_pos);
    ui.invisible_button("merge_connector_lr", [CONNECTOR_W, pane_h]);

    let (_remote_rect, remote_scroll, remote_origin) = render_pane(
        ui, state, remote_pos, pane_w, pane_h, Pane::Remote, session_id,
        pending_edits, &remote_layout, &hover_panes[2], &focus_event, lh,
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
    // y0 of each pane's connector strip is the same.
    draw_connector(
        ui,
        connector_bl_pos,
        CONNECTOR_W,
        pane_h,
        base_origin[1] - base_scroll,
        local_origin[1] - local_scroll,
        &base_layout.ranges,
        &local_layout.ranges,
        &base_layout.line_ys,
        &local_layout.line_ys,
        anchors.iter().map(|a| (a.base, a.local)).collect::<Vec<_>>().as_slice(),
        hunks,
        lh,
    );
    draw_connector(
        ui,
        connector_lr_pos,
        CONNECTOR_W,
        pane_h,
        local_origin[1] - local_scroll,
        remote_origin[1] - remote_scroll,
        &local_layout.ranges,
        &remote_layout.ranges,
        &local_layout.line_ys,
        &remote_layout.line_ys,
        anchors.iter().map(|a| (a.local, a.remote)).collect::<Vec<_>>().as_slice(),
        hunks,
        lh,
    );

    // Hover overlay panels. Drawn last so they sit above panes + connectors.
    for (i, cell) in hover_panes.iter().enumerate() {
        if let Some((hunk_id, kind, pos)) = cell.get() {
            let _ = i;
            draw_control_overlay(ui, store, session_id, hunk_id, kind, status, pos, lh);
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

    // Wheel input: only the hovered pane consumes io.mouse_wheel for its
    // own target. imgui's child window also sees the wheel internally, but
    // we pin its scroll via igSetNextWindowScroll below so any internal
    // movement is overwritten next frame.
    let wheel = if ui.is_mouse_hovering_rect(
        [widget_pos[0], widget_pos[1]],
        [widget_pos[0] + widget_w, widget_pos[1] + pane_h],
    ) {
        ui.io().mouse_wheel
    } else {
        0.0
    };
    let prev_target = state.target[pane as usize];
    let mut target = pending_scroll
        .unwrap_or(prev_target - wheel * lh * SCROLL_LINES_PER_WHEEL_TICK);
    if target < 0.0 {
        target = 0.0;
    }
    if target > max_scroll {
        target = max_scroll;
    }
    state.target[pane as usize] = target;

    let prev_displayed = state.last[pane as usize];
    let dt = ui.io().delta_time.max(0.0).min(0.1);
    let k = 1.0 - (-dt * SCROLL_SMOOTH_SPEED).exp();
    let mut displayed = prev_displayed + (target - prev_displayed) * k;
    if (target - displayed).abs() < SCROLL_SNAP_EPSILON {
        displayed = target;
    }

    // Pin the multiline's internal child scroll to our value every frame
    // so the rendered text aligns with our overlay. The widget IS the next
    // window for purposes of igSetNextWindowScroll. Imgui may still process
    // the wheel internally; our override the next frame wins.
    unsafe {
        imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x: -1.0, y: displayed });
    }

    ui.set_cursor_screen_pos(widget_pos);

    let widget_id = format!("##merge_pane_{:?}_e{}", pane, state.input_epoch);

    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + pane_h,
    ];

    let caret_byte: Cell<i32> = Cell::new(-1);
    let origin_out: [f32; 2] = widget_pos;

    let (changed, new_text_opt, widget_active) = {
        let buf = match pane {
            Pane::Base => &mut state.base_buf,
            Pane::Local => &mut state.local_buf,
            Pane::Remote => &mut state.remote_buf,
        };
        let _spacing = ui.push_style_var(StyleVar::ItemSpacing([0.0, 0.0]));

        // Suppress imgui's own FrameBg + Text rendering — we paint
        // everything (row tints, text, caret) on the foreground draw list.
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

        let changed = ui
            .input_text_multiline(&widget_id, buf, [widget_w, pane_h])
            .no_undo_redo(true)
            .callback(
                imgui::InputTextMultilineCallback::ALWAYS,
                CaretCapture { cursor: &caret_byte },
            )
            .build();
        let active = ui.is_item_active();
        let new_text = if changed { Some(buf.clone()) } else { None };
        (changed, new_text, active)
    };
    let scroll_y_out = displayed;
    state.last[pane as usize] = displayed;
    if widget_active {
        focus_event.set(Some(pane.as_focused_pane()));
    }
    if let Some(new_text) = new_text_opt {
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
    let _ = changed;

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
        lh,
        caret_byte.get(),
        widget_active,
        hover_out,
    );

    // Gutter on the left of this pane (line numbers).
    paint_gutter(ui, pane_pos, g_w, pane_h, scroll_y_out, lh, buf_line_count as u32);

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
) {
    let dl = ui.get_window_draw_list();
    if lh <= 0.0 {
        return;
    }
    let g_top = pane_pos[1];
    let g_bottom = pane_pos[1] + pane_h;
    let g_left = pane_pos[0];
    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + pane_h) / lh).ceil() as u32 + 1;
    for line in first_line..=last_line.min(line_count) {
        let y = g_top + (line as f32 - 1.0) * lh - scroll_y;
        if y + lh < g_top || y > g_bottom {
            continue;
        }
        let text = format!("{line}");
        let text_w = ui.calc_text_size(&text)[0];
        dl.add_text([g_left + g_w - 4.0 - text_w, y + 2.0], theme::OVERLAY1, &text);
    }
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
    lh: f32,
    caret_byte: i32,
    widget_active: bool,
    hover_out: &Cell<Option<(u32, HunkKind, [f32; 2])>>,
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

    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + widget_h) / lh).ceil() as u32 + 1;

    // Build a fast line-no -> tint lookup for visible lines.
    let tint_for_line = |ln: u32| -> Option<[f32; 4]> {
        for (_id, kind, lo, hi) in &layout.hunks {
            let Some(kind_v) = *kind else { continue };
            if ln >= *lo && ln <= *hi {
                return Some(match kind_v {
                    HunkKind::LocalOnly => theme::with_alpha(theme::BLUE, 0.22),
                    HunkKind::RemoteOnly => theme::with_alpha(theme::MAUVE, 0.22),
                    HunkKind::Conflict => theme::with_alpha(theme::PEACH, 0.30),
                });
            }
        }
        None
    };

    let dl = ui.get_window_draw_list();
    dl.with_clip_rect([widget_left, widget_top], [widget_right, widget_bottom], || {
        for (line_idx, line_text) in buf.lines().enumerate() {
            let ln = (line_idx as u32) + 1;
            if ln < first_line || ln > last_line {
                continue;
            }
            let y = widget_top + padding_y + (ln as f32 - 1.0) * lh - scroll_y;
            if y + lh < widget_top || y > widget_bottom {
                continue;
            }
            let y0 = y.max(widget_top);
            let y1 = (y + lh).min(widget_bottom);

            if let Some(bg) = tint_for_line(ln) {
                if y1 > y0 {
                    dl.add_rect([widget_left, y0], [widget_right, y1], bg)
                        .filled(true)
                        .build();
                }
            }

            if !line_text.is_empty() {
                dl.add_text(
                    [widget_left + padding_x, y],
                    theme::TEXT,
                    line_text,
                );
            }
        }

        if widget_active && caret_byte >= 0 {
            let blink_on = (ui.time() * 2.0).rem_euclid(2.0) < 1.0;
            if blink_on {
                let target = caret_byte as usize;
                let mut byte_acc: usize = 0;
                let mut painted = false;
                for (line_idx, line_text) in buf.lines().enumerate() {
                    let line_end = byte_acc + line_text.len();
                    if target >= byte_acc && target <= line_end {
                        let local = target - byte_acc;
                        let clamped = local.min(line_text.len());
                        let mut snap = clamped;
                        while snap > 0 && !line_text.is_char_boundary(snap) {
                            snap -= 1;
                        }
                        let x = widget_left + padding_x + ui.calc_text_size(&line_text[..snap])[0];
                        let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
                        if y + lh >= widget_top && y <= widget_bottom {
                            dl.add_line([x, y + 1.0], [x, y + lh - 1.0], theme::TEXT)
                                .thickness(1.0)
                                .build();
                        }
                        painted = true;
                        break;
                    }
                    byte_acc = line_end + 1;
                }
                if !painted && target >= byte_acc {
                    let line_idx = buf.lines().count();
                    let x = widget_left + padding_x;
                    let y = widget_top + padding_y + (line_idx as f32) * lh - scroll_y;
                    if y + lh >= widget_top && y <= widget_bottom {
                        dl.add_line([x, y + 1.0], [x, y + lh - 1.0], theme::TEXT)
                            .thickness(1.0)
                            .build();
                    }
                }
            }
        }
    });

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
    status: &mut String,
    pos: [f32; 2],
    lh: f32,
) {
    let _pad = ui.push_style_var(StyleVar::FramePadding([6.0, 2.0]));
    let _spacing = ui.push_style_var(StyleVar::ItemSpacing([4.0, 0.0]));

    let panel_x = pos[0] + 4.0;
    let panel_y = pos[1] + 2.0;
    let panel_w = 260.0;
    let panel_h = lh - 4.0;

    let dl = ui.get_window_draw_list();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        theme::with_alpha(theme::MANTLE, 0.95),
    )
    .filled(true)
    .rounding(4.0)
    .build();
    let border_color = match kind {
        HunkKind::LocalOnly => theme::BLUE,
        HunkKind::RemoteOnly => theme::MAUVE,
        HunkKind::Conflict => theme::PEACH,
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
        MergeHunk::Stable { .. } => theme::with_alpha(theme::OVERLAY1, 0.10),
        MergeHunk::LocalOnly { .. } => theme::with_alpha(theme::BLUE, 0.28),
        MergeHunk::RemoteOnly { .. } => theme::with_alpha(theme::MAUVE, 0.28),
        MergeHunk::Conflict { .. } => theme::with_alpha(theme::PEACH, 0.32),
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
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST, 3.0);
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
