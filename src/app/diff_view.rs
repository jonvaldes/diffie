//! 2-way diff view.
//!
//! Side-by-side virtualized panes, a bezier-ribbon connector strip, inline
//! per-hunk decision buttons, center-anchored scroll sync, and click-to-anchor
//! line correspondence. Pending: char-level highlights (step 9).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use imgui::{FontId, ListClipper, StyleVar, Ui};

use super::char_diff::{char_diff, left_segments, right_segments, Segment};
use super::syntax::LineSpans;
use super::theme;
use super::undo_stack::DiffEdit;
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
    /// In-progress LMB-down → drag → release. `Some` from the frame an LMB
    /// press lands inside a pane until the button is released. The selection
    /// is only created once the drag exceeds a threshold; a press+release
    /// without movement is just a caret placement and leaves selection `None`.
    drag: Option<DragState>,
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
    pin_scroll_x_after_splice: Option<(Side, f32, u8)>,
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
struct DragState {
    side: Side,
    anchor: SelPoint,
    press_screen: [f32; 2],
    threshold_passed: bool,
}

/// Build a `SpliceTwoWayLines` edit that deletes the text covered by `sel`
/// from the corresponding source side. For a multi-line selection the
/// boundary lines' kept-content is merged into a single replacement line, so
/// `Delete` on the selection collapses the lines exactly the way a normal
/// text editor would. Returns `None` if the selection is zero-width or refers
/// to lines that no longer exist in the source.
fn build_selection_splice(
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

/// Class of a character for double-click word-select. ImGui's input_text
/// only treats whitespace as a word boundary, so double-clicking on `=`
/// in `target_arch = "x"` selects `#[cfg(target_arch ` (everything from
/// the previous space to the next one). We override with the standard
/// text-editor heuristic: word chars cluster, whitespace clusters, and
/// each individual punct char is its own "word".
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
enum CharClass {
    Word,
    Whitespace,
    Punct,
}

fn char_class(c: char) -> CharClass {
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
fn double_click_word_bounds(s: &str, byte_idx: usize) -> (usize, usize) {
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

#[cfg(test)]
mod word_bounds_tests {
    use super::*;

    #[test]
    fn word_run() {
        let s = "alpha beta gamma";
        // Click at any char of "beta" → selects "beta".
        assert_eq!(double_click_word_bounds(s, 6), (6, 10));
        assert_eq!(double_click_word_bounds(s, 7), (6, 10));
        assert_eq!(double_click_word_bounds(s, 9), (6, 10));
    }

    #[test]
    fn punct_is_single_char() {
        let s = "#[cfg(target_arch = \"wasm32\")]";
        // '=' is at index 18.
        assert_eq!(double_click_word_bounds(s, 18), (18, 19));
        // '#' at index 0.
        assert_eq!(double_click_word_bounds(s, 0), (0, 1));
        // ')' at index 28.
        assert_eq!(double_click_word_bounds(s, 28), (28, 29));
    }

    #[test]
    fn whitespace_run() {
        let s = "a   b";
        assert_eq!(double_click_word_bounds(s, 2), (1, 4));
    }

    #[test]
    fn underscore_is_word() {
        let s = "target_arch";
        assert_eq!(double_click_word_bounds(s, 6), (0, 11));
    }

    #[test]
    fn empty_and_out_of_bounds() {
        assert_eq!(double_click_word_bounds("", 0), (0, 0));
        // Clamps high byte_idx to last char.
        assert_eq!(double_click_word_bounds("ab", 100), (0, 2));
    }

    #[test]
    fn utf8() {
        // 'café': 'c','a','f','é'. 'é' is 2 bytes (0xC3 0xA9).
        let s = "café word";
        // 'é' starts at byte 3, len 2.
        assert_eq!(double_click_word_bounds(s, 3), (0, 5)); // selects "café"
        assert_eq!(double_click_word_bounds(s, 6), (6, 10)); // selects "word"
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
    pending_edits: &mut Vec<DiffEdit>,
    a_highlights: &[LineSpans],
    b_highlights: &[LineSpans],
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
    // Structural-line request from the inline editor (Backspace/Delete on an
    // empty buffer ⇒ remove that line). Drained into `pending_edits` after
    // both panes render.
    let line_remove: Cell<Option<DiffEdit>> = Cell::new(None);
    // Up/Down arrow focus handoff between row input_texts. Seeded from
    // `state.arrow_focus` so a request set last frame survives; consumed by
    // whichever row matches and re-set by an active row that sees Up/Down.
    let arrow_focus_cell: Cell<Option<(Side, u32, usize)>> = Cell::new(state.arrow_focus.take());
    // Phase-reset for the manually drawn caret blink. Rows write `ui.time()`
    // here whenever an input freshly activates (line change, click) so the
    // caret is guaranteed visible at that instant rather than possibly
    // landing in the "off" half of the cycle.
    let caret_blink_reset_cell: Cell<f64> = Cell::new(state.caret_blink_reset);

    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    let left_scroll = Cell::new(0.0_f32);
    let right_scroll = Cell::new(0.0_f32);
    let left_scroll_x = Cell::new(0.0_f32);
    let right_scroll_x = Cell::new(0.0_f32);
    let left_origin = Cell::new([0.0_f32, 0.0_f32]);
    let right_origin = Cell::new([0.0_f32, 0.0_f32]);
    let left_visible = Cell::new(avail[1]);
    let right_visible = Cell::new(avail[1]);
    // Filled by whichever row's input_text is active this frame, with the
    // imgui-internal selection bounds AFTER our CaretCapture callback ran.
    // Read at end of `render` into `state.last_active_input_selection`.
    let active_selection: Cell<Option<(Side, u32, usize, usize)>> = Cell::new(None);
    // Shift+Up / Shift+Down inside an active input_text extends the
    // cross-row selection. Filled by `draw_row` when shift is held and
    // an arrow key fires: `(side, current_line, current_col, target_line)`.
    // Applied to `state.selection` after the panes render.
    let shift_arrow_extend: Cell<Option<(Side, u32, usize, u32)>> = Cell::new(None);
    // Plain arrow key (no shift) inside an active input_text: clear the
    // cross-row selection. Standard editor behavior — the caret moves
    // and the selection collapses.
    let clear_state_selection: Cell<bool> = Cell::new(false);

    // Read the splice-induced scroll-x pin (if any) and decrement its
    // frame counter. We re-push it via `igSetNextWindowScroll` for the
    // matching pane each frame the counter is live.
    let pin_scroll_x: Option<(Side, f32)> = state
        .pin_scroll_x_after_splice
        .as_ref()
        .map(|(s, x, _)| (*s, *x));
    if let Some((_, _, n)) = state.pin_scroll_x_after_splice.as_mut() {
        *n = n.saturating_sub(1);
        if *n == 0 {
            state.pin_scroll_x_after_splice = None;
        }
    }
    // First row's `calc_text_size("m")` under the mono font lands here; the
    // central selection handler needs it to map mouse x to a column.
    let char_w_cell: Cell<f32> = Cell::new(0.0);

    // Carries the drag's side AND whether it has crossed the movement
    // threshold. The threshold flag gates per-row imgui-selection
    // suppression: below threshold (e.g. just after a click or a
    // double-click) we let imgui's native input_text selection alone so
    // double-click word-select can survive; past threshold we collapse it
    // so our drag-selection is the only visible selection.
    let drag_active_side: Option<(Side, bool)> =
        state.drag.as_ref().map(|d| (d.side, d.threshold_passed));

    let frame_selection = state.selection.clone();
    let max_left = (left.rows.len() as f32 * row_h() - avail[1]).max(0.0);
    let max_right = (right.rows.len() as f32 * row_h() - avail[1]).max(0.0);

    // Per-side content widths — drive horizontal scrolling. Push the mono
    // font briefly so `calc_text_size` measures the same font the rows
    // render with, then walk each side's rows to find the widest line.
    // A small trailing padding keeps the caret visible past the last
    // character without immediately overflowing the scrollbar.
    let char_w_global = {
        let _tok = mono_font.map(|f| ui.push_font(f));
        ui.calc_text_size("m")[0].max(1.0)
    };
    let max_chars = |rows: &[Row]| -> usize {
        rows.iter()
            .map(|r| r.segments.iter().map(|s| s.text.chars().count()).sum::<usize>())
            .max()
            .unwrap_or(0)
    };
    let content_w_for = |rows: &[Row]| -> f32 {
        gutter_w() + (max_chars(rows) as f32) * char_w_global + 16.0
    };
    let content_w_left = content_w_for(&left.rows);
    let content_w_right = content_w_for(&right.rows);

    // Pane geometry, computed before any child renders so we can flip the
    // render order without losing the visual layout. `set_cursor_screen_pos`
    // positions each child explicitly; `connector_origin` is derived from
    // geometry rather than the cursor between the two children.
    let panes_top_left = ui.cursor_screen_pos();
    let left_pos = panes_top_left;
    let right_pos = [panes_top_left[0] + pane_w + CONNECTOR_W, panes_top_left[1]];
    let connector_origin = [panes_top_left[0] + pane_w, panes_top_left[1]];

    // Driver detection: which pane is the mouse hovering when the wheel
    // fires? We need to know up-front so we can render the driver first,
    // capture its post-wheel scroll, and push the follower's matching
    // scroll via `igSetNextWindowScroll` — all within the same frame.
    let mouse_pos = ui.io().mouse_pos;
    let wheel = ui.io().mouse_wheel;
    let in_y = mouse_pos[1] >= panes_top_left[1]
        && mouse_pos[1] < panes_top_left[1] + avail[1];
    let in_right_x =
        mouse_pos[0] >= right_pos[0] && mouse_pos[0] < right_pos[0] + pane_w;
    let right_first = wheel.abs() > 1e-3 && in_y && in_right_x;

    // --- Inline closures kept lightweight by separating "render" from the
    // sync math. Each render branch threads the same state mutations
    // (pending_X, written_X) explicitly to keep the borrow checker happy.

    if right_first {
        // --- right is driver: render right first ---
        ui.set_cursor_screen_pos(right_pos);
        {
            let y_to_apply = state.pending_right.take();
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Right)
                .map(|(_, x)| x);
            if y_to_apply.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = y_to_apply.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = y_to_apply {
                    state.written_right = Some(y);
                }
            }
        }
        ui.child_window("diffie_right")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_right, 0.0])
            .build(|| {
                right_scroll.set(ui.scroll_y());
                right_scroll_x.set(ui.scroll_x());
                right_origin.set(ui.cursor_screen_pos());
                right_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &right.rows,
                    Side::Right,
                    session_id,
                    &anchored_b,
                    &right_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    b_highlights,
                    content_w_right,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                );
            });

        // Right pane has applied its wheel-induced scroll — derive matching
        // left target same-frame.
        let cur_right_for_sync = right_scroll.get();
        let r_changed = (cur_right_for_sync - state.last_right).abs() > ECHO_TOLERANCE;
        let r_echo = state
            .written_right
            .map_or(false, |w| (cur_right_for_sync - w).abs() < ECHO_TOLERANCE);
        let left_override = if r_changed && !r_echo {
            target_scroll(
                cur_right_for_sync,
                avail[1],
                avail[1],
                &right.ranges,
                &left.ranges,
            )
            .map(|t| t.clamp(0.0, max_left))
        } else {
            None
        };
        // Always drain `pending_left` so a same-frame override never leaves a
        // stale value queued for the next frame (which would snap-back the
        // scroll when the wheel rate dips below threshold mid-gesture).
        let pending_consumed = state.pending_left.take();
        let apply_left = left_override.or(pending_consumed);

        ui.set_cursor_screen_pos(left_pos);
        {
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Left)
                .map(|(_, x)| x);
            if apply_left.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = apply_left.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = apply_left {
                    state.written_left = Some(y);
                }
            }
        }
        ui.child_window("diffie_left")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_left, 0.0])
            .build(|| {
                left_scroll.set(ui.scroll_y());
                left_scroll_x.set(ui.scroll_x());
                left_origin.set(ui.cursor_screen_pos());
                left_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &left.rows,
                    Side::Left,
                    session_id,
                    &anchored_a,
                    &left_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    a_highlights,
                    content_w_left,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                );
            });

        // Stamp `written_X` with the actually-rendered scroll values, not
        // the floats we requested. Imgui rounds Scroll to whole pixels, so
        // when `left_override` is fractional (e.g. 2369.5) the echoed value
        // comes back as 2369.0 — exactly `ECHO_TOLERANCE` off, which the
        // strict `<` check trips into `!l_echo` and `sync_scrolls` queues a
        // stale `pending_right`. Storing the post-render value makes the
        // echo check exact.
        state.written_left = Some(left_scroll.get());
        state.written_right = Some(cur_right_for_sync);
    } else {
        // --- left is driver (or no wheel): render left first ---
        ui.set_cursor_screen_pos(left_pos);
        {
            let y_to_apply = state.pending_left.take();
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Left)
                .map(|(_, x)| x);
            if y_to_apply.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = y_to_apply.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = y_to_apply {
                    state.written_left = Some(y);
                }
            }
        }
        ui.child_window("diffie_left")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_left, 0.0])
            .build(|| {
                left_scroll.set(ui.scroll_y());
                left_scroll_x.set(ui.scroll_x());
                left_origin.set(ui.cursor_screen_pos());
                left_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &left.rows,
                    Side::Left,
                    session_id,
                    &anchored_a,
                    &left_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    a_highlights,
                    content_w_left,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                );
            });

        let cur_left_for_sync = left_scroll.get();
        let l_changed = (cur_left_for_sync - state.last_left).abs() > ECHO_TOLERANCE;
        let l_echo = state
            .written_left
            .map_or(false, |w| (cur_left_for_sync - w).abs() < ECHO_TOLERANCE);
        let right_override = if l_changed && !l_echo {
            target_scroll(
                cur_left_for_sync,
                avail[1],
                avail[1],
                &left.ranges,
                &right.ranges,
            )
            .map(|t| t.clamp(0.0, max_right))
        } else {
            None
        };
        // Always drain `pending_right` — see the right_first branch for why.
        let pending_consumed = state.pending_right.take();
        let apply_right = right_override.or(pending_consumed);

        ui.set_cursor_screen_pos(right_pos);
        {
            let x_to_apply = pin_scroll_x
                .filter(|(s, _)| *s == Side::Right)
                .map(|(_, x)| x);
            if apply_right.is_some() || x_to_apply.is_some() {
                let x = x_to_apply.unwrap_or(-1.0);
                let y = apply_right.unwrap_or(-1.0);
                unsafe {
                    imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 { x, y });
                }
                if let Some(y) = apply_right {
                    state.written_right = Some(y);
                }
            }
        }
        ui.child_window("diffie_right")
            .size([pane_w, avail[1]])
            .border(true)
            .horizontal_scrollbar(true)
            .content_size([content_w_right, 0.0])
            .build(|| {
                right_scroll.set(ui.scroll_y());
                right_scroll_x.set(ui.scroll_x());
                right_origin.set(ui.cursor_screen_pos());
                right_visible.set(ui.content_region_avail()[1]);
                draw_pane(
                    ui,
                    &right.rows,
                    Side::Right,
                    session_id,
                    &anchored_b,
                    &right_click,
                    mono_font,
                    frame_selection.as_ref(),
                    &focus_event,
                    &line_remove,
                    pending_edits,
                    &arrow_focus_cell,
                    &caret_blink_reset_cell,
                    state.input_epoch,
                    drag_active_side,
                    &char_w_cell,
                    b_highlights,
                    content_w_right,
                    &active_selection,
                    &shift_arrow_extend,
                    &clear_state_selection,
                );
            });

        // Mirror of the right_first branch — see comment there. Store the
        // actually-rendered scrolls so the next frame's echo check is exact.
        let cur_right_post = right_scroll.get();
        state.written_left = Some(cur_left_for_sync);
        state.written_right = Some(cur_right_post);
    }
    // Restore the cursor below the diff area for any subsequent widgets.
    ui.set_cursor_screen_pos([panes_top_left[0], panes_top_left[1] + avail[1]]);

    if let Some(p) = focus_event.get() {
        *focus_request = Some(p);
    }
    // Persist any unconsumed arrow-focus request for the next frame (the
    // target row may not have been visible this frame).
    state.arrow_focus = arrow_focus_cell.take();
    state.caret_blink_reset = caret_blink_reset_cell.get();
    // Apply Shift+Up/Down cross-row selection extension. Anchor is
    // preserved from any existing same-side selection; otherwise it's
    // pinned at the cursor's pre-move position. Caret moves to the
    // same column on the adjacent line.
    if let Some((side, cur_ln, cur_col, new_ln)) = shift_arrow_extend.take() {
        let anchor = match state.selection.as_ref() {
            Some(s) if s.side == side => s.anchor,
            _ => SelPoint { line_no: cur_ln, col: cur_col },
        };
        state.selection = Some(Selection {
            side,
            anchor,
            caret: SelPoint { line_no: new_ln, col: cur_col },
        });
    } else if clear_state_selection.take() {
        // Plain arrow press inside the active row — collapse any
        // existing cross-row selection. Mutually exclusive with the
        // shift-extend branch above.
        state.selection = None;
    }
    if let Some(edit) = line_remove.take() {
        pending_edits.push(edit);
    }

    update_selection(
        ui,
        state,
        &left,
        &right,
        left_origin.get(),
        right_origin.get(),
        left_visible.get(),
        right_visible.get(),
        pane_w,
        char_w_cell.get(),
        focus_request,
    );

    // Selection + Delete/Backspace ⇒ splice the selected range out of the
    // source side. We bypass `want_capture_keyboard` so a focused row's
    // input_text doesn't swallow Delete when a multi-line selection exists.
    //
    // The focused row's input_text will have *already* processed the same
    // Backspace this frame, queuing its own `SetTwoWayLine` for line 1 (the
    // active row) into `pending_edits`. That edit refers to a line we're
    // about to splice out, so we drop it before pushing the Splice — this
    // collapses the deletion into a single undo entry whose snapshot
    // reflects the true pre-keystroke state.
    let key_pressed = ui.is_key_pressed(imgui::Key::Delete)
        || ui.is_key_pressed(imgui::Key::Backspace);
    if key_pressed {
        if let Some(sel) = state.selection.as_ref().cloned() {
            if let Ok(snap) = store.snapshot(session_id) {
                if let Some(edit) = build_selection_splice(&snap, &sel, session_id) {
                    let (lo, hi) = ordered_endpoints(&sel);
                    let sel_side: TwoWaySide = match sel.side {
                        Side::Left => TwoWaySide::A,
                        Side::Right => TwoWaySide::B,
                    };
                    pending_edits.retain(|e| match e {
                        DiffEdit::SetTwoWayLine {
                            session_id: e_sid,
                            side: e_side,
                            line_no,
                            ..
                        } => !(*e_sid == session_id
                            && *e_side == sel_side
                            && *line_no >= lo.line_no
                            && *line_no <= hi.line_no),
                        _ => true,
                    });
                    pending_edits.push(edit);
                    // Park the caret on the surviving merged line at the
                    // selection's start column, so the next frame's render
                    // refocuses that row's input_text instead of leaving
                    // nothing active after the splice tears down the old
                    // row's widget id.
                    state.arrow_focus = Some((sel.side, lo.line_no, lo.col));
                    // Pin scroll_x for next frame: `set_keyboard_focus_here`
                    // would otherwise nav-scroll the pane to bring the wide
                    // input_text into view. The cells hold this frame's
                    // pre-edit scroll position — exactly what we want to
                    // restore.
                    let cur_scroll_x = match sel.side {
                        Side::Left => left_scroll_x.get(),
                        Side::Right => right_scroll_x.get(),
                    };
                    state.pin_scroll_x_after_splice = Some((sel.side, cur_scroll_x, 4));
                    state.selection = None;
                }
            }
        }
    }

    // Any structural edit invalidates line numbers the selection refers to.
    if pending_edits.iter().any(|e| {
        matches!(
            e,
            DiffEdit::SpliceTwoWayLines { .. } | DiffEdit::ReplaceHunkSide { .. }
        )
    }) {
        state.selection = None;
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
    state.last_left_scroll_x = left_scroll_x.get();
    state.last_right_scroll_x = right_scroll_x.get();
    state.last_active_input_selection = active_selection.get();

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
fn update_selection(
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
    use super::input::{self, InputFrame};

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

fn side_to_input(s: Side) -> super::input::Side {
    match s {
        Side::Left => super::input::Side::Left,
        Side::Right => super::input::Side::Right,
    }
}

fn side_from_input(s: super::input::Side) -> Side {
    match s {
        super::input::Side::Left => Side::Left,
        super::input::Side::Right => Side::Right,
    }
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
        theme::with_alpha(theme::BLUE, 0.28)
    } else {
        theme::with_alpha(theme::OVERLAY1, 0.10)
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
            stroke_bezier_curve(x_l, x_r, ly, ry, theme::CRUST, 3.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &Ui,
    rows: &[Row],
    side: Side,
    session_id: SessionId,
    anchored: &HashSet<u32>,
    click_out: &Cell<Option<u32>>,
    mono_font: Option<FontId>,
    selection: Option<&Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    line_remove: &Cell<Option<DiffEdit>>,
    pending_edits: &mut Vec<DiffEdit>,
    arrow_focus: &Cell<Option<(Side, u32, usize)>>,
    caret_blink_reset: &Cell<f64>,
    input_epoch: u32,
    drag_active: Option<(Side, bool)>,
    char_w_out: &Cell<f32>,
    highlights: &[LineSpans],
    content_w: f32,
    active_selection_out: &Cell<Option<(Side, u32, usize, usize)>>,
    shift_arrow_out: &Cell<Option<(Side, u32, usize, u32)>>,
    clear_state_selection_out: &Cell<bool>,
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
            let line_hl = r
                .line_no
                .and_then(|ln| highlights.get((ln as usize).saturating_sub(1)))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            if let Some(clicked_line) = draw_row(
                ui,
                r,
                side,
                i,
                session_id,
                anchored,
                mono_font,
                &hover,
                selection,
                focus_event,
                line_remove,
                pending_edits,
                arrow_focus,
                caret_blink_reset,
                input_epoch,
                drag_active,
                char_w_out,
                line_hl,
                content_w,
                active_selection_out,
                shift_arrow_out,
                clear_state_selection_out,
            ) {
                click_out.set(Some(clicked_line));
            }
        }
    }
    drop(_spacing);

    // Drag auto-scroll: while a drag is live on this side and the mouse is
    // past the pane's visible band, scroll proportionally. The selection
    // caret advances on its own via `update_selection`, which clamps the
    // mouse to the visible band and re-computes the caret each frame.
    if drag_active.map(|(s, _)| s) == Some(side) && ui.is_mouse_down(imgui::MouseButton::Left) {
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
        }
    }

    if let Some((hunk_id, pos)) = hover.get() {
        draw_control_overlay(ui, session_id, hunk_id, pos, pending_edits);
    }
}

