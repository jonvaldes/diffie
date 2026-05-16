//! P4Merge-style relationship strip rendered at the top of 3-way tabs.
//!
//! Owns the small diagram + counts + color legend. Filename inputs continue
//! to live in `pane_header_bar`; this strip only adds the visual relationship
//! summary and per-side color key.

use imgui::Ui;

use crate::app::theme;
use crate::merge::MergeHunk;

/// Per-classification hunk counts surfaced in the header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeCounts {
    pub local_changes: u32,
    pub remote_changes: u32,
    pub conflicts: u32,
}

/// Tally hunks by classification. Stable hunks do not contribute.
pub fn count_hunks(hunks: &[MergeHunk]) -> MergeCounts {
    let mut c = MergeCounts::default();
    for h in hunks {
        match h {
            MergeHunk::Stable { .. } => {}
            MergeHunk::LocalOnly { .. } => c.local_changes += 1,
            MergeHunk::RemoteOnly { .. } => c.remote_changes += 1,
            MergeHunk::Conflict { .. } => c.conflicts += 1,
        }
    }
    c
}

// ---------------------------------------------------------------------------
// Diagram constants (relative to a 80x60 px canvas).
// ---------------------------------------------------------------------------

const CANVAS_W: f32 = 80.0;
const CANVAS_H: f32 = 60.0;
const STRIP_H: f32 = 72.0;
const MARKER_HALF: f32 = 6.0;

const BASE_POS: [f32; 2] = [40.0, 8.0];    // top center
const REMOTE_POS: [f32; 2] = [12.0, 30.0]; // left
const LOCAL_POS: [f32; 2] = [68.0, 30.0];  // right
const MERGE_POS: [f32; 2] = [40.0, 52.0];  // bottom center

// ---------------------------------------------------------------------------
// Render entry point.
// ---------------------------------------------------------------------------

/// Paint the diagram, counts, and legend at the current cursor. Advances the
/// cursor past the strip so subsequent widgets land below it.
pub fn render(ui: &Ui, counts: MergeCounts) {
    let origin = ui.cursor_screen_pos();
    let avail_w = ui.content_region_avail()[0];

    draw_diagram(ui, origin);
    draw_counts(ui, [origin[0] + CANVAS_W + 16.0, origin[1] + 6.0], counts);
    draw_legend(ui, [origin[0] + avail_w - legend_width(ui), origin[1] + 6.0]);

    ui.set_cursor_screen_pos([origin[0], origin[1] + STRIP_H]);
}

fn draw_diagram(ui: &Ui, origin: [f32; 2]) {
    let p = |off: [f32; 2]| [origin[0] + off[0], origin[1] + off[1]];

    let stroke = theme::OVERLAY1();
    let thickness = 1.5;

    // Curves: Base <-> Remote, Base <-> Local, Remote <-> Merge, Local <-> Merge.
    stroke_curve(p(BASE_POS), p(REMOTE_POS), stroke, thickness);
    stroke_curve(p(BASE_POS), p(LOCAL_POS), stroke, thickness);
    stroke_curve(p(REMOTE_POS), p(MERGE_POS), stroke, thickness);
    stroke_curve(p(LOCAL_POS), p(MERGE_POS), stroke, thickness);

    // Markers: Base = yellow square, Remote = sapphire diamond,
    // Local = green circle, Merge = overlay1 diamond.
    fill_square(p(BASE_POS), MARKER_HALF, theme::YELLOW());
    fill_diamond(p(REMOTE_POS), MARKER_HALF, theme::SAPPHIRE());
    fill_circle(p(LOCAL_POS), MARKER_HALF, theme::GREEN());
    fill_diamond(p(MERGE_POS), MARKER_HALF, theme::OVERLAY1());

    // Suppress unused-variable warning for `ui` — we keep the same signature
    // as `draw_counts`/`draw_legend` for consistency; `ui` may be used for
    // clipping in future iterations.
    let _ = ui;
}

fn draw_counts(ui: &Ui, top_left: [f32; 2], counts: MergeCounts) {
    let dl = ui.get_window_draw_list();
    let lh = ui.text_line_height();

    let rows: [(u8, &str, u32, [f32; 4]); 3] = [
        (0, "Remote changes:", counts.remote_changes, theme::SAPPHIRE()),
        (1, "Local changes:",  counts.local_changes,  theme::GREEN()),
        (2, "Conflicts:",      counts.conflicts,      conflict_count_color(counts.conflicts)),
    ];

    for (i, label, val, color) in rows {
        let y = top_left[1] + (i as f32) * (lh + 2.0);
        // Marker square.
        let sx = top_left[0];
        let sz = lh - 4.0;
        dl.add_rect([sx, y + 2.0], [sx + sz, y + 2.0 + sz], color)
            .filled(true)
            .build();
        // Label + value text.
        let tx = sx + sz + 6.0;
        dl.add_text([tx, y], theme::TEXT(), &format!("{label} {val}"));
    }
}

fn conflict_count_color(n: u32) -> [f32; 4] {
    if n > 0 { theme::RED() } else { theme::OVERLAY1() }
}

const LEGEND_ENTRIES: [(&str, fn() -> [f32; 4]); 4] = [
    ("Remote", theme::SAPPHIRE),
    ("Base",   theme::YELLOW),
    ("Local",  theme::GREEN),
    ("Merge",  theme::OVERLAY1),
];

