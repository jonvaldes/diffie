//! Editable result pane.
//!
//! Shows `SessionStore::compute_result` for the active session and lets the
//! user override it. Edits flow back through `update_manual_result`, so the
//! session's `manual_result` field is the source of truth once the user
//! types. When the user is not editing, decision/resolution changes upstream
//! recompute the result and refresh the buffer here.
//!
//! Layout: a left gutter holds per-hunk resolution-picker icons (Remote /
//! Base / Local — only the ones with content for that hunk kind), an
//! originating-side accent stripe sits between the gutter and the text
//! widget, and the text widget itself is a transparent `input_text_multiline`
//! whose text is painted manually so it can carry syntax-highlight colors.

use std::cell::Cell;
use std::collections::HashMap;

use imgui::{FontId, StyleVar, Ui};

use crate::app::merge_view::MergeViewState;
use crate::app::syntax::LineSpans;
use crate::app::syntax_paint;
use crate::app::theme;
use crate::merge::{hunk_output_ranges, result_line_origins, LineOrigin, MergeHunk, Resolution};
use crate::session::{SessionId, SessionStore};

/// Slot in `MergeViewState`'s 4-way scroll arrays that we use for the
/// result pane. Slots 0..3 are the three upper panes
/// (Base / Local / Remote); 3 is the result.
pub const RESULT_SCROLL_SLOT: usize = 3;

/// Wheel/easing constants — kept in sync with the values in `merge_view`
/// so result-pane scrolling feels identical to the three upper panes.
const SCROLL_LINES_PER_WHEEL_TICK: f32 = 3.0;
const SCROLL_SMOOTH_SPEED: f32 = 25.0;
const SCROLL_SNAP_EPSILON: f32 = 0.5;
/// Tolerance for "scroll moved by something other than us" (i.e. user
/// dragged the native vertical scrollbar). Keep it well above the easing
/// snap epsilon so a frame mid-ease isn't misread as a drag.
const DRAG_DETECT_TOLERANCE: f32 = 1.5;

const STRIPE_W: f32 = 4.0;
const GUTTER_W: f32 = 56.0;
/// Width of the line-number column drawn to the left of the picker gutter.
/// Mirrors the merge view's per-pane gutter so the result pane reads
/// visually consistent with the three upper panes.
const LN_GUTTER_W: f32 = 50.0;
const ICON_HALF: f32 = 6.0;
const ICON_SPACING: f32 = 18.0;

#[derive(Default)]
pub struct ResultState {
    buffer: String,
    was_active_last_frame: bool,
    initialized: bool,
    /// Bumped on picker-driven mutations so we re-sync from `compute_result`
    /// next frame regardless of the editor's active state.
    force_reload: bool,
    /// User-adjusted result pane height (px). `None` means use the default
    /// (50% of available vertical space). Set by the splitter between merge & result.
    pub pane_height: Option<f32>,
}

