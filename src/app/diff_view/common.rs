//! 2-way diff view — shared types, constants, and helpers.
//!
//! State, geometry, and pure utility functions used by both `render` and
//! `input`. Public re-exports for this whole submodule come through `mod.rs`.

use std::collections::HashMap;

use imgui::Ui;

use super::super::char_diff::{char_diff, left_segments, right_segments, Segment};
use crate::diff::{DiffOp, Hunk, SubSpan, SubSpanKind};

/// Convert engine-supplied sub-line spans into renderer `Segment`s. Each
/// `Changed` span becomes a highlighted segment and each `Same` span
/// becomes an un-highlighted segment, using byte indices into `text`.
fn segments_from_spans(text: &str, spans: &[SubSpan]) -> Vec<Segment> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        let start = (s.start as usize).min(bytes.len());
        let end = (s.end as usize).min(bytes.len()).max(start);
        // Slicing on byte ranges may split a multibyte codepoint if the engine
        // produced bad ranges; defensively snap to char boundaries.
        let slice = match text.get(start..end) {
            Some(s) => s.to_string(),
            None => {
                // Fall back: nearest char boundaries.
                let s = nearest_boundary(text, start);
                let e = nearest_boundary(text, end).max(s);
                text[s..e].to_string()
            }
        };
        out.push(Segment {
            text: slice,
            hl: matches!(s.kind, SubSpanKind::Changed),
        });
    }
    out
}

fn nearest_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

/// Tall enough for the 1.5x Roboto Mono used in code rows at zoom=1.0.
pub(super) const ROW_H_BASE: f32 = 24.0;
/// Width of the line-number gutter, sized for ~4 digits in the code-row mono.
pub(super) const GUTTER_W_BASE: f32 = 60.0;

pub(super) const CONNECTOR_W: f32 = 60.0;

pub(super) fn row_h() -> f32 {
    ROW_H_BASE * crate::app::code_font_zoom()
}

pub(super) fn gutter_w() -> f32 {
    GUTTER_W_BASE * crate::app::code_font_zoom()
}

/// Per-session view state that must persist across frames.
#[derive(Default)]
pub struct DiffViewState {
    pub(super) last_left: f32,
    pub(super) last_right: f32,
    pub(super) written_left: Option<f32>,
    pub(super) written_right: Option<f32>,
    pub(super) pending_left: Option<f32>,
    pub(super) pending_right: Option<f32>,
    /// Two-click anchor creation: line picked on side A awaiting partner on B.
    pub(super) pending_a: Option<u32>,
    pub(super) pending_b: Option<u32>,
    /// Active text selection. `side` is the pane the anchor was set in; the
    /// selection is always confined to that one pane.
    pub selection: Option<Selection>,
    /// In-progress LMB-down → drag → release. `Some` from the frame an LMB
    /// press lands inside a pane until the button is released. The selection
    /// is only created once the drag exceeds a threshold; a press+release
    /// without movement is just a caret placement and leaves selection `None`.
    pub(super) drag: Option<DragState>,
    /// Arrow-key focus request: when set, the row whose (side, line_no)
    /// matches grabs keyboard focus on its next render. The `usize` is the
    /// target column (in chars) we'd like the caret to land at — clamped to
    /// the new line's length so vertical motion preserves the column the
    /// user was on. Driven by Up / Down inside an active `input_text` row.
    pub arrow_focus: Option<(Side, u32, usize)>,
    /// `ui.time()` at the last moment something forced the caret to become
    /// visible (line jump, click). Used to phase-reset the blink so the new
    /// caret is on immediately rather than potentially landing in the "off"
    /// half of the cycle.
    pub caret_blink_reset: f64,
    /// Bumped whenever something mutates session lines from *outside* the
    /// row's own input_text (undo, redo, hunk apply). Mixed into each row's
    /// imgui widget id so an active input_text gets a fresh internal state
    /// instead of writing its stale stb buffer back into our session — that
    /// stale write was the root cause of "undo immediately reapplies the
    /// edit" loops while the row had focus.
    pub input_epoch: u32,
    /// Horizontal scroll pin applied for several frames following a
    /// selection-delete splice. The splice queues `arrow_focus` to refocus
    /// the surviving row; on the next frame `set_keyboard_focus_here`
    /// fires and imgui's nav system writes a `ScrollTarget.x = gutter_w`
    /// to bring the wide input_text widget into view, which would shift
    /// the pane horizontally by `gutter_w` pixels. We pre-empt that by
    /// pushing `igSetNextWindowScroll` each frame the pin is live;
    /// `igSetNextWindowScroll` writes `Scroll.x` directly at `Begin()`
    /// AND clears any pending `ScrollTarget.x`. The countdown is
    /// empirically tuned: 2 frames is not enough (imgui keeps re-setting
    /// `ScrollTarget` across multiple frames as the widget activates
    /// over its 2-frame activation cycle); 3 is the minimum that holds,
    /// 4 gives a 1-frame safety margin. The cost of running longer is
    /// that user-initiated horizontal scroll is blocked for those
    /// frames after a splice — ~67ms, brief and only after a Delete.
    /// `u8` is a frame countdown decremented each render entry.
    pub(super) pin_scroll_x_after_splice: Option<(Side, f32, u8)>,
    /// Last frame's per-pane horizontal scroll. Written at end of `render`.
    /// Currently used only by the headless test harness to inspect scroll
    /// position after a frame. Cheap to maintain (one `f32` per pane).
    pub last_left_scroll_x: f32,
    pub last_right_scroll_x: f32,
    /// Last frame's active input_text selection (imgui's internal one,
    /// captured AFTER our suppression callback runs). `None` when no row
    /// is active or the selection is collapsed. `(side, line_no, start_byte,
    /// end_byte)`. Written by `draw_row`'s callback; read by tests to
    /// verify behaviors like double-click word-select.
    pub last_active_input_selection: Option<(Side, u32, usize, usize)>,
    /// Last frame's caret x offset from `text_start_x` (the pane's text
    /// column), in pixels. Equals `char_col * char_w` where `char_col`
    /// is the character index (not byte index) of the caret. Tests use
    /// this to verify the caret aligns with the rendered characters.
    pub last_active_caret_offset: Option<(Side, f32)>,
}