/// Floating panel with the four decision buttons, rendered on top of the
/// hovered row. Takes no space in the row layout because it sets the cursor
/// to an absolute screen position and we ignore the cursor advance afterwards.
fn draw_control_overlay(
    ui: &Ui,
    session_id: SessionId,
    hunk_id: u32,
    pos: [f32; 2],
    pending_edits: &mut Vec<DiffEdit>,
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
        theme::with_alpha(theme::MANTLE, 0.95),
    )
    .filled(true)
    .rounding(4.0)
    .build();
    dl.add_rect(
        [panel_x, panel_y],
        [panel_x + panel_w, panel_y + panel_h],
        theme::BLUE,
    )
    .rounding(4.0)
    .thickness(1.0)
    .build();

    ui.set_cursor_screen_pos([panel_x + 6.0, panel_y + 3.0]);
    // 2-way edit mode: these copy this hunk's content from one side to the
    // other. Queued onto the undo stack so the operation is reversible.
    if ui.small_button(format!("Apply A → B##ov{hunk_id}_atob")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::B,
            old_target_lines: None,
        });
    }
    ui.same_line();
    if ui.small_button(format!("B → A##ov{hunk_id}_btoa")) {
        pending_edits.push(DiffEdit::ReplaceHunkSide {
            session_id,
            hunk_id,
            target: TwoWaySide::A,
            old_target_lines: None,
        });
    }
}