/// Output line ranges + view height returned to the caller so it can run
/// the unified 4-way scroll sync alongside `merge_view::render`.
#[derive(Default)]
pub struct ResultPaneSync {
    /// Per-hunk (id, content_y_top, content_y_bottom) in result-pane content
    /// space. Empty when nothing rendered (e.g. snapshot failure).
    pub ranges: Vec<(u32, f32, f32)>,
    pub view_h: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &Ui,
    store: &SessionStore,
    session_id: SessionId,
    state: &mut ResultState,
    merge_state: &mut MergeViewState,
    mono_font: Option<FontId>,
    focus_request: &mut Option<crate::app::FocusedPane>,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
    result_highlights: &[LineSpans],
) -> ResultPaneSync {
    if state.force_reload || !state.was_active_last_frame {
        if let Ok(text) = store.compute_result(session_id) {
            if text != state.buffer {
                state.buffer = text;
            }
        }
        state.initialized = true;
        state.force_reload = false;
    }

    if !state.initialized {
        ui.text_disabled("Computing…");
        return ResultPaneSync::default();
    }

    let avail = ui.content_region_avail();
    let _font_tok = mono_font.map(|f| ui.push_font(f));
    let origin = ui.cursor_screen_pos();
    let lh = ui.text_line_height();

    // Column layout, left → right:
    //   [line-number gutter] [picker gutter] [stripe] [text]
    // Line numbers and picker icons each get their own gutter so the picker
    // column matches what it was before, just shifted right by `LN_GUTTER_W`.
    let widget_h = avail[1];
    let ln_gutter_rect = [
        origin[0],
        origin[1],
        origin[0] + LN_GUTTER_W,
        origin[1] + widget_h,
    ];
    let gutter_rect = [
        origin[0] + LN_GUTTER_W,
        origin[1],
        origin[0] + LN_GUTTER_W + GUTTER_W,
        origin[1] + widget_h,
    ];
    let stripe_x = origin[0] + LN_GUTTER_W + GUTTER_W;
    let stripe_rect = [stripe_x, origin[1], stripe_x + STRIPE_W, origin[1] + widget_h];
    let widget_pos = [stripe_x + STRIPE_W, origin[1]];
    let widget_w = (avail[0] - LN_GUTTER_W - GUTTER_W - STRIPE_W).max(40.0);

    // Reserve the gutter strip up front. The actual icons + hit tests are
    // painted/placed AFTER the text widget so we can read its scroll position.
    ui.set_cursor_screen_pos(origin);
    ui.dummy([LN_GUTTER_W + GUTTER_W, widget_h]);

    // Size the inner multiline to the full content height so it never has
    // to scroll internally; the outer child window owns scrolling — that way
    // wheel AND scrollbar drag both work natively, and we just read the
    // resulting scroll_y back out for the manual text + gutter paint.
    let line_count = state.buffer.lines().count().max(1) as f32;
    let style = ui.clone_style();
    let padding_y = style.frame_padding[1];
    let inner_h = line_count * lh + padding_y * 2.0;

    ui.set_cursor_screen_pos(widget_pos);

    let new_buf_cell: Cell<Option<String>> = Cell::new(None);
    let widget_active_cell: Cell<bool> = Cell::new(false);
    let widget_focused_cell: Cell<bool> = Cell::new(false);
    let scroll_y_cell: Cell<f32> = Cell::new(0.0);
    let caret_byte: Cell<i32> = Cell::new(-1);

    // Scroll model: keep this in step with `merge_view::render_pane`. We
    // ease a `displayed` value from `last` toward `target`, and override
    // imgui's native vertical scroll on the outer child every frame via
    // `igSetNextWindowScroll`. Wheel events feed `target` directly;
    // sync from `merge_view::sync_scrolls` (slot 3) likewise feeds
    // `target`. Native scrollbar drag is detected post-build by comparing
    // the reported scroll_y to what we set, and snaps `target` instantly
    // so the drag tracks the cursor.
    let pending_scroll = merge_state.pending[RESULT_SCROLL_SLOT].take();
    let max_scroll = (inner_h - widget_h).max(0.0);
    let wheel_v = if ui.is_mouse_hovering_rect(
        widget_pos,
        [widget_pos[0] + widget_w, widget_pos[1] + widget_h],
    ) {
        ui.io().mouse_wheel
    } else {
        0.0
    };
    let prev_target = merge_state.target[RESULT_SCROLL_SLOT];
    let prev_displayed = merge_state.last[RESULT_SCROLL_SLOT];
    let mut target = if let Some(s) = pending_scroll {
        s
    } else {
        prev_target - wheel_v * lh * SCROLL_LINES_PER_WHEEL_TICK
    };
    target = target.clamp(0.0, max_scroll);

    // Clamp dt to ~one 30fps frame so a wake-up frame after idle doesn't
    // collapse the easing into a single jump (mirrors merge_view).
    let dt = ui.io().delta_time.max(0.0).min(0.033);
    let k = 1.0 - (-dt * SCROLL_SMOOTH_SPEED).exp();
    let mut displayed = prev_displayed + (target - prev_displayed) * k;
    if (target - displayed).abs() < SCROLL_SNAP_EPSILON {
        displayed = target;
    }
    displayed = displayed.clamp(0.0, max_scroll);

    unsafe {
        imgui::sys::igSetNextWindowScroll(imgui::sys::ImVec2 {
            x: -1.0,
            y: displayed,
        });
    }

    let _wp = ui.push_style_var(StyleVar::WindowPadding([0.0, 0.0]));
    let _cbg = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0, 0.0, 0.0, 0.0]);
    ui.child_window("##diffie_result_outer")
        .size([widget_w, widget_h])
        .build(|| {
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

            // Inner multiline sized to full content — no internal scroll.
            let changed = ui
                .input_text_multiline("##diffie_result", &mut state.buffer, [widget_w, inner_h])
                .callback(
                    imgui::InputTextMultilineCallback::ALWAYS,
                    CaretCapture { cursor: &caret_byte },
                )
                .build();
            widget_active_cell.set(ui.is_item_active());
            widget_focused_cell.set(ui.is_item_focused());
            if changed {
                new_buf_cell.set(Some(state.buffer.clone()));
            }
            // Read the OUTER child's scroll_y (we're back in its scope after
            // the multiline build returns). Imgui's native wheel + scrollbar
            // drag both feed into this value.
            unsafe {
                scroll_y_cell.set(imgui::sys::igGetScrollY());
            }
        });
    drop(_cbg);
    drop(_wp);

    let scroll_y = scroll_y_cell.get();
    let widget_active = widget_active_cell.get();
    let widget_focused = widget_focused_cell.get();

    // Per-line origin tints: only meaningful when the buffer matches the
    // computed merge output (no manual edit overriding things). We compare
    // line counts as a cheap guard.
    let origins = result_line_origins(hunks, resolutions);
    let origins_match = origins.len() == state.buffer.lines().count();

    // Paint syntax-highlighted text on top of the transparent multiline.
    paint_text(
        ui,
        widget_pos,
        widget_w,
        widget_h,
        &state.buffer,
        result_highlights,
        scroll_y,
        lh,
        if origins_match { Some(&origins) } else { None },
    );

    // Manual caret. Imgui's Text color is transparent so its native caret
    // doesn't show; paint our own blinking vertical line at the caret byte.
    if widget_active {
        let style = ui.clone_style();
        let dl = ui.get_window_draw_list();
        syntax_paint::paint_caret(
            ui,
            &dl,
            [widget_pos[0], widget_pos[1], widget_pos[0] + widget_w, widget_pos[1] + widget_h],
            &state.buffer,
            caret_byte.get(),
            0.0,
            scroll_y,
            style.frame_padding[0],
            style.frame_padding[1],
            lh,
        );
    }

    // Origin-side accent stripe between gutter and text widget.
    paint_origin_stripes(
        ui,
        stripe_rect,
        scroll_y,
        lh,
        hunks,
        resolutions,
    );

    // Picker icons in the gutter + hit testing. Dispatches resolution changes
    // directly to the store; sets force_reload so we re-sync next frame.
    let clicked = paint_and_hit_pickers(
        ui,
        gutter_rect,
        scroll_y,
        lh,
        hunks,
        resolutions,
    );
    if let Some((hunk_id, resolution)) = clicked {
        let _ = store.set_three_way_resolution(session_id, hunk_id, resolution);
        state.force_reload = true;
    }

    drop(_font_tok);

    if widget_active || widget_focused {
        *focus_request = Some(crate::app::FocusedPane::Result);
    }

    if let Some(new_text) = new_buf_cell.take() {
        let _ = store.update_manual_result(session_id, new_text);
    }
    state.was_active_last_frame = widget_active;

    // Line-number gutter (drawn after we know `scroll_y` so it stays aligned
    // with the multiline as the user scrolls).
    paint_line_number_gutter(
        ui,
        ln_gutter_rect,
        scroll_y,
        lh,
        state.buffer.lines().count().max(1) as u32,
    );

    // 4-way scroll sync: post-build, detect whether the user dragged the
    // native vertical scrollbar (the only path that can move scroll_y away
    // from our `displayed` override) and snap target+last to the dragged
    // position so the user sees a 1:1 follow. Otherwise the canonical
    // values are our own eased `displayed` / `target`.
    let drag_delta = scroll_y - displayed;
    let (final_target, final_displayed) = if drag_delta.abs() > DRAG_DETECT_TOLERANCE {
        let snapped = scroll_y.clamp(0.0, max_scroll);
        (snapped, snapped)
    } else {
        (target, displayed)
    };
    merge_state.target[RESULT_SCROLL_SLOT] = final_target;
    merge_state.last[RESULT_SCROLL_SLOT] = final_displayed;

    // Per-hunk content-y ranges in the result pane, used by the unified sync.
    let mut ranges: Vec<(u32, f32, f32)> = Vec::new();
    for (id, first, last) in hunk_output_ranges(hunks, resolutions) {
        let y0 = (first as f32 - 1.0) * lh;
        let y1 = (last as f32) * lh;
        if y1 > y0 {
            ranges.push((id, y0, y1));
        }
    }

    ResultPaneSync { ranges, view_h: widget_h }
}