/// One end of a selection. `line_no` is the source-line index on `side` of
/// the pane that owns the selection (1-based, matching `Row::line_no`). Lines
/// are stable across diff recomputes, so the selection survives unrelated
/// edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelPoint {
    pub line_no: u32,
    pub col: usize,
}

#[derive(Clone)]
pub struct Selection {
    pub side: Side,
    pub anchor: SelPoint,
    pub caret: SelPoint,
}

/// Tracks an LMB-held interaction inside a pane. The selection is only
/// materialized (`state.selection = Some(...)`) once `threshold_passed` flips
/// true, so a plain click → release never produces a selection.
pub(super) struct DragState {
    pub(super) side: Side,
    pub(super) anchor: SelPoint,
    pub(super) press_screen: [f32; 2],
    pub(super) threshold_passed: bool,
}

/// Return the selection's endpoints in document order (top-most, then
/// bottom-most). For a single-line selection ties break by column.
pub fn ordered_endpoints(sel: &Selection) -> (SelPoint, SelPoint) {
    let (a, b) = (sel.anchor, sel.caret);
    if a.line_no < b.line_no || (a.line_no == b.line_no && a.col <= b.col) {
        (a, b)
    } else {
        (b, a)
    }
}

/// The canonical text→pixel formula for this module. Returns the pixel
/// x of `byte_pos` within `text`, using imgui's per-glyph advance
/// measurement. Snaps `byte_pos` to the nearest preceding char boundary
/// so callers can pass byte positions from imgui's stb_textedit cursor
/// (which is char-boundary-aligned but worth defending against) or from
/// `seg.text.len()` accumulation (which is also char-boundary-aligned
/// by construction). Every text-positioned element in `draw_row` —
/// caret, per-char highlight rects, drag-selection rect, and
/// `paint_row_text`'s chunk positions — calls this helper, so they
/// cannot drift relative to each other.
pub(super) fn text_x_at_byte(ui: &Ui, text: &str, byte_pos: usize) -> f32 {
    let take = byte_pos.min(text.len());
    let mut idx = take;
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    ui.calc_text_size(&text[..idx])[0]
}

/// Class of a character for double-click word-select. ImGui's input_text
/// only treats whitespace as a word boundary, so double-clicking on `=`
/// in `target_arch = "x"` selects `#[cfg(target_arch ` (everything from
/// the previous space to the next one). We override with the standard
/// text-editor heuristic: word chars cluster, whitespace clusters, and
/// each individual punct char is its own "word".
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub(super) enum CharClass {
    Word,
    Whitespace,
    Punct,
}