fn legend_width(ui: &Ui) -> f32 {
    let mut w = 0.0_f32;
    for (label, _) in LEGEND_ENTRIES {
        w += ui.calc_text_size(label)[0] + 24.0;
    }
    w
}

fn draw_legend(ui: &Ui, top_left: [f32; 2]) {
    let dl = ui.get_window_draw_list();
    let lh = ui.text_line_height();
    let mut x = top_left[0];
    let y = top_left[1];
    for (label, color_fn) in LEGEND_ENTRIES {
        let sz = lh - 4.0;
        dl.add_rect([x, y + 2.0], [x + sz, y + 2.0 + sz], color_fn())
            .filled(true)
            .build();
        let tx = x + sz + 4.0;
        dl.add_text([tx, y], theme::TEXT(), label);
        x = tx + ui.calc_text_size(label)[0] + 12.0;
    }
}

// ---------------------------------------------------------------------------
// Primitive painters — use unsafe imgui sys calls because this version of
// imgui-rs does not expose add_bezier_curve / add_circle(filled) / add_polyline
// at the safe API level. Patterns mirror stroke_bezier_curve and fill_polygon
// in merge_view.rs.
// ---------------------------------------------------------------------------

fn pack_color(c: [f32; 4]) -> u32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to8(c[0]) | (to8(c[1]) << 8) | (to8(c[2]) << 16) | (to8(c[3]) << 24)
}

fn iv2(x: f32, y: f32) -> imgui::sys::ImVec2 {
    imgui::sys::ImVec2 { x, y }
}

fn stroke_curve(a: [f32; 2], b: [f32; 2], color: [f32; 4], thickness: f32) {
    // Cubic bezier with vertically-offset control points so the curve bows softly.
    let mid_y = (a[1] + b[1]) * 0.5;
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        imgui::sys::ImDrawList_PathLineTo(dl, iv2(a[0], a[1]));
        imgui::sys::ImDrawList_PathBezierCubicCurveTo(
            dl,
            iv2(a[0], mid_y),
            iv2(b[0], mid_y),
            iv2(b[0], b[1]),
            0,
        );
        imgui::sys::ImDrawList_PathStroke(dl, col, imgui::sys::ImDrawFlags_None as i32, thickness);
    }
}

fn fill_square(center: [f32; 2], half: f32, color: [f32; 4]) {
    let pts = [
        [center[0] - half, center[1] - half],
        [center[0] + half, center[1] - half],
        [center[0] + half, center[1] + half],
        [center[0] - half, center[1] + half],
    ];
    fill_convex_poly(&pts, color);
}

fn fill_circle(center: [f32; 2], radius: f32, color: [f32; 4]) {
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        // ImDrawList_AddCircleFilled: center, radius, col, num_segments
        imgui::sys::ImDrawList_AddCircleFilled(
            dl,
            iv2(center[0], center[1]),
            radius,
            col,
            12,
        );
    }
}

fn fill_diamond(center: [f32; 2], half: f32, color: [f32; 4]) {
    let pts = [
        [center[0], center[1] - half],
        [center[0] + half, center[1]],
        [center[0], center[1] + half],
        [center[0] - half, center[1]],
    ];
    fill_convex_poly(&pts, color);
}

/// Fill a convex polygon using imgui's PathFillConvex, which avoids the
/// earcutr dependency and works for all convex shapes.
fn fill_convex_poly(pts: &[[f32; 2]], color: [f32; 4]) {
    if pts.len() < 3 {
        return;
    }
    let col = pack_color(color);
    unsafe {
        let dl = imgui::sys::igGetWindowDrawList();
        imgui::sys::ImDrawList_PathClear(dl);
        for p in pts {
            imgui::sys::ImDrawList_PathLineTo(dl, iv2(p[0], p[1]));
        }
        imgui::sys::ImDrawList_PathFillConvex(dl, col);
    }
}

// ---------------------------------------------------------------------------
// Suppress dead-code warnings for CANVAS_H — it will be used when the strip
// gains a background fill in Task 4.
// ---------------------------------------------------------------------------
const _: f32 = CANVAS_H;

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_hunks_empty_is_zero() {
        let c = count_hunks(&[]);
        assert_eq!(c, MergeCounts::default());
    }

    #[test]
    fn count_hunks_stable_does_not_count() {
        let hunks = vec![MergeHunk::Stable {
            id: 0,
            base: vec!["x".into()],
            text: vec!["x".into()],
        }];
        let c = count_hunks(&hunks);
        assert_eq!(c, MergeCounts::default());
    }

    #[test]
    fn count_hunks_categorizes_mixed() {
        let hunks = vec![
            MergeHunk::Stable { id: 0, base: vec![], text: vec![] },
            MergeHunk::LocalOnly { id: 1, base: vec![], local: vec!["L".into()] },
            MergeHunk::LocalOnly { id: 2, base: vec![], local: vec!["L".into()] },
            MergeHunk::RemoteOnly { id: 3, base: vec![], remote: vec!["R".into()] },
            MergeHunk::Conflict {
                id: 4, base: vec![], local: vec![], remote: vec![],
            },
        ];
        let c = count_hunks(&hunks);
        assert_eq!(c.local_changes, 2);
        assert_eq!(c.remote_changes, 1);
        assert_eq!(c.conflicts, 1);
    }

    #[test]
    fn conflict_count_color_red_when_positive() {
        assert_eq!(conflict_count_color(0), theme::OVERLAY1());
        assert_eq!(conflict_count_color(3), theme::RED());
    }
}