/// Render a single row. The text area is an always-live `input_text` so
/// Paint a row's text via the draw list, picking foreground colors from the
/// startup-computed `ColorTable` for the right background. Char positions
/// covered by a `seg.hl=true` segment on a Delete/Insert row use the
/// red/green table; everything else uses the normal table.
///
/// Adjacent chars that resolve to the same color are coalesced into a single
/// `add_text` call so unchanged stretches don't pay per-char overhead.
fn paint_row_text(
    dl: &imgui::DrawListMut<'_>,
    segments: &[Segment],
    origin: [f32; 2],
    char_w: f32,
    spans: &[super::syntax::LineSpan],
    row_cls: Cls,
) {
    // Flatten segments into char buffer + parallel hl mask.
    let mut chars: Vec<char> = Vec::new();
    let mut hl_mask: Vec<bool> = Vec::new();
    for seg in segments {
        for c in seg.text.chars() {
            chars.push(c);
            hl_mask.push(seg.hl);
        }
    }
    let n = chars.len();
    if n == 0 {
        return;
    }

    // Per-char syntax kind, filled from spans (non-overlapping, sorted).
    let mut kind_at: Vec<Option<super::syntax::SyntaxKind>> = vec![None; n];
    for s in spans {
        let start = s.start_col.min(n);
        let end = s.end_col.min(n).max(start);
        for c in start..end {
            kind_at[c] = Some(s.kind);
        }
    }

    let table_at = |c: usize| -> &'static super::syntax::ColorTable {
        let bg = match (row_cls, hl_mask[c]) {
            (Cls::Equal, _) => super::syntax::HlBg::None,
            (Cls::Delete, false) => super::syntax::HlBg::DeleteRow,
            (Cls::Delete, true) => super::syntax::HlBg::DeleteHl,
            (Cls::Insert, false) => super::syntax::HlBg::InsertRow,
            (Cls::Insert, true) => super::syntax::HlBg::InsertHl,
        };
        super::syntax::table_for(bg)
    };

    let pick = |c: usize| -> [f32; 4] { table_at(c).get(kind_at[c]) };

    // Coalesce contiguous same-color runs.
    let mut run_start = 0usize;
    let mut run_color = pick(0);
    for c in 1..n {
        let color = pick(c);
        if color != run_color {
            let chunk: String = chars[run_start..c].iter().collect();
            let pos = [origin[0] + run_start as f32 * char_w, origin[1]];
            dl.add_text(pos, run_color, &chunk);
            run_start = c;
            run_color = color;
        }
    }
    let chunk: String = chars[run_start..].iter().collect();
    let pos = [origin[0] + run_start as f32 * char_w, origin[1]];
    dl.add_text(pos, run_color, &chunk);
}

