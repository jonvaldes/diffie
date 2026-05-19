//! GUI-side state and rendering for the find-in-panes feature.
//!
//! Owns the `AppSearch` struct that lives on `AppState`, the per-frame match
//! collection that every pane registers into, and the find input rendered by
//! the engine bar. The pure matching logic lives in `crate::search`.

use std::collections::HashMap;

use crate::search::{find_matches_in_text, CompiledQuery, Match};

/// Identifies a text pane uniquely within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneId {
    TwoWayA,
    TwoWayB,
    ThreeWayBase,
    ThreeWayLocal,
    ThreeWayRemote,
    Result,
}

impl PaneId {
    /// Map a `FocusedPane` to its `PaneId`. They have identical variants but
    /// live in different modules so the search code can stay independent of
    /// the high-level focus enum.
    pub fn from_focused(p: super::FocusedPane) -> Self {
        match p {
            super::FocusedPane::TwoWayA => PaneId::TwoWayA,
            super::FocusedPane::TwoWayB => PaneId::TwoWayB,
            super::FocusedPane::ThreeWayBase => PaneId::ThreeWayBase,
            super::FocusedPane::ThreeWayLocal => PaneId::ThreeWayLocal,
            super::FocusedPane::ThreeWayRemote => PaneId::ThreeWayRemote,
            super::FocusedPane::Result => PaneId::Result,
        }
    }
}

/// Direction for an Enter / Shift+Enter jump.
#[derive(Clone, Copy, Debug)]
pub enum JumpDir {
    Next,
    Prev,
}

/// Live find state, persisted across frames as part of `AppState`.
pub struct AppSearch {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// `Some(Ok(q))` for a valid non-empty query, `Some(Err(()))` for invalid
    /// regex, `None` when the query is empty.
    pub compiled: Result<Option<CompiledQuery>, ()>,
    /// Per-pane match list, refreshed every frame.
    pub matches: HashMap<PaneId, Vec<Match>>,
    /// Total match count across all panes this frame.
    pub total_matches: usize,
    /// Set by Ctrl+F; consumed by the engine bar to focus the input.
    pub focus_request: bool,
    /// Set by Enter / F3 / Shift+F3; consumed by view code.
    pub jump_request: Option<JumpDir>,
    /// Pane + match index currently jumped to (the "current" match).
    pub current: Option<(PaneId, usize)>,
}

impl Default for AppSearch {
    fn default() -> Self {
        Self {
            query: String::new(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            compiled: Ok(None),
            matches: HashMap::new(),
            total_matches: 0,
            focus_request: false,
            jump_request: None,
            current: None,
        }
    }
}

impl AppSearch {
    /// Rebuild `compiled` from the current query and toggles. Call after any
    /// change to `query`, `case_sensitive`, `whole_word`, or `regex`.
    pub fn recompile(&mut self) {
        // Any query/options change invalidates the "current match" index:
        // the match set just changed, so the stored index no longer
        // identifies the same hit.
        self.current = None;
        if self.query.is_empty() {
            self.compiled = Ok(None);
            return;
        }
        match CompiledQuery::build(
            &self.query,
            self.case_sensitive,
            self.whole_word,
            self.regex,
        ) {
            Ok(q) => self.compiled = Ok(Some(q)),
            Err(_) => self.compiled = Err(()),
        }
    }

    /// `true` while the find field should render red.
    pub fn is_error_state(&self) -> bool {
        if self.query.is_empty() {
            return false;
        }
        match &self.compiled {
            Err(()) => true,
            Ok(Some(_)) => self.total_matches == 0,
            Ok(None) => false,
        }
    }

    /// Clear per-frame state at the top of a frame.
    pub fn begin_frame(&mut self) {
        self.matches.clear();
        self.total_matches = 0;
    }

    /// Register a pane's match list for this frame.
    pub fn register(&mut self, pane: PaneId, ms: Vec<Match>) {
        self.total_matches += ms.len();
        self.matches.insert(pane, ms);
    }