/// Draw 1-based line numbers in the gutter strip, right-aligned with a
/// small padding. Mirrors `merge_view::paint_gutter` minus the per-row
/// hunk-tint background — result-pane tints already come from
/// `result_line_origins` painted in `paint_text`.
fn paint_line_number_gutter(
    ui: &Ui,
    g_rect: [f32; 4],
    scroll_y: f32,
    lh: f32,
    line_count: u32,
) {
    if lh <= 0.0 {
        return;
    }
    let dl = ui.get_window_draw_list();
    let g_left = g_rect[0];
    let g_top = g_rect[1];
    let g_right = g_rect[2];
    let g_bottom = g_rect[3];
    let padding_y = ui.clone_style().frame_padding[1];
    let first_line = (scroll_y / lh).floor() as u32 + 1;
    let last_line = ((scroll_y + (g_bottom - g_top)) / lh).ceil() as u32 + 1;
    dl.with_clip_rect_intersect([g_left, g_top], [g_right, g_bottom], || {
        for line in first_line..=last_line.min(line_count) {
            let y = g_top + padding_y + (line as f32 - 1.0) * lh - scroll_y;
            if y + lh < g_top || y > g_bottom {
                continue;
            }
            let text = format!("{line}");
            let text_w = ui.calc_text_size(&text)[0];
            dl.add_text(
                [g_right - 4.0 - text_w, y + 2.0],
                theme::OVERLAY1(),
                &text,
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Text painting.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint_text(
    ui: &Ui,
    widget_pos: [f32; 2],
    widget_w: f32,
    widget_h: f32,
    buf: &str,
    highlights: &[LineSpans],
    scroll_y: f32,
    lh: f32,
    origins: Option<&[LineOrigin]>,
) {
    let style = ui.clone_style();
    let padding_x = style.frame_padding[0];
    let padding_y = style.frame_padding[1];
    let widget_rect = [
        widget_pos[0],
        widget_pos[1],
        widget_pos[0] + widget_w,
        widget_pos[1] + widget_h,
    ];
    let widget_left = widget_rect[0];
    let widget_right = widget_rect[2];
    syntax_paint::paint_text_lines(
        ui,
        widget_rect,
        buf,
        highlights,
        0.0,
        scroll_y,
        padding_x,
        padding_y,
        lh,
        |dl, line_idx, _line_text, _ln, y0, y1| {
            let Some(origins) = origins else { return };
            let Some(origin) = origins.get(line_idx) else { return };
            let Some(color) = origin_row_bg(*origin) else { return };
            if y1 > y0 {
                dl.add_rect([widget_left, y0], [widget_right, y1], color)
                    .filled(true)
                    .build();
            }
        },
    );
}

/// Per-row background tint for each line origin. Mirrors the 2-way diff's
/// row-background convention (low-alpha fills) but uses the 3-way side
/// palette so the tint matches the gutter stripe and picker icons.
fn origin_row_bg(origin: LineOrigin) -> Option<[f32; 4]> {
    match origin {
        LineOrigin::Stable => None,
        LineOrigin::Local => Some([0.18, 0.50, 0.22, 0.30]),
        LineOrigin::Remote => Some([0.18, 0.30, 0.55, 0.30]),
        LineOrigin::Base => Some([0.55, 0.50, 0.18, 0.30]),
        LineOrigin::Custom => Some([0.40, 0.40, 0.40, 0.30]),
    }
}

// ---------------------------------------------------------------------------
// Origin-side stripe.
// ---------------------------------------------------------------------------

fn paint_origin_stripes(
    ui: &Ui,
    stripe_rect: [f32; 4],
    scroll_y: f32,
    lh: f32,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
) {
    if lh <= 0.0 {
        return;
    }
    let stripe_left = stripe_rect[0];
    let stripe_right = stripe_rect[2];
    let stripe_top = stripe_rect[1];
    let stripe_bottom = stripe_rect[3];
    let dl = ui.get_window_draw_list();
    let ranges = hunk_output_ranges(hunks, resolutions);
    let hunks_by_id: HashMap<u32, &MergeHunk> = hunks.iter().map(|h| (h.id(), h)).collect();
    dl.with_clip_rect(
        [stripe_left, stripe_top],
        [stripe_right, stripe_bottom],
        || {
            for (id, first, last) in ranges {
                let Some(hunk) = hunks_by_id.get(&id) else { continue };
                let Some(color) = stripe_color(hunk, resolutions.get(&id)) else { continue };
                let y0 = stripe_top + (first as f32 - 1.0) * lh - scroll_y;
                let y1 = stripe_top + (last as f32) * lh - scroll_y;
                if y1 < stripe_top || y0 > stripe_bottom {
                    continue;
                }
                let y0 = y0.max(stripe_top);
                let y1 = y1.min(stripe_bottom);
                dl.add_rect([stripe_left, y0], [stripe_right, y1], color)
                    .filled(true)
                    .build();
            }
        },
    );
}

/// Stripe color for a hunk's output region, given the current resolution.
/// Returns `None` for stable hunks (no stripe drawn).
pub(crate) fn stripe_color(hunk: &MergeHunk, resolution: Option<&Resolution>) -> Option<[f32; 4]> {
    match hunk {
        MergeHunk::Stable { .. } => None,
        MergeHunk::LocalOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::GREEN(),
        }),
        MergeHunk::RemoteOnly { .. } => Some(match resolution {
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
            _ => theme::SAPPHIRE(),
        }),
        MergeHunk::Conflict { .. } => Some(match resolution {
            None => theme::RED(),
            Some(Resolution::Local) => theme::GREEN(),
            Some(Resolution::Remote) => theme::SAPPHIRE(),
            Some(Resolution::Base) => theme::YELLOW(),
            Some(Resolution::Custom { .. }) => theme::OVERLAY1(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Picker gutter: icon shapes per resolution choice, filled = current pick.
// ---------------------------------------------------------------------------

/// One pickable icon: the shape, the color, the resolution it sets when
/// clicked, and whether the current resolution matches this icon (filled).
#[derive(Clone)]
struct PickerIcon {
    shape: IconShape,
    color: [f32; 4],
    resolution: Resolution,
    filled: bool,
}

#[derive(Clone, Copy)]
enum IconShape {
    Diamond, // Remote
    Square,  // Base
    Circle,  // Local
}

/// Compute the icon set for a hunk in its current resolution state. Stable
/// hunks return an empty slice (no pickers).
fn icons_for_hunk(hunk: &MergeHunk, resolution: Option<&Resolution>) -> Vec<PickerIcon> {
    let active = resolution_kind(resolution);
    match hunk {
        MergeHunk::Stable { .. } => vec![],
        MergeHunk::LocalOnly { .. } => vec![
            PickerIcon {
                shape: IconShape::Square,
                color: theme::YELLOW(),
                resolution: Resolution::Base,
                filled: active == Some(ResKind::Base),
            },
            PickerIcon {
                shape: IconShape::Circle,
                color: theme::GREEN(),
                resolution: Resolution::Local,
                // Default resolution for LocalOnly is Local — show filled
                // when the resolution map is empty.
                filled: active == Some(ResKind::Local) || active.is_none(),
            },
        ],
        MergeHunk::RemoteOnly { .. } => vec![
            PickerIcon {
                shape: IconShape::Diamond,
                color: theme::SAPPHIRE(),
                resolution: Resolution::Remote,
                filled: active == Some(ResKind::Remote) || active.is_none(),
            },
            PickerIcon {
                shape: IconShape::Square,
                color: theme::YELLOW(),
                resolution: Resolution::Base,
                filled: active == Some(ResKind::Base),
            },
        ],
        MergeHunk::Conflict { .. } => vec![
            PickerIcon {
                shape: IconShape::Diamond,
                color: theme::SAPPHIRE(),
                resolution: Resolution::Remote,
                filled: active == Some(ResKind::Remote),
            },
            PickerIcon {
                shape: IconShape::Square,
                color: theme::YELLOW(),
                resolution: Resolution::Base,
                filled: active == Some(ResKind::Base),
            },
            PickerIcon {
                shape: IconShape::Circle,
                color: theme::GREEN(),
                resolution: Resolution::Local,
                filled: active == Some(ResKind::Local),
            },
        ],
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResKind {
    Local,
    Remote,
    Base,
    Custom,
}

fn resolution_kind(r: Option<&Resolution>) -> Option<ResKind> {
    match r {
        Some(Resolution::Local) => Some(ResKind::Local),
        Some(Resolution::Remote) => Some(ResKind::Remote),
        Some(Resolution::Base) => Some(ResKind::Base),
        Some(Resolution::Custom { .. }) => Some(ResKind::Custom),
        None => None,
    }
}

/// Paint per-hunk picker icons and place invisible buttons over each.
/// Returns `Some((hunk_id, resolution))` if the user clicked an icon this frame.
fn paint_and_hit_pickers(
    ui: &Ui,
    gutter_rect: [f32; 4],
    scroll_y: f32,
    lh: f32,
    hunks: &[MergeHunk],
    resolutions: &HashMap<u32, Resolution>,
) -> Option<(u32, Resolution)> {
    if lh <= 0.0 {
        return None;
    }
    let g_left = gutter_rect[0];
    let g_right = gutter_rect[2];
    let g_top = gutter_rect[1];
    let g_bottom = gutter_rect[3];

    let ranges = hunk_output_ranges(hunks, resolutions);
    let hunks_by_id: HashMap<u32, &MergeHunk> = hunks.iter().map(|h| (h.id(), h)).collect();
    let style = ui.clone_style();
    let padding_y = style.frame_padding[1];

    let mut clicked: Option<(u32, Resolution)> = None;

    for (id, first, _last) in ranges {
        let Some(hunk) = hunks_by_id.get(&id) else { continue };
        let icons = icons_for_hunk(hunk, resolutions.get(&id));
        if icons.is_empty() {
            continue;
        }
        let y_center = g_top + padding_y + (first as f32 - 1.0) * lh + lh * 0.5 - scroll_y;
        if y_center + ICON_HALF < g_top || y_center - ICON_HALF > g_bottom {
            continue;
        }

        // Lay icons out right-to-left ending at the gutter's right edge so
        // they sit visually adjacent to the stripe.
        let row_w = (icons.len() as f32) * ICON_SPACING;
        let row_left = g_right - row_w - 2.0;
        let row_left = row_left.max(g_left);

        for (i, icon) in icons.iter().enumerate() {
            let cx = row_left + (i as f32) * ICON_SPACING + ICON_SPACING * 0.5;
            let cy = y_center;
            paint_icon(ui, [cx, cy], icon);

            // Hit-test via an invisible_button overlay.
            let btn_id = format!("##diffie_result_pick_{}_{}", id, i);
            let btn_pos = [cx - ICON_HALF - 2.0, cy - ICON_HALF - 2.0];
            ui.set_cursor_screen_pos(btn_pos);
            if ui.invisible_button(&btn_id, [ICON_HALF * 2.0 + 4.0, ICON_HALF * 2.0 + 4.0]) {
                clicked = Some((id, icon.resolution.clone()));
            }
        }
    }

    clicked
}

/// Draw a filled role icon for a 3-way side using the same shape + color as
/// the picker icons in the result-pane gutter. Used by the merge view's pane
/// header strip so each label visually matches its source in the result.
pub(crate) fn paint_role_icon(
    _ui: &Ui,
    center: [f32; 2],
    side: crate::session::ThreeWaySide,
    half: f32,
) {
    use crate::session::ThreeWaySide;
    let (shape, color) = match side {
        ThreeWaySide::Remote => (IconShape::Diamond, theme::SAPPHIRE()),
        ThreeWaySide::Base => (IconShape::Square, theme::YELLOW()),
        ThreeWaySide::Local => (IconShape::Circle, theme::GREEN()),
    };
    fill_shape(shape, center, half, color);
}

fn paint_icon(ui: &Ui, center: [f32; 2], icon: &PickerIcon) {
    let _ = ui; // unused; draw list is window-global
    if icon.filled {
        fill_shape(icon.shape, center, ICON_HALF, icon.color);
    } else {
        stroke_shape(icon.shape, center, ICON_HALF, icon.color, 1.5);
    }
}

fn fill_shape(shape: IconShape, center: [f32; 2], half: f32, color: [f32; 4]) {
    match shape {
        IconShape::Square => fill_convex_poly(
            &[
                [center[0] - half, center[1] - half],
                [center[0] + half, center[1] - half],
                [center[0] + half, center[1] + half],
                [center[0] - half, center[1] + half],
            ],
            color,
        ),
        IconShape::Diamond => fill_convex_poly(
            &[
                [center[0], center[1] - half],
                [center[0] + half, center[1]],
                [center[0], center[1] + half],
                [center[0] - half, center[1]],
            ],
            color,
        ),
        IconShape::Circle => unsafe {
            let dl = imgui::sys::igGetWindowDrawList();
            imgui::sys::ImDrawList_AddCircleFilled(
                dl,
                imgui::sys::ImVec2 { x: center[0], y: center[1] },
                half,
                pack_color(color),
                14,
            );
        },
    }
}

fn stroke_shape(shape: IconShape, center: [f32; 2], half: f32, color: [f32; 4], thickness: f32) {
    match shape {
        IconShape::Square => stroke_poly(
            &[
                [center[0] - half, center[1] - half],
                [center[0] + half, center[1] - half],
                [center[0] + half, center[1] + half],
                [center[0] - half, center[1] + half],
            ],
            color,
            thickness,
            true,
        ),
        IconShape::Diamond => stroke_poly(
            &[
                [center[0], center[1] - half],
                [center[0] + half, center[1]],
                [center[0], center[1] + half],
                [center[0] - half, center[1]],
            ],
            color,
            thickness,
            true,
        ),
        IconShape::Circle => unsafe {
            let dl = imgui::sys::igGetWindowDrawList();
            imgui::sys::ImDrawList_AddCircle(
                dl,
                imgui::sys::ImVec2 { x: center[0], y: center[1] },
                half,
                pack_color(color),
                14,
                thickness,
            );
        },
    }
}

fn fill_convex_poly(pts: &[[f32; 2]], color: [f32; 4]) {
    if pts.len() < 3 {
        return;
    }
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        for p in pts {
            imgui::sys::ImDrawList_PathLineTo(dl, imgui::sys::ImVec2 { x: p[0], y: p[1] });
        }
        imgui::sys::ImDrawList_PathFillConvex(dl, pack_color(color));
    }
}

fn stroke_poly(pts: &[[f32; 2]], color: [f32; 4], thickness: f32, closed: bool) {
    if pts.len() < 2 {
        return;
    }
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        for p in pts {
            imgui::sys::ImDrawList_PathLineTo(dl, imgui::sys::ImVec2 { x: p[0], y: p[1] });
        }
        let flags = if closed { imgui::sys::ImDrawFlags_Closed } else { imgui::sys::ImDrawFlags_None };
        imgui::sys::ImDrawList_PathStroke(dl, pack_color(color), flags as i32, thickness);
    }
}

fn pack_color(c: [f32; 4]) -> u32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to8(c[0]) | (to8(c[1]) << 8) | (to8(c[2]) << 16) | (to8(c[3]) << 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::MergeHunk;

    fn stable() -> MergeHunk {
        MergeHunk::Stable { id: 0, base: vec![], text: vec![] }
    }
    fn local_only() -> MergeHunk {
        MergeHunk::LocalOnly { id: 1, base: vec![], local: vec!["L".into()] }
    }
    fn remote_only() -> MergeHunk {
        MergeHunk::RemoteOnly { id: 2, base: vec![], remote: vec!["R".into()] }
    }
    fn conflict() -> MergeHunk {
        MergeHunk::Conflict {
            id: 3, base: vec![], local: vec![], remote: vec![],
        }
    }

    #[test]
    fn stable_has_no_stripe() {
        assert_eq!(stripe_color(&stable(), None), None);
    }

    #[test]
    fn local_only_defaults_to_green() {
        assert_eq!(stripe_color(&local_only(), None), Some(theme::GREEN()));
    }

    #[test]
    fn local_only_base_resolution_is_yellow() {
        assert_eq!(
            stripe_color(&local_only(), Some(&Resolution::Base)),
            Some(theme::YELLOW())
        );
    }

    #[test]
    fn remote_only_defaults_to_sapphire() {
        assert_eq!(stripe_color(&remote_only(), None), Some(theme::SAPPHIRE()));
    }

    #[test]
    fn conflict_unresolved_is_red() {
        assert_eq!(stripe_color(&conflict(), None), Some(theme::RED()));
    }

    #[test]
    fn conflict_resolutions_match_chosen_side() {
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Local)), Some(theme::GREEN()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Remote)), Some(theme::SAPPHIRE()));
        assert_eq!(stripe_color(&conflict(), Some(&Resolution::Base)), Some(theme::YELLOW()));
        assert_eq!(
            stripe_color(&conflict(), Some(&Resolution::Custom { text: vec![] })),
            Some(theme::OVERLAY1())
        );
    }

    #[test]
    fn stable_has_no_pickers() {
        assert!(icons_for_hunk(&stable(), None).is_empty());
    }

    #[test]
    fn local_only_pickers_default_local_filled() {
        let icons = icons_for_hunk(&local_only(), None);
        assert_eq!(icons.len(), 2);
        // Base outlined, Local filled (default).
        assert!(!icons[0].filled);
        assert!(icons[1].filled);
        assert!(matches!(icons[0].resolution, Resolution::Base));
        assert!(matches!(icons[1].resolution, Resolution::Local));
    }

    #[test]
    fn local_only_base_pick_flips_filled() {
        let icons = icons_for_hunk(&local_only(), Some(&Resolution::Base));
        assert!(icons[0].filled);
        assert!(!icons[1].filled);
    }

    #[test]
    fn remote_only_pickers_default_remote_filled() {
        let icons = icons_for_hunk(&remote_only(), None);
        assert_eq!(icons.len(), 2);
        assert!(icons[0].filled);  // Remote (sapphire diamond)
        assert!(!icons[1].filled); // Base (yellow square)
        assert!(matches!(icons[0].resolution, Resolution::Remote));
        assert!(matches!(icons[1].resolution, Resolution::Base));
    }

    #[test]
    fn conflict_pickers_unresolved_all_outlined() {
        let icons = icons_for_hunk(&conflict(), None);
        assert_eq!(icons.len(), 3);
        for icon in &icons {
            assert!(!icon.filled, "expected outlined for unresolved conflict");
        }
    }

    #[test]
    fn conflict_picks_match_filled_icon() {
        let icons = icons_for_hunk(&conflict(), Some(&Resolution::Remote));
        assert!(icons[0].filled);
        assert!(!icons[1].filled);
        assert!(!icons[2].filled);

        let icons = icons_for_hunk(&conflict(), Some(&Resolution::Base));
        assert!(!icons[0].filled);
        assert!(icons[1].filled);
        assert!(!icons[2].filled);

        let icons = icons_for_hunk(&conflict(), Some(&Resolution::Local));
        assert!(!icons[0].filled);
        assert!(!icons[1].filled);
        assert!(icons[2].filled);
    }

    #[test]
    fn custom_resolution_leaves_all_outlined() {
        let icons = icons_for_hunk(&conflict(), Some(&Resolution::Custom { text: vec![] }));
        for icon in &icons {
            assert!(!icon.filled);
        }
        let icons = icons_for_hunk(&local_only(), Some(&Resolution::Custom { text: vec![] }));
        for icon in &icons {
            assert!(!icon.filled);
        }
    }
}