pub(super) fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Punct
    }
}

/// Return the byte range of the "word" containing `byte_idx` in `s`,
/// where "word" is defined by `char_class`: a run of word chars, a run
/// of whitespace, or a single punct char. Returns `(0, 0)` for an empty
/// string and clamps `byte_idx` into range. Handles UTF-8.
pub(super) fn double_click_word_bounds(s: &str, byte_idx: usize) -> (usize, usize) {
    if s.is_empty() {
        return (0, 0);
    }
    let positions: Vec<(usize, char)> = s.char_indices().collect();
    // Find the char whose byte range contains byte_idx (clamp to last).
    let mut target = positions.len() - 1;
    for (i, (start, _)) in positions.iter().enumerate() {
        if *start > byte_idx {
            target = i.saturating_sub(1);
            break;
        }
    }
    let (target_start, target_char) = positions[target];
    let class = char_class(target_char);
    if class == CharClass::Punct {
        return (target_start, target_start + target_char.len_utf8());
    }
    let mut lo = target;
    while lo > 0 && char_class(positions[lo - 1].1) == class {
        lo -= 1;
    }
    let mut hi = target + 1;
    while hi < positions.len() && char_class(positions[hi].1) == class {
        hi += 1;
    }
    let start_byte = positions[lo].0;
    let end_byte = if hi < positions.len() {
        positions[hi].0
    } else {
        s.len()
    };
    (start_byte, end_byte)
}

impl Side {
    pub fn as_focused_pane(self) -> crate::app::FocusedPane {
        match self {
            Side::Left => crate::app::FocusedPane::TwoWayA,
            Side::Right => crate::app::FocusedPane::TwoWayB,
        }
    }
}

/// Build the source text for `sel.side` and slice out the selected range
/// directly from the line vector (no row mapping needed — line_no is the key).
pub fn extract_selection_text(snap: &crate::session::DiffSession, sel: &Selection) -> String {
    let crate::session::SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode else {
        return String::new();
    };
    let source = match sel.side {
        Side::Left => a_lines,
        Side::Right => b_lines,
    };
    if source.is_empty() {
        return String::new();
    }
    let (lo, hi) = ordered_endpoints(sel);
    let s_idx = (lo.line_no.saturating_sub(1) as usize).min(source.len() - 1);
    let e_idx = (hi.line_no.saturating_sub(1) as usize).min(source.len() - 1);
    let mut out = String::new();
    for i in s_idx..=e_idx {
        let chars: Vec<char> = source[i].chars().collect();
        let l = if i == s_idx { lo.col } else { 0 }.min(chars.len());
        let h = if i == e_idx { hi.col } else { chars.len() }.min(chars.len());
        out.extend(chars[l..h].iter());
        if i < e_idx {
            out.push('\n');
        }
    }
    out
}

/// Select all of `side` in the active diff session.
pub fn select_all(snap: &crate::session::DiffSession, side: Side) -> Option<Selection> {
    let crate::session::SessionMode::TwoWay { a_lines, b_lines, .. } = &snap.mode else {
        return None;
    };
    let source = match side {
        Side::Left => a_lines,
        Side::Right => b_lines,
    };
    if source.is_empty() {
        return None;
    }
    let last_idx = source.len() - 1;
    let last_chars = source[last_idx].chars().count();
    Some(Selection {
        side,
        anchor: SelPoint { line_no: 1, col: 0 },
        caret: SelPoint {
            line_no: (last_idx as u32) + 1,
            col: last_chars,
        },
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub(super) enum Cls {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone)]
pub(super) struct Row {
    pub(super) line_no: Option<u32>,
    pub(super) segments: Vec<Segment>,
    pub(super) cls: Cls,
    /// Hunk this row belongs to. Used so the hover overlay knows which hunk
    /// the user is interacting with without re-scanning the list.
    pub(super) hunk_id: u32,
    /// True iff this row sits inside a change hunk (i.e., a hunk the
    /// decision buttons can act on).
    pub(super) is_change: bool,
    /// Index of the first row of `hunk_id` inside the pane's `rows` Vec.
    /// Used to position the hover overlay at the top of the hunk rather
    /// than at the cursor.
    pub(super) hunk_first_row: usize,
}

pub(super) struct Pane {
    pub(super) rows: Vec<Row>,
    /// (hunk_id, top_y, bot_y) per hunk in content-pixel coordinates.
    pub(super) ranges: Vec<(u32, f32, f32)>,
    /// Line number → y in content-pixel coordinates.
    pub(super) line_ys: HashMap<u32, f32>,
}

pub(super) fn is_change_hunk(h: &Hunk) -> bool {
    h.ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }))
}