    /// Get the active compiled query if there is one.
    pub fn active_query(&self) -> Option<&CompiledQuery> {
        match &self.compiled {
            Ok(Some(q)) => Some(q),
            _ => None,
        }
    }
}

/// Render the find input + toggles inside the engine bar. Returns whether
/// the user pressed Enter (and in which direction) so the caller can post a
/// jump request.
pub fn render_find_bar(ui: &imgui::Ui, s: &mut AppSearch) {
    use imgui::StyleColor;

    // Consume focus request.
    if s.focus_request {
        ui.set_keyboard_focus_here();
        s.focus_request = false;
    }

    // Push red frame bg when in error state.
    let _err_color = s.is_error_state().then(|| {
        let red = [0.65, 0.15, 0.18, 1.0];
        (
            ui.push_style_color(StyleColor::FrameBg, red),
            ui.push_style_color(StyleColor::FrameBgHovered, red),
            ui.push_style_color(StyleColor::FrameBgActive, red),
        )
    });

    let mut q = s.query.clone();
    let frame_h = ui.frame_height();
    let prev_w = ui.push_item_width(220.0);
    let entered = ui
        .input_text("##find_input", &mut q)
        .hint("Find (Ctrl+F)")
        .enter_returns_true(true)
        .build();
    prev_w.end();

    let input_focused = ui.is_item_focused();
    // Escape clears the query while the input is focused.
    if input_focused && ui.is_key_pressed(imgui::Key::Escape) {
        q.clear();
    }

    if q != s.query {
        s.query = q;
        s.recompile();
    }

    if entered {
        s.jump_request = Some(if ui.io().key_shift {
            JumpDir::Prev
        } else {
            JumpDir::Next
        });
    }

    drop(_err_color);

    // Toggle buttons: Aa, \b, .*
    let style = ui.clone_style();
    let pad_x = style.frame_padding[0];
    let toggle_w = (ui.calc_text_size("Aa")[0] + pad_x * 2.0).max(frame_h);

    let mut changed = false;
    ui.same_line_with_spacing(0.0, 4.0);
    changed |= render_toggle(ui, "Aa", "Match case", toggle_w, frame_h, &mut s.case_sensitive);
    ui.same_line_with_spacing(0.0, 4.0);
    changed |= render_toggle(ui, "\\b", "Whole word", toggle_w, frame_h, &mut s.whole_word);
    ui.same_line_with_spacing(0.0, 4.0);
    changed |= render_toggle(ui, ".*", "Regex", toggle_w, frame_h, &mut s.regex);
    if changed {
        s.recompile();
    }
}

/// Soft fill behind every match.
pub const MATCH_FILL: [f32; 4] = [1.0, 0.92, 0.2, 0.35];
/// Stronger fill for the "current" match (the one a jump landed on).
pub const CURRENT_MATCH_FILL: [f32; 4] = [1.0, 0.55, 0.1, 0.7];
/// Scrollbar tick color.
pub const TICK_COLOR: [f32; 4] = [1.0, 0.55, 0.1, 0.85];

/// Compute the match list for a pane's text and register it with the search
/// state. Returns the freshly-cloned list so the caller can use it for
/// painting and jump logic without going back through `search.matches`.
pub fn compute_and_register(
    search: &mut AppSearch,
    pane: PaneId,
    text: &str,
) -> Vec<Match> {
    let Some(q) = search.active_query() else {
        search.register(pane, Vec::new());
        return Vec::new();
    };
    let ms = find_matches_in_text(text, q);
    search.register(pane, ms.clone());
    ms
}

/// Paint highlight rectangles behind every match in `widget_rect`. Glyphs
/// are painted by the caller on top; the soft fill bleeds through enough
/// for the text to stay legible.
///
/// `char_advance` is the per-character pixel width of the monospace font.
/// `pane_id` is used to find the "current" match within `current`.
#[allow(clippy::too_many_arguments)]
pub fn paint_highlights(
    ui: &imgui::Ui,
    widget_rect: [f32; 4],
    matches: &[Match],
    current: Option<(PaneId, usize)>,
    pane: PaneId,
    scroll_y: f32,
    scroll_x: f32,
    lh: f32,
    char_advance: f32,
    frame_pad: [f32; 2],
) {
    if matches.is_empty() {
        return;
    }
    let dl = ui.get_window_draw_list();
    let clip_x0 = widget_rect[0];
    let clip_y0 = widget_rect[1];
    let clip_x1 = widget_rect[2];
    let clip_y1 = widget_rect[3];
    dl.with_clip_rect_intersect([clip_x0, clip_y0], [clip_x1, clip_y1], || {
        for (i, m) in matches.iter().enumerate() {
            let y0 = widget_rect[1] + frame_pad[1] + (m.line as f32 - 1.0) * lh - scroll_y;
            let y1 = y0 + lh;
            if y1 < clip_y0 || y0 > clip_y1 {
                continue;
            }
            let x0 = widget_rect[0] + frame_pad[0] + m.start_col as f32 * char_advance - scroll_x;
            let x1 = widget_rect[0] + frame_pad[0] + m.end_col as f32 * char_advance - scroll_x;
            let is_current = current == Some((pane, i));
            let color = if is_current { CURRENT_MATCH_FILL } else { MATCH_FILL };
            dl.add_rect([x0, y0], [x1, y1], color).filled(true).build();
        }
    });
}

/// Paint scrollbar ticks at every matching line. `vbar_rect` is the
/// `[x0, y0, x1, y1]` of the scrollbar track in screen space.
pub fn paint_scrollbar_ticks(
    ui: &imgui::Ui,
    vbar_rect: [f32; 4],
    total_lines: u32,
    matches: &[Match],
) {
    if matches.is_empty() || total_lines == 0 {
        return;
    }
    let dl = ui.get_window_draw_list();
    let track_top = vbar_rect[1];
    let track_h = (vbar_rect[3] - vbar_rect[1]).max(1.0);
    let x0 = vbar_rect[0];
    let x1 = vbar_rect[2];
    let mut last_line = 0u32;
    for m in matches {
        if m.line == last_line {
            continue;
        }
        last_line = m.line;
        let frac = (m.line.saturating_sub(1)) as f32 / total_lines as f32;
        let y = track_top + frac * track_h;
        dl.add_rect([x0, y - 1.0], [x1, y + 1.0], TICK_COLOR)
            .filled(true)
            .build();
    }
}

/// Result of a jump consumed by a pane: the new caret byte offset and the
/// line we should scroll to (so the view can center it). `match_index` is
/// stored back into `AppSearch.current` by the caller.
#[derive(Debug, Clone, Copy)]
pub struct JumpResult {
    pub caret_byte: usize,
    pub line: u32,
    pub match_index: usize,
}

/// If a jump is pending and this pane is the focused one (or no pane is
/// focused and this pane has matches), pick the next/prev match relative to
/// `caret_byte` and return it. The caller is expected to:
///   1. Move its caret to `caret_byte`.
///   2. Scroll `line` into view (centered).
///   3. Set `search.current = Some((pane, match_index))`.
///   4. Clear `search.jump_request` (this helper does it).
///
/// `focused` should be the `PaneId` mapped from `AppState.focused`.
pub fn consume_jump_for_pane(
    search: &mut AppSearch,
    pane: PaneId,
    matches: &[Match],
    caret_byte: usize,
    focused: Option<PaneId>,
) -> Option<JumpResult> {
    let Some(dir) = search.jump_request else { return None; };
    // Pane must be either the focused one, or — if nothing is focused — the
    // first pane with matches.
    let target_ok = match focused {
        Some(f) => f == pane,
        None => !matches.is_empty(),
    };
    if !target_ok || matches.is_empty() {
        return None;
    }
    // If we just landed on a match in this pane, advance from that match's
    // index rather than re-deriving from the widget caret. The widget caret
    // gets reset to 0 when we bump input_epoch on jumps, which would
    // otherwise send "Next" back to the first match instead of advancing.
    let pinned_idx = match search.current {
        Some((p, i)) if p == pane && i < matches.len() => Some(i),
        _ => None,
    };
    let idx = if let Some(i) = pinned_idx {
        match dir {
            JumpDir::Next => (i + 1) % matches.len(),
            JumpDir::Prev => (i + matches.len() - 1) % matches.len(),
        }
    } else {
        match dir {
            JumpDir::Next => matches
                .iter()
                .position(|m| m.byte_start >= caret_byte)
                .unwrap_or(0),
            JumpDir::Prev => matches
                .iter()
                .rposition(|m| m.byte_end <= caret_byte)
                .unwrap_or(matches.len() - 1),
        }
    };
    let m = &matches[idx];
    search.jump_request = None;
    search.current = Some((pane, idx));
    Some(JumpResult {
        caret_byte: m.byte_end,
        line: m.line,
        match_index: idx,
    })
}

fn render_toggle(
    ui: &imgui::Ui,
    label: &str,
    tooltip: &str,
    w: f32,
    h: f32,
    flag: &mut bool,
) -> bool {
    use imgui::StyleColor;
    use crate::app::theme;
    let _tint = flag.then(|| {
        (
            ui.push_style_color(StyleColor::Button, theme::SURFACE1()),
            ui.push_style_color(StyleColor::ButtonHovered, theme::SURFACE2()),
            ui.push_style_color(StyleColor::ButtonActive, theme::OVERLAY0()),
        )
    });
    let mut changed = false;
    if ui.button_with_size(format!("{label}##find_tog_{label}"), [w, h]) {
        *flag = !*flag;
        changed = true;
    }
    if ui.is_item_hovered() {
        ui.tooltip_text(tooltip);
    }
    changed
}