/// clicks place the caret directly and every keystroke commits — the diff
/// re-runs every frame the buffer changes. Mouse-driven selection transitions
/// live in `update_selection`; this function is read-only with respect to
/// `selection`. Returns `Some(line_no)` if the row was right-clicked this
/// frame (anchor pick).
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    row: &Row,
    side: Side,
    idx: i32,
    session_id: SessionId,
    anchored: &HashSet<u32>,
    mono_font: Option<FontId>,
    hover_out: &Cell<Option<(u32, [f32; 2])>>,
    selection: Option<&Selection>,
    focus_event: &Cell<Option<crate::app::FocusedPane>>,
    line_remove: &Cell<Option<DiffEdit>>,
    pending_edits: &mut Vec<DiffEdit>,
    arrow_focus: &Cell<Option<(Side, u32, usize)>>,
    caret_blink_reset: &Cell<f64>,
    input_epoch: u32,
    drag_active: Option<(Side, bool)>,
    char_w_out: &Cell<f32>,
    line_hl: &[super::syntax::LineSpan],
    content_w: f32,
    active_selection_out: &Cell<Option<(Side, u32, usize, usize)>>,
    shift_arrow_out: &Cell<Option<(Side, u32, usize, u32)>>,
    clear_state_selection_out: &Cell<bool>,
) -> Option<u32> {
    let p0 = ui.cursor_screen_pos();
    let row_w = ui.content_region_avail()[0];
    let p1 = [p0[0] + row_w, p0[1] + row_h()];

    // Gutter rect — used only for RMB anchor picking. LMB on the gutter is
    // handled by the central click handler like any other in-pane click.
    let gutter_p1 = [p0[0] + gutter_w(), p1[1]];
    let gutter_hovered = ui.is_mouse_hovering_rect(p0, gutter_p1);
    let rmb_anchor = gutter_hovered && ui.is_mouse_clicked(imgui::MouseButton::Right);

    // Positional hover for the full row, independent of any active widget.
    let mouse_in_row = ui.is_mouse_hovering_rect(p0, p1);
    if mouse_in_row && row.is_change {
        let pane_origin_y = p0[1] - (idx as f32) * row_h();
        let pane_visible_top = pane_origin_y + ui.scroll_y();
        let first_row_y = pane_origin_y + (row.hunk_first_row as f32) * row_h();
        let anchor_y = first_row_y.max(pane_visible_top);
        hover_out.set(Some((row.hunk_id, [p0[0], anchor_y])));
    }

    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let char_w = ui.calc_text_size("m")[0].max(1.0);
    char_w_out.set(char_w);
    let text_start_x = p0[0] + gutter_w();
    let char_count: usize = row.segments.iter().map(|s| s.text.chars().count()).sum();

    let dl = ui.get_window_draw_list();

    // ---- backgrounds: hunk color → hover tint → selection ----
    let bg = match row.cls {
        Cls::Equal => None,
        Cls::Delete => Some([0.55, 0.18, 0.18, 0.30]),
        Cls::Insert => Some([0.18, 0.50, 0.22, 0.30]),
    };
    if let Some(bg_rgba) = bg {
        dl.add_rect(p0, p1, bg_rgba).filled(true).build();
    }
    if mouse_in_row {
        dl.add_rect(p0, p1, theme::with_alpha(theme::TEXT, 0.04))
            .filled(true)
            .build();
    }
    if let (Some(sel), Some(ln)) = (selection, row.line_no) {
        if sel.side == side {
            let (lo, hi) = ordered_endpoints(sel);
            if ln >= lo.line_no && ln <= hi.line_no {
                let l_col = if ln == lo.line_no { lo.col } else { 0 };
                let r_col = if ln == hi.line_no { hi.col } else { char_count };
                let l_col = l_col.min(char_count);
                let r_col = r_col.min(char_count);
                if r_col > l_col {
                    let sel_x0 = text_start_x + l_col as f32 * char_w;
                    let sel_x1 = text_start_x + r_col as f32 * char_w;
                    dl.add_rect(
                        [sel_x0, p0[1]],
                        [sel_x1, p1[1]],
                        theme::with_alpha(theme::BLUE, 0.40),
                    )
                    .filled(true)
                    .build();
                }
            }
        }
    }
    let _ = focus_event;

    // ---- char-level highlight rects (red/green tint under changed chars) ----
    let hl_bg = match row.cls {
        Cls::Delete => [0.85, 0.18, 0.18, 0.20],
        Cls::Insert => [0.18, 0.70, 0.30, 0.20],
        Cls::Equal => [0.0, 0.0, 0.0, 0.0],
    };
    let mut x = text_start_x;
    for seg in &row.segments {
        if seg.text.is_empty() {
            continue;
        }
        let w = ui.calc_text_size(&seg.text)[0];
        if seg.hl {
            dl.add_rect([x, p0[1] + 2.0], [x + w, p0[1] + row_h() - 2.0], hl_bg)
                .filled(true)
                .build();
        }
        x += w;
    }

    // ---- gutter line number ----
    let line_text = match row.line_no {
        Some(n) => format!("{n:>4}"),
        None => "    ".to_string(),
    };
    dl.add_text([p0[0] + 6.0, p0[1] + 3.0], theme::OVERLAY1, &line_text);

    // ---- anchored row marker ----
    if let Some(ln) = row.line_no {
        if anchored.contains(&ln) {
            dl.add_rect(p0, [p0[0] + 3.0, p1[1]], theme::LAVENDER)
                .filled(true)
                .build();
        }
    }

    // ---- syntax-colored text rendering ----
    //
    // We paint the row text directly via the draw list (before input_text
    // builds) so per-token colors land. The `input_text` widget that follows
    // gets its Text style color set to transparent — it still owns the
    // caret, selection-bg, and keyboard input, but doesn't draw its own
    // (un-highlighted) copy on top of ours.
    //
    // Syntax spans apply on every row regardless of hunk class; the red/green
    // *background* tints (row bg + per-char hl rects) continue to mark
    // Delete/Insert visually, but the text itself stays readable in
    // palette colors.
    let mut buf: String = row.segments.iter().map(|s| s.text.as_str()).collect();
    let was_empty = buf.is_empty();
    paint_row_text(
        &dl,
        &row.segments,
        [text_start_x, p0[1] + 3.0],
        char_w,
        line_hl,
        row.cls,
    );
    let _frame_bg = ui.push_style_color(imgui::StyleColor::FrameBg, [0.0, 0.0, 0.0, 0.0]);
    let _frame_bg_hov = ui.push_style_color(imgui::StyleColor::FrameBgHovered, [0.0, 0.0, 0.0, 0.0]);
    let _frame_bg_act = ui.push_style_color(imgui::StyleColor::FrameBgActive, [0.0, 0.0, 0.0, 0.0]);
    // Transparent text so input_text doesn't double-draw on top of the
    // colored spans we just painted via `paint_row_text`.
    let _text_color = ui.push_style_color(imgui::StyleColor::Text, [0.0, 0.0, 0.0, 0.0]);
    let _pad = ui.push_style_var(StyleVar::FramePadding([2.0, 2.0]));
    let _border = ui.push_style_var(StyleVar::FrameBorderSize(0.0));
    ui.set_cursor_screen_pos([text_start_x, p0[1]]);
    // Match the parent window's content width so the input_text spans the
    // whole row; otherwise imgui's input_text would horizontally scroll its
    // *own* contents on long lines, fighting the parent's scroll position.
    ui.set_next_item_width((content_w - gutter_w()).max(1.0));
    let input_id = match row.line_no {
        Some(n) => format!("##rowedit_{:?}_{n}_e{input_epoch}", side),
        None => format!("##rowedit_{:?}_idx_{idx}_e{input_epoch}", side),
    };
    // If a previous frame's Up/Down arrow asked us to focus this row, claim
    // keyboard focus right before the input_text builds. imgui routes
    // SetKeyboardFocusHere through its nav-tabbing system, which actually
    // activates the widget on the *next* frame — so we keep the request
    // alive until `is_item_activated` confirms the input took focus, and
    // the callback below clears the imgui-inserted select-all whenever the
    // request is still live for this row.
    let arrow_match_target: Option<usize> = match (arrow_focus.get(), row.line_no) {
        (Some((req_side, req_ln, tcol)), Some(ln)) if req_side == side && req_ln == ln => {
            Some(tcol)
        }
        _ => None,
    };
    let arrow_match = arrow_match_target.is_some();
    if arrow_match {
        ui.set_keyboard_focus_here();
    }
    // Convert the requested target column (chars) into a byte offset within
    // this row's buffer, clamped to its length. -1 means "don't seed".
    let seed_byte: i32 = match arrow_match_target {
        Some(tcol) => {
            let take = tcol.min(buf.chars().count());
            buf.chars().take(take).map(|c| c.len_utf8()).sum::<usize>() as i32
        }
        None => -1,
    };
    // Detect a double-click that lands inside this row's input_text and
    // pre-compute the desired byte range. ImGui's native double-click
    // selects from the previous space to the next space — too greedy for
    // punctuation. Our override narrows to the standard text-editor
    // word-class run; the CaretCapture callback applies it after imgui's
    // word-select has run.
    let dbl_click_override: Option<(usize, usize)> = if ui
        .is_mouse_double_clicked(imgui::MouseButton::Left)
    {
        let click_pos = ui.io().mouse_pos;
        let widget_x0 = text_start_x;
        let widget_x1 = text_start_x + (content_w - gutter_w()).max(1.0);
        if click_pos[0] >= widget_x0
            && click_pos[0] < widget_x1
            && click_pos[1] >= p0[1]
            && click_pos[1] < p1[1]
        {
            let raw_col = ((click_pos[0] - widget_x0) / char_w).floor().max(0.0);
            let char_col = raw_col as usize;
            let byte_idx = buf
                .char_indices()
                .nth(char_col)
                .map(|(b, _)| b)
                .unwrap_or(buf.len());
            Some(double_click_word_bounds(&buf, byte_idx))
        } else {
            None
        }
    } else {
        None
    };
    // Capture imgui's internal cursor position via the ALWAYS callback so we
    // can paint the caret ourselves below — imgui's own caret uses the Text
    // color, which we forced to transparent to avoid double-drawing. We also
    // use the callback to suppress the select-all that imgui does on the
    // first frame after `SetKeyboardFocusHere`, which otherwise highlights
    // the whole row when arrow keys jump between lines, and to seed the
    // caret at the column the user had on the previous line.
    // While a cross-row drag selection is live on this side, the row where
    // mouse-down landed will *also* drag-select its imgui input_text
    // contents — that's the extra horizontal highlight tracking the
    // pointer. Suppress it by collapsing imgui's selection to the cursor
    // every frame the drag is live on our side.
    // Only suppress imgui's native input_text selection once our drag has
    // crossed the movement threshold. Pre-threshold we let imgui's
    // selection survive so double-click word-select (which sets a multi-
    // char selection that our state.selection doesn't track) doesn't get
    // immediately collapsed by this callback.
    let drag_on_this_side_past_threshold = drag_active
        .map(|(s, past)| s == side && past)
        .unwrap_or(false);
    let caret_pos: Cell<i32> = Cell::new(-1);
    // Filled after the callback with imgui's post-mutation selection bounds
    // (start_byte, end_byte). Read after `build()` so we know whether
    // imgui's input_text ended up with a selection this frame.
    let caret_selection: Cell<Option<(usize, usize)>> = Cell::new(None);
    struct CaretCapture<'a> {
        out: &'a Cell<i32>,
        selection_out: &'a Cell<Option<(usize, usize)>>,
        clear_selection: bool,
        seed_byte: i32,
        suppress_imgui_selection: bool,
        dbl_click_override: Option<(usize, usize)>,
    }
    impl<'a> imgui::InputTextCallbackHandler for CaretCapture<'a> {
        fn on_always(&mut self, mut data: imgui::TextCallbackData) {
            if self.clear_selection {
                if self.seed_byte >= 0 {
                    data.set_cursor_pos(self.seed_byte as usize);
                }
                let pos = data.cursor_pos() as i32;
                *data.selection_start_mut() = pos;
                *data.selection_end_mut() = pos;
            } else if self.suppress_imgui_selection {
                let pos = data.cursor_pos() as i32;
                *data.selection_start_mut() = pos;
                *data.selection_end_mut() = pos;
            } else if let Some((s, e)) = self.dbl_click_override {
                // Replace imgui's overly-greedy word selection.
                data.set_cursor_pos(e);
                *data.selection_start_mut() = s as i32;
                *data.selection_end_mut() = e as i32;
            }
            self.out.set(data.cursor_pos() as i32);
            // Capture the post-mutation selection so tests can observe
            // double-click word-select and similar behaviors.
            let sel = data.selection();
            self.selection_out.set(Some((sel.start, sel.end)));
        }
    }
    let changed = ui
        .input_text(input_id, &mut buf)
        // imgui's input_text has its own per-char undo stack on Ctrl+Z. If
        // it ran alongside our app-level stack, a Ctrl+Z that pops a
        // selection-driven `Splice` would *also* make imgui re-insert a
        // char in the focused row, the input fires `changed`, we push a
        // stale `SetTwoWayLine`, and `record.edit` truncates the redo
        // history — the Splice is gone for good. We own undo at the diff
        // level, so disable imgui's.
        .no_undo_redo(true)
        .callback(
            imgui::InputTextCallback::ALWAYS,
            CaretCapture {
                out: &caret_pos,
                selection_out: &caret_selection,
                clear_selection: arrow_match,
                seed_byte,
                suppress_imgui_selection: drag_on_this_side_past_threshold,
                dbl_click_override,
            },
        )
        .build();
    let input_active = ui.is_item_active();
    // Export the active row's imgui selection (post-callback) so render's
    // caller — currently just headless tests — can observe behaviors like
    // double-click word-select. Last-active-row wins if multiple are
    // active in a frame, which doesn't happen in practice.
    if input_active {
        if let (Some(ln), Some((s, e))) = (row.line_no, caret_selection.get()) {
            if s != e {
                active_selection_out.set(Some((side, ln, s, e)));
            }
        }
    }
    // The arrow-focus request is satisfied once the input actually becomes
    // active — `is_item_activated` is true only on that single frame, after
    // which we drop the request so a normal click can select-all again.
    if ui.is_item_activated() {
        caret_blink_reset.set(ui.time());
        if arrow_match {
            arrow_focus.set(None);
        }
    }
    // Up/Down inside an active row: hand keyboard focus to the adjacent
    // source-line row on the same side. We snapshot the current caret column
    // (chars, converted from imgui's byte offset using the row's buffer) so
    // the target row can drop the caret at the same column instead of at the
    // end of the line.
    if input_active {
        if let Some(ln) = row.line_no {
            let up = ui.is_key_pressed(imgui::Key::UpArrow) && ln > 1;
            let down = ui.is_key_pressed(imgui::Key::DownArrow);
            let left = ui.is_key_pressed(imgui::Key::LeftArrow);
            let right = ui.is_key_pressed(imgui::Key::RightArrow);
            let shift = ui.io().key_shift;
            // Lateral motion within the row (Left/Right) is handled by
            // imgui's input_text internally, so `is_item_activated`
            // doesn't fire — we need an explicit blink-reset here so the
            // caret is on for the first half-cycle after the move.
            if left || right {
                caret_blink_reset.set(ui.time());
            }
            if up || down {
                let cur_byte = caret_pos.get().max(0) as usize;
                let take = cur_byte.min(buf.len());
                let cur_col = buf
                    .get(..take)
                    .map(|s| s.chars().count())
                    .unwrap_or_else(|| buf.chars().count());
                let new_ln = if up { ln - 1 } else { ln + 1 };
                arrow_focus.set(Some((side, new_ln, cur_col)));
                if shift {
                    shift_arrow_out.set(Some((side, ln, cur_col, new_ln)));
                }
            }
            // Plain arrow navigation (any direction, no shift) collapses
            // the cross-row selection. The caret continues moving via
            // imgui's own handling (Left/Right within the row) or our
            // arrow_focus (Up/Down between rows).
            if !shift && (up || down || left || right) {
                clear_state_selection_out.set(true);
            }
        }
    }
    drop(_pad);
    drop(_border);
    drop(_text_color);
    drop(_frame_bg_act);
    drop(_frame_bg_hov);
    drop(_frame_bg);

    // Manual caret: imgui draws its own caret with `ImGuiCol_Text`, which we
    // forced to transparent so it wouldn't overpaint the syntax-colored
    // spans. We replay the caret here at the position the callback reported,
    // blinking on a ~1s cycle to roughly match imgui's default.
    if input_active && caret_pos.get() >= 0 {
        // Phase the blink off the most recent activation so the caret is on
        // for the first half-cycle after a line jump or click.
        let since = (ui.time() - caret_blink_reset.get()).max(0.0);
        let blink_on = (since % 1.06) < 0.53;
        if blink_on {
            let col = caret_pos.get() as f32;
            let cx = text_start_x + col * char_w;
            let cy0 = p0[1] + 2.0;
            let cy1 = p0[1] + row_h() - 2.0;
            dl.add_line([cx, cy0], [cx, cy1], theme::TEXT)
                .thickness(1.0)
                .build();
        }
    }

    // Live commit: any change pushes a `SetTwoWayLine` onto the undo stack,
    // and the next frame's diff reflects it. Equivalent edits on the same
    // line coalesce via `DiffEdit::merge` so the undo stack stays compact.
    if changed {
        if let Some(ln) = row.line_no {
            let two_way_side = match side {
                Side::Left => TwoWaySide::A,
                Side::Right => TwoWaySide::B,
            };
            pending_edits.push(DiffEdit::SetTwoWayLine {
                session_id,
                side: two_way_side,
                line_no: ln,
                new_text: buf,
                old_text: None,
            });
        }
    } else if input_active
        && was_empty
        && (ui.is_key_pressed(imgui::Key::Backspace) || ui.is_key_pressed(imgui::Key::Delete))
    {
        // Backspace/Delete on an already-empty input: remove the underlying
        // source line. (Single-char + Backspace deletes the char only; the
        // `was_empty` guard prevents the same keystroke from also removing
        // the line.)
        if let Some(ln) = row.line_no {
            let two_way_side = match side {
                Side::Left => TwoWaySide::A,
                Side::Right => TwoWaySide::B,
            };
            let line_idx = (ln as usize).saturating_sub(1);
            line_remove.set(Some(DiffEdit::SpliceTwoWayLines {
                session_id,
                side: two_way_side,
                start: line_idx,
                end: line_idx + 1,
                replacement: Vec::new(),
                old_target_lines: None,
            }));
        }
    }

    if input_active {
        focus_event.set(Some(side.as_focused_pane()));
    }

    drop(_font_tok);

    // Pin layout cursor exactly one row_h() down, regardless of input_text
    // height jitter, so the connector's content-y model stays accurate.
    ui.set_cursor_screen_pos([p0[0], p0[1] + row_h()]);

    if rmb_anchor {
        return row.line_no;
    }
    None
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