pub(super) fn plain(text: &str) -> Vec<Segment> {
    vec![Segment {
        text: text.to_string(),
        hl: false,
    }]
}

pub(super) fn build_pane(hunks: &[Hunk], side: Side) -> Pane {
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
            // Engine-supplied spans (DiffOp.spans) take precedence when present
            // — they reflect the session's chosen sub-line granularity. Otherwise
            // fall back to the local char_diff helper.
            let dels: Vec<(u32, &str, Option<&Vec<SubSpan>>)> = h
                .ops
                .iter()
                .filter_map(|op| match op {
                    DiffOp::Delete { a, text, spans, .. } => Some((*a, text.as_str(), spans.as_ref())),
                    _ => None,
                })
                .collect();
            let inss: Vec<(u32, &str, Option<&Vec<SubSpan>>)> = h
                .ops
                .iter()
                .filter_map(|op| match op {
                    DiffOp::Insert { b, text, spans, .. } => Some((*b, text.as_str(), spans.as_ref())),
                    _ => None,
                })
                .collect();
            let n_pairs = dels.len().min(inss.len());

            let segments_for = |text: &str, spans: Option<&Vec<SubSpan>>, other: &str, is_left: bool| -> Vec<Segment> {
                if let Some(sp) = spans {
                    return segments_from_spans(text, sp);
                }
                let runs = char_diff(text, other);
                if is_left { left_segments(&runs) } else { right_segments(&runs) }
            };

            match side {
                Side::Left => {
                    for i in 0..n_pairs {
                        let segments = segments_for(dels[i].1, dels[i].2, inss[i].1, true);
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
                        let segments = segments_for(inss[i].1, inss[i].2, dels[i].1, false);
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

pub(super) fn side_to_input(s: Side) -> super::super::input::Side {
    match s {
        Side::Left => super::super::input::Side::Left,
        Side::Right => super::super::input::Side::Right,
    }
}

pub(super) fn side_from_input(s: super::super::input::Side) -> Side {
    match s {
        super::super::input::Side::Left => Side::Left,
        super::super::input::Side::Right => Side::Right,
    }
}

pub(super) fn ribbon_color(is_change: bool) -> [f32; 4] {
    if is_change {
        super::super::theme::with_alpha(super::super::theme::BLUE, 0.28)
    } else {
        super::super::theme::with_alpha(super::super::theme::OVERLAY1, 0.10)
    }
}

pub(super) fn pack_color(c: [f32; 4]) -> u32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to8(c[0]) | (to8(c[1]) << 8) | (to8(c[2]) << 16) | (to8(c[3]) << 24)
}

pub(super) fn v2(x: f32, y: f32) -> imgui::sys::ImVec2 {
    imgui::sys::ImVec2 { x, y }
}

pub(super) const BEZIER_SEGMENTS: usize = 24;

pub(super) fn cubic_bezier(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
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

pub(super) fn sample_curve(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) -> Vec<[f32; 2]> {
    (0..=BEZIER_SEGMENTS)
        .map(|i| cubic_bezier(p0, p1, p2, p3, i as f32 / BEZIER_SEGMENTS as f32))
        .collect()
}

/// Fill an arbitrary (possibly concave) polygon by triangulating it with
/// earcut, then submitting the resulting triangles directly via imgui's
/// low-level primitive API. imgui's own `AddConvexPolyFilled` / `PathFillConvex`
/// fans from vertex 0, which only works for convex shapes; this bypass
/// guarantees correct fill regardless of polygon shape.
pub(super) fn fill_polygon(pts: &[[f32; 2]], color: [f32; 4]) {
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
pub(super) fn fill_bezier_ribbon(x_l: f32, x_r: f32, a1: f32, a2: f32, b1: f32, b2: f32, color: [f32; 4]) {
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

pub(super) fn stroke_bezier_curve(
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

pub(super) fn locate_hunk(ranges: &[(u32, f32, f32)], y: f32) -> Option<(u32, f32)> {
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

pub(super) fn target_scroll(
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

// Note: build_selection_splice lives in input.rs.