// ---------------------------------------------------------------------------
// Headless test harness
// ---------------------------------------------------------------------------
//
// Spike: drive `diff_view::render` inside a manually-constructed imgui
// context (no winit, no wgpu, no real window) so tests can observe scroll
// position and other post-frame state without running the GUI.
//
// What works: imgui's layout/widget code runs in process; setting
// `io.display_size` and building the default font atlas is enough for child
// windows, text-width calculations, hit-tests and scroll bookkeeping. Tests
// read `state.last_left_scroll_x` / `state.last_right_scroll_x` after the
// frame returns.
//
// What this does NOT yet do: drive the multi-frame splice scenario
// end-to-end. That needs applying the `pending_edits` between frames via
// `SessionStore` methods, then re-snapshotting and re-rendering. The shape
// is straightforward to extend; this spike just proves the harness.

#[cfg(test)]
mod headless_tests {
    use super::*;
    use crate::session::{SessionMode, SessionStore};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `imgui::Context` is a process-global singleton. `cargo test` runs
    /// tests in parallel by default, so we serialize through a static
    /// mutex. Holding the guard for the lifetime of the context guarantees
    /// at most one active context across the process.
    fn imgui_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Recover from poisoning: a panicked test shouldn't block others.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn build_ui_context() -> imgui::Context {
        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        // Enable keyboard nav so `set_keyboard_focus_here` actually engages
        // imgui's nav system (which is what triggers `ScrollToBringRectIntoView`
        // — the behavior we're testing for).
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Materialize the default font atlas so layouts can measure text.
        let _atlas = ctx.fonts().build_rgba32_texture();
        ctx
    }

    /// Load the live app's mono font (RobotoMono) into the context and
    /// return its `FontId`. Tests that care about pixel-accurate hit
    /// testing (e.g., double-click column → byte index) must push this
    /// font; otherwise the default proportional font's varying glyph
    /// widths make `(click_x - widget_x0) / char_w` lie.
    fn load_mono_font(ctx: &mut imgui::Context, size_pixels: f32) -> imgui::FontId {
        ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: include_bytes!("../../assets/RobotoMono-Regular.ttf"),
            size_pixels,
            config: Some(imgui::FontConfig {
                size_pixels,
                ..Default::default()
            }),
        }])
    }

    /// One render frame with no input: scroll_x should be at 0 and no pin
    /// should be queued. Confirms the harness wiring is sound (font atlas
    /// resolves, child windows lay out, the render fn writes back its
    /// per-frame scroll fields).
    #[test]
    fn headless_render_reads_scroll_x_at_zero() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        // Make one side wide enough that content_w > pane_w. Without this,
        // horizontal scroll is permanently 0 regardless of any bug.
        let long = "x".repeat(500);
        let text = format!("short\n{long}\ntail\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };

        let mut ctx = build_ui_context();
        let ui = ctx.new_frame();

        let mut view_state = DiffViewState::default();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();

        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    &store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    &mut view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();

        assert_eq!(view_state.last_left_scroll_x, 0.0);
        assert_eq!(view_state.last_right_scroll_x, 0.0);
        assert!(view_state.pin_scroll_x_after_splice.is_none());
        // No keys pressed, no splice; no edits should be queued.
        assert!(pending_edits.is_empty());
    }

    /// Pre-seed a selection on the short first line, inject Backspace via
    /// `io.add_key_event`, render once, and verify the splice fired and
    /// queued the scroll-x pin. Stops short of multi-frame application —
    /// the next iteration would apply the splice edit via
    /// `store.splice_two_way_lines`, render two more frames, and assert
    /// `last_left_scroll_x` stayed at 0.
    #[test]
    fn headless_splice_sets_pin() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };

        let mut ctx = build_ui_context();
        // Inject Backspace press for this frame.
        ctx.io_mut().add_key_event(imgui::Key::Backspace, true);

        let ui = ctx.new_frame();
        let mut view_state = DiffViewState::default();
        // Select "hello" on line 1 of side A.
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();

        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    &store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    &mut view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();

        // Splice path fired: edit queued, arrow_focus parked, pin set.
        assert!(
            pending_edits
                .iter()
                .any(|e| matches!(e, DiffEdit::SpliceTwoWayLines { .. })),
            "expected a SpliceTwoWayLines edit to be queued",
        );
        assert!(view_state.selection.is_none(), "selection should be cleared after splice");
        let (side, x, frames) = view_state
            .pin_scroll_x_after_splice
            .expect("pin should be set after splice");
        assert_eq!(side, Side::Left);
        assert_eq!(x, 0.0); // scroll_x was 0 going in
        assert_eq!(frames, 4);
    }

    /// Apply a queued `DiffEdit` to the store the same way the real app
    /// would, bypassing the undo stack (we don't care about undo in tests).
    fn apply_edit(store: &SessionStore, edit: DiffEdit) {
        match edit {
            DiffEdit::SpliceTwoWayLines {
                session_id,
                side,
                start,
                end,
                replacement,
                ..
            } => {
                let _ = store.splice_two_way_lines(session_id, side, start..end, replacement);
            }
            DiffEdit::SetTwoWayLine {
                session_id,
                side,
                line_no,
                new_text,
                ..
            } => {
                let _ = store.set_two_way_line(session_id, side, line_no, new_text);
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                hunk_id,
                target,
                ..
            } => {
                let _ = store.replace_hunk_side(session_id, hunk_id, target);
            }
        }
    }

    #[derive(Default)]
    struct FrameInput {
        backspace: bool,
        /// Place the mouse at this screen position before NewFrame.
        mouse_pos: Option<[f32; 2]>,
        /// Press or release the left mouse button.
        left_button: Option<bool>,
        /// Press an arrow key (UpArrow or DownArrow) this frame.
        arrow: Option<imgui::Key>,
        /// Hold the shift modifier this frame.
        shift: bool,
    }

    /// Run one render frame: snapshot the session, inject queued input
    /// events into imgui, build a Ui, call `render` inside a window,
    /// then apply queued `pending_edits` back to the store. Mirrors the
    /// per-frame flow `app::mod::frame_ui` runs in the real app.
    fn run_frame(
        ctx: &mut imgui::Context,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        input: FrameInput,
    ) {
        if let Some(pos) = input.mouse_pos {
            ctx.io_mut().add_mouse_pos_event(pos);
        }
        if let Some(down) = input.left_button {
            ctx.io_mut().add_mouse_button_event(imgui::MouseButton::Left, down);
        }
        if input.backspace {
            ctx.io_mut().add_key_event(imgui::Key::Backspace, true);
        }
        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };
        let ui = ctx.new_frame();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();
        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();
        for edit in pending_edits {
            apply_edit(store, edit);
        }
    }

    /// End-to-end state flow: pre-seed an in-line selection, press
    /// Backspace, render four frames (splice frame + the two frames the
    /// pin covers + an idle frame), applying queued edits to the store
    /// between frames. Asserts:
    ///   - the splice executed (line 1 was shortened);
    ///   - `pin_scroll_x_after_splice` was set with countdown=2 and the
    ///     captured x matches the splice-frame scroll_x;
    ///   - the countdown decremented (2 → 1 → cleared);
    ///   - scroll_x did not drift catastrophically from the splice-frame
    ///     baseline across the pin window.
    ///
    /// **Caveat — does NOT prove the pin prevents imgui's nav-scroll.**
    /// Verified empirically: temporarily replacing the pin push with
    /// `let pin_scroll_x: Option<(Side, f32)> = None;` at the top of
    /// `render` does NOT make this test fail. Things attempted to engage
    /// imgui's nav-scroll pipeline in headless mode:
    ///   - `ConfigFlags::NAV_ENABLE_KEYBOARD` set on `Io`.
    ///   - A click + release sequence injected via `add_mouse_pos_event`
    ///     and `add_mouse_button_event` to establish `NavWindow` and
    ///     activate the input_text widget before the splice.
    /// Neither makes imgui's `set_keyboard_focus_here` actually scroll
    /// the child window the way it does in the live app. The pipeline
    /// likely needs the renderer in the loop (or a full nav-state warmup
    /// across many frames with a stable `ActiveId` lifecycle) that a
    /// `Context::create()` + `new_frame()` loop doesn't reproduce. This
    /// test catches regressions in the state-machine wiring (the pin
    /// field's setup, countdown, and clearing); the imgui-side override
    /// behavior is only verified manually in the live GUI.
    #[test]
    fn headless_splice_preserves_scroll_x_across_pin_window() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = build_ui_context();
        let mut view_state = DiffViewState::default();
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Frame 0 (engage nav): click somewhere inside the left pane so
        // imgui sets `NavWindow` and activates the row's input_text. The
        // bug's trigger (`set_keyboard_focus_here` → nav-scroll) requires
        // an engaged nav system; without this click the headless context
        // never enters that code path. Position is chosen to land on a
        // visible row well inside the pane; exact value isn't critical.
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput {
                mouse_pos: Some([150.0, 80.0]),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput {
                left_button: Some(false),
                ..Default::default()
            },
        );
        // Restore the synthetic selection the click cleared. We're not
        // testing the click-then-shift-click extension path here, just
        // the splice path, so this is fine.
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Frame 1 (splice frame): Backspace pressed. Splice edit queued
        // and applied; pin is set with countdown=2. ImGui's per-frame
        // bookkeeping (active-widget tracking, layout) makes scroll_x
        // settle to a non-zero baseline whose exact value depends on
        // imgui internals — we just capture it as the reference.
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput { backspace: true, ..Default::default() },
        );
        let snap = store.snapshot(id).unwrap();
        if let SessionMode::TwoWay { a_lines, .. } = &snap.mode {
            // "hello world" with "hello" removed becomes " world".
            assert_eq!(a_lines[0], " world", "splice should have shortened line 1");
        } else {
            unreachable!();
        }
        let baseline_x = view_state.last_left_scroll_x;
        let pin = view_state
            .pin_scroll_x_after_splice
            .expect("pin should be set after splice");
        assert_eq!(pin.0, Side::Left);
        assert_eq!(pin.2, 4);
        assert!(
            (pin.1 - baseline_x).abs() < 1e-3,
            "pinned x ({}) should match this frame's captured scroll_x ({})",
            pin.1,
            baseline_x,
        );

        // Frame 2 (pin frame 1 of 2): the merged row's set_keyboard_focus_here
        // fires here and would, absent the pin, queue a nav-scroll that
        // pushes scroll_x toward (content_w - viewport_w) — i.e., several
        // thousand pixels. The pin holds it at baseline.
        run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
        // The original bug pushed scroll_x to roughly (content_w - pane_w),
        // which is several thousand pixels for our 500-char long line. A
        // tolerance well below that — but loose enough to ignore imgui's
        // own small per-frame layout adjustments — catches the regression.
        const MAX_DRIFT: f32 = 200.0;
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "frame 2: scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
            view_state.last_left_scroll_x,
        );
        // Countdown decrements; specific value isn't material here.
        assert!(matches!(
            view_state.pin_scroll_x_after_splice,
            Some((Side::Left, _, _))
        ));

        // Run enough idle frames to exhaust the countdown (max=4 today).
        for _ in 0..5 {
            run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
            assert!(
                (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
                "scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
                view_state.last_left_scroll_x,
            );
        }
        assert!(view_state.pin_scroll_x_after_splice.is_none());

        // Frame 4 (idle): pin has expired; scroll_x must still hold.
        run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "frame 4: scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
            view_state.last_left_scroll_x,
        );
        assert!(view_state.pin_scroll_x_after_splice.is_none());
    }

    // ---- wgpu-backed harness -------------------------------------------

    /// Try to spin up a headless wgpu device. Returns `None` if the
    /// machine has no usable adapter (common in CI without GPU); tests
    /// that need this should bail gracefully.
    fn try_init_wgpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("diffie-headless-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
            },
        ))
        .ok()?;
        Some((device, queue))
    }

    /// One frame with the full imgui → wgpu pipeline: build the Ui,
    /// call `render`, then `ctx.render()` + `Renderer::render` into an
    /// offscreen texture and `queue.submit`. Mirrors the live app's
    /// per-frame flow (`app::mod::render` around lines 425-456) minus
    /// the surface present.
    fn run_frame_with_wgpu(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono_font: Option<imgui::FontId>,
        input: FrameInput,
    ) {
        if let Some(pos) = input.mouse_pos {
            ctx.io_mut().add_mouse_pos_event(pos);
        }
        if let Some(down) = input.left_button {
            ctx.io_mut().add_mouse_button_event(imgui::MouseButton::Left, down);
        }
        if input.backspace {
            ctx.io_mut().add_key_event(imgui::Key::Backspace, true);
        }
        // Shift modifier must go through the event queue so NewFrame
        // updates `io.key_shift` for this frame's widgets.
        if input.shift {
            ctx.io_mut().add_key_event(imgui::Key::ModShift, true);
        }
        if let Some(k) = input.arrow {
            ctx.io_mut().add_key_event(k, true);
        }
        ctx.io_mut().delta_time = 1.0 / 60.0;

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };
        let ui = ctx.new_frame();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();
        ui.window("test")
            .size([1200.0, 800.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    view_state,
                    mono_font,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let draw_data = ctx.render();

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-target"),
            size: wgpu::Extent3d {
                width: 1200,
                height: 800,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            });
            renderer
                .render(draw_data, queue, device, &mut pass)
                .expect("imgui render");
        }
        queue.submit(Some(encoder.finish()));
        // No present (no surface). Pixel buffer is discarded; we only
        // care about imgui's post-frame state.
        // Release the arrow + shift so the next frame doesn't see them
        // as still pressed.
        if let Some(k) = input.arrow {
            ctx.io_mut().add_key_event(k, false);
        }
        if input.shift {
            ctx.io_mut().add_key_event(imgui::Key::ModShift, false);
        }

        for edit in pending_edits {
            apply_edit(store, edit);
        }
    }

    /// Double-clicking a word activates imgui's input_text native
    /// word-selection. The selection must survive into subsequent
    /// frames — previously our `suppress_imgui_selection` callback
    /// collapsed it the very next frame because we suppressed
    /// imgui's selection whenever `state.drag` was Some on this side,
    /// even at `threshold_passed=false` (which is the state right
    /// after any click). The fix gates suppression on
    /// `threshold_passed` so double-click survives.
    ///
    /// Requires the wgpu pipeline because imgui's input_text word-select
    /// only fires when the widget is fully active, which needs the same
    /// renderer-in-the-loop conditions as the scroll-pin bug.
    #[test]
    fn headless_wgpu_double_click_selects_word() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let store = SessionStore::new();
        let text = "alpha beta gamma\n";
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Make sure the synthetic clicks fall well inside imgui's default
        // double-click window (0.3s); each frame advances time by
        // delta_time, so two clicks separated by a few frames is fine.

        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();

        // Aim at the "beta" token on row 1 (the only row containing text;
        // row 0 is the diff's top row). Position calibration: pane origin
        // ~ (8, 33); gutter_w=60; chars start at x≈68; char_w with the
        // default font ≈ 7px. "alpha " is 6 chars → "beta" starts at
        // x ≈ 68 + 6*7 = 110. Click at x=120 (somewhere inside "beta").
        // y=40 lands inside the first row (height ~24, top ~33).
        let word_pos = [120.0, 40.0];

        // First click: down, then up.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(word_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        // Second click on the same pixel — completes the double-click
        // gesture. ImGui's input_text recognizes this and selects the
        // word under the cursor.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(word_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );

        // Run a few more idle frames to make sure the selection persists
        // past the suppression check (which fires only post-threshold).
        for _ in 0..3 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        // ImGui's input_text should have selected SOME word on the row.
        // We don't pin to a specific word — pane origin and char_w in the
        // headless context differ from the live app, so the exact hit
        // column shifts. What we're testing is the bug-relevant invariant:
        // a non-collapsed selection survives past the splice-suppression
        // window. With the bug present the selection would be collapsed
        // by frame 2 of the post-double-click run.
        let (side, line_no, start, end) = view_state
            .last_active_input_selection
            .expect("imgui input_text should have a selection after double-click");
        assert_eq!(side, Side::Left);
        assert_eq!(line_no, 1);
        assert!(end > start, "selection should be non-collapsed");
        let line = "alpha beta gamma";
        let selected = &line[start..end];
        assert!(
            ["alpha", "beta", "gamma"].contains(&selected),
            "expected a whole-word selection (alpha/beta/gamma); got bytes {start}..{end} = {selected:?}",
        );
    }

    /// Double-clicking on a punctuation char in a non-space run (e.g.,
    /// `target_arch=value`, `==`, `::`) must select just the single
    /// punct char, not the whole run. ImGui's default WORDLEFT/WORDRIGHT
    /// uses whitespace as the only word boundary, so for runs with no
    /// internal spaces it selects everything.
    ///
    /// Definitive regression check: a row of `===` (three equals signs,
    /// no whitespace) — without the fix imgui selects all three; with
    /// the fix any click in the rendered text selects a single `=`.
    #[test]
    fn headless_wgpu_double_click_punct_selects_single_char() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let line = "===";
        let text = format!("{line}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let click_pos = [90.0, 40.0];
        let mut view_state = DiffViewState::default();
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(click_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(click_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        for _ in 0..2 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        let (_, ln, start, end) = view_state
            .last_active_input_selection
            .expect("imgui input_text should have a selection after double-click");
        assert_eq!(ln, 1);
        let selected = &line[start..end];
        assert_eq!(
            selected, "=",
            "expected single '=' to be selected; got bytes {start}..{end} = {selected:?}",
        );
    }

    /// Drive the harness through enough frames to fully activate the
    /// input_text on `(side, line_no)` and let imgui settle. Returns
    /// the column the caret ended up at (which may differ slightly
    /// from the requested column due to imgui's clamping behavior).
    fn focus_row_and_settle(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono: imgui::FontId,
        side: Side,
        line_no: u32,
        col: usize,
    ) {
        view_state.arrow_focus = Some((side, line_no, col));
        // Several frames: set_keyboard_focus_here takes a couple of
        // frames to make the widget active; selection state stabilizes
        // after another frame or two.
        for _ in 0..5 {
            run_frame_with_wgpu(
                ctx, renderer, device, queue, target_format,
                store, id, view_state, Some(mono), FrameInput::default(),
            );
        }
    }

    /// Shift+Down inside the middle of a line extends `state.selection`
    /// across rows: anchor at the caret's pre-move position, caret at
    /// the same column on the line below. Standard editor behavior.
    #[test]
    fn headless_wgpu_shift_down_extends_selection_to_next_line() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park the caret at column 4 on line 1 via the arrow-focus
        // mechanism (more reliable in headless than relying on a click
        // to activate imgui's input_text).
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 4,
        );

        // Press Shift+Down. Selection should now span (1, 4) → (2, 4).
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::DownArrow),
                shift: true,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        let sel = view_state
            .selection
            .as_ref()
            .expect("Shift+Down should produce a selection");
        assert_eq!(sel.side, Side::Left);
        assert_eq!(
            sel.anchor,
            SelPoint { line_no: 1, col: 4 },
            "anchor should be at the pre-move caret position",
        );
        assert_eq!(
            sel.caret,
            SelPoint { line_no: 2, col: 4 },
            "caret should jump to same column on line 2",
        );
    }

    /// Left or Right arrow inside an active row must reset
    /// `state.caret_blink_reset` to the current imgui time so the
    /// manually-drawn caret is on for the first half of the new
    /// blink cycle — otherwise the user would press Left/Right and
    /// see no caret for up to half a second.
    #[test]
    fn headless_wgpu_lateral_arrow_resets_caret_blink() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Activate line 1 column 4. After settle, blink_reset is set
        // to the activation frame's time.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 4,
        );
        let blink_at_activation = view_state.caret_blink_reset;

        // Run several idle frames so imgui's clock advances well past
        // the activation timestamp. blink_reset should NOT change —
        // idle time with no input doesn't reset the blink.
        for _ in 0..10 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        assert_eq!(
            view_state.caret_blink_reset, blink_at_activation,
            "idle frames must not reset blink",
        );

        // Press RightArrow. This should bump blink_reset to a later
        // imgui time so the caret is visible at the new position.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        let blink_after_right = view_state.caret_blink_reset;
        assert!(
            blink_after_right > blink_at_activation,
            "RightArrow should reset blink_reset to a later time \
             (was {blink_at_activation}, now {blink_after_right})",
        );

        // Idle frames again — blink_reset should hold steady.
        for _ in 0..5 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        assert_eq!(
            view_state.caret_blink_reset, blink_after_right,
            "idle frames after RightArrow must not reset again",
        );

        // LeftArrow likewise resets.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::LeftArrow),
                ..Default::default()
            },
        );
        assert!(
            view_state.caret_blink_reset > blink_after_right,
            "LeftArrow should reset blink_reset",
        );
    }

    /// Pressing a plain arrow key (no shift modifier) inside an active
    /// row collapses any existing cross-row `state.selection`. Standard
    /// editor behavior: arrow keys without shift dismiss the selection
    /// and move the caret as a point.
    #[test]
    fn headless_wgpu_plain_arrow_clears_selection() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\nuvwxyz0123\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Activate line 2's input_text.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 2, 4,
        );
        // Pre-seed a cross-row selection from (1, 4) → (2, 4).
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 4 },
            caret: SelPoint { line_no: 2, col: 4 },
        });

        // Plain Down (no shift) should clear the selection.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::DownArrow),
                shift: false,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        assert!(
            view_state.selection.is_none(),
            "plain DownArrow should have cleared selection; got {:?}",
            view_state.selection.as_ref().map(|s| (s.anchor, s.caret)),
        );
    }

    /// Shift+Up mirror of the Shift+Down test.
    #[test]
    fn headless_wgpu_shift_up_extends_selection_to_prev_line() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\nuvwxyz0123\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 2, 4,
        );

        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::UpArrow),
                shift: true,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        let sel = view_state
            .selection
            .as_ref()
            .expect("Shift+Up should produce a selection");
        assert_eq!(sel.side, Side::Left);
        assert_eq!(
            sel.anchor,
            SelPoint { line_no: 2, col: 4 },
            "anchor should be at the pre-move caret position",
        );
        assert_eq!(
            sel.caret,
            SelPoint { line_no: 1, col: 4 },
            "caret should jump to same column on line 1",
        );
    }

    /// User-reported scenario: double-clicking on `=` in
    /// `#[cfg(target_arch = "wasm32")]` selects just `=`, not
    /// `target_arch`. This test loads the live app's mono font
    /// (RobotoMono) so `char_w` matches imgui's hit-test and we can
    /// drive a click at the exact `=` column.
    ///
    /// Note: imgui's default WORDLEFT/WORDRIGHT happens to also select
    /// just `=` when the cursor lands directly on it (because `=` is
    /// flanked by spaces, forming a one-char non-space run). So this
    /// test PASSES with or without the override fix — it's a scenario
    /// documentation rather than a bug-catcher. The punct-run test
    /// above is the regression gate for the user's underlying class
    /// of issue (no-space punct runs).
    #[test]
    fn headless_wgpu_double_click_equal_in_cfg_selects_just_equal() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let line = "#[cfg(target_arch = \"wasm32\")]";
        let text = format!("{line}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Load mono font so calc_text_size("m") returns the per-char
        // width imgui actually uses for hit-testing — without this the
        // default proportional font breaks our column math.
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // The `=` is at byte (and char) index 18. With the mono font
        // and 1200×800 display, widget_x0 ≈ 76 and char_w ≈ 7.8;
        // x ≈ 76 + 18*7.8 ≈ 217. We sweep a small range to absorb any
        // few-pixel drift; with the mono font in place this only needs
        // one or two iterations to hit `=`, and crucially the imgui
        // state doesn't bleed because the override fires once on each
        // double-click detection.
        let equal_byte_idx = 18;
        let mut hit_equal = false;
        for click_x in (160..=220).step_by(2) {
            let mut view_state = DiffViewState::default();
            let click_pos = [click_x as f32, 40.0];
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput {
                    mouse_pos: Some(click_pos),
                    left_button: Some(true),
                    ..Default::default()
                },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput { left_button: Some(false), ..Default::default() },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput {
                    mouse_pos: Some(click_pos),
                    left_button: Some(true),
                    ..Default::default()
                },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput { left_button: Some(false), ..Default::default() },
            );
            for _ in 0..2 {
                run_frame_with_wgpu(
                    &mut ctx, &mut renderer, &device, &queue, target_format,
                    &store, id, &mut view_state, Some(mono), FrameInput::default(),
                );
            }

            let Some((_, ln, start, end)) = view_state.last_active_input_selection else {
                continue;
            };
            assert_eq!(ln, 1);
            if start == equal_byte_idx && end == equal_byte_idx + 1 {
                hit_equal = true;
                break;
            }
        }
        assert!(
            hit_equal,
            "no swept x position selected exactly '='; calibration drifted",
        );
    }

    /// Real-renderer end-to-end: drives the full imgui → wgpu pipeline
    /// per frame (ctx.render → CommandEncoder → render_pass →
    /// Renderer::render → queue.submit, against an offscreen target),
    /// then asserts that across the pin window scroll_x stays at the
    /// post-splice baseline.
    ///
    /// **This test does catch the original bug.** With the pin push
    /// disabled (replace the `pin_scroll_x` capture at the top of
    /// `render` with `None`), scroll_x drifts from 0 to gutter_w (~60px)
    /// — exactly the live-app symptom — and the test fails. With the
    /// pin active, scroll_x stays at the baseline.
    ///
    /// Notes:
    ///   - The wgpu device + queue is required: without rendering
    ///     submission, imgui's nav-scroll pipeline doesn't fully trip.
    ///   - `NAV_ENABLE_KEYBOARD` config flag is required (sets up the
    ///     nav system so set_keyboard_focus_here engages it).
    ///   - The pin countdown must be ≥3 frames to outlast imgui's
    ///     widget-activation cycle; we use 4 for safety.
    ///   - This test takes ~1.5s due to wgpu init + per-frame texture
    ///     allocation; the in-memory variant covers state-machine
    ///     regressions in ~20ms.
    #[test]
    fn headless_wgpu_splice_preserves_scroll_x() {
        let _guard = imgui_lock();

        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available in this environment");
            return;
        };

        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Note: imgui_wgpu::Renderer::new builds the font atlas itself.

        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();

        // Engage NavWindow: click + release inside the left pane.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some([150.0, 80.0]),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Splice frame: Backspace pressed.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { backspace: true, ..Default::default() },
        );
        let snap = store.snapshot(id).unwrap();
        if let SessionMode::TwoWay { a_lines, .. } = &snap.mode {
            assert_eq!(a_lines[0], " world", "splice should have shortened line 1");
        }
        let baseline_x = view_state.last_left_scroll_x;
        assert!(matches!(
            view_state.pin_scroll_x_after_splice,
            Some((Side::Left, _, _))
        ));

        // Run many idle frames — enough to outlast any pin countdown.
        for _ in 0..15 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        // The live-app bug shifts scroll_x by exactly gutter_w (60 px
        // at code_font_zoom=1.0). A 10-px bound catches that with margin
        // for any sub-pixel float drift but is way below imgui's
        // bug-magnitude scroll.
        const MAX_DRIFT: f32 = 10.0;
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px) — pin failed",
            view_state.last_left_scroll_x,
        );
        assert!(view_state.pin_scroll_x_after_splice.is_none());
    }
}
