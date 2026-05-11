//! 2-way diff view.
//!
//! Two side-by-side child windows with virtualized row rendering. Step 4
//! deliberately omits the connector ribbons, char-level highlights, scroll
//! sync, and the per-hunk control buttons — each lands in a later step.

use imgui::{ListClipper, StyleColor, Ui};

use crate::diff::{DiffOp, Hunk};

/// Uniform row height in screen pixels. Chosen to comfortably fit the default
/// imgui font; revisit when we add font scaling.
pub const ROW_H: f32 = 18.0;

/// 60px wide strip reserved between the two panes for the connector (step 6).
const CONNECTOR_W: f32 = 60.0;

#[derive(Clone, Copy)]
enum Cls {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone)]
struct Row {
    line_no: Option<u32>,
    text: String,
    cls: Cls,
}

fn build_rows(hunks: &[Hunk]) -> (Vec<Row>, Vec<Row>) {
    let mut left: Vec<Row> = Vec::new();
    let mut right: Vec<Row> = Vec::new();
    for h in hunks {
        for op in &h.ops {
            match op {
                DiffOp::Equal { a, b, text } => {
                    left.push(Row { line_no: Some(*a), text: text.clone(), cls: Cls::Equal });
                    right.push(Row { line_no: Some(*b), text: text.clone(), cls: Cls::Equal });
                }
                DiffOp::Delete { a, text } => {
                    left.push(Row { line_no: Some(*a), text: text.clone(), cls: Cls::Delete });
                }
                DiffOp::Insert { b, text } => {
                    right.push(Row { line_no: Some(*b), text: text.clone(), cls: Cls::Insert });
                }
            }
        }
    }
    (left, right)
}

pub fn render(ui: &Ui, _a_lines: &[String], _b_lines: &[String], hunks: &[Hunk]) {
    let (left, right) = build_rows(hunks);
    let avail = ui.content_region_avail();
    let pane_w = ((avail[0] - CONNECTOR_W) * 0.5).max(80.0);

    ui.child_window("diffie_left")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| draw_pane(ui, &left));

    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_connector")
        .size([CONNECTOR_W, avail[1]])
        .border(true)
        .build(|| {
            // Reserved for bezier ribbons + anchor lines in step 6.
        });

    ui.same_line_with_spacing(0.0, 0.0);

    ui.child_window("diffie_right")
        .size([pane_w, avail[1]])
        .border(true)
        .build(|| draw_pane(ui, &right));
}

fn draw_pane(ui: &Ui, rows: &[Row]) {
    let total = rows.len() as i32;
    if total == 0 {
        return;
    }
    let mut clipper = ListClipper::new(total).items_height(ROW_H).begin(ui);
    while clipper.step() {
        for i in clipper.display_start()..clipper.display_end() {
            draw_row(ui, &rows[i as usize]);
        }
    }
}

fn draw_row(ui: &Ui, row: &Row) {
    // Background rect for delete/insert.
    let (bg, fg) = match row.cls {
        Cls::Equal => (None, None),
        Cls::Delete => (Some([0.55, 0.18, 0.18, 0.30]), Some([1.0, 0.65, 0.62, 1.0])),
        Cls::Insert => (Some([0.18, 0.50, 0.22, 0.30]), Some([0.72, 1.0, 0.78, 1.0])),
    };
    if let Some(bg_rgba) = bg {
        let cursor = ui.cursor_screen_pos();
        let dl = ui.get_window_draw_list();
        let row_w = ui.content_region_avail()[0];
        dl.add_rect(
            [cursor[0], cursor[1]],
            [cursor[0] + row_w, cursor[1] + ROW_H],
            bg_rgba,
        )
        .filled(true)
        .build();
    }
    let line_text = match row.line_no {
        Some(n) => format!("{n:>4} "),
        None => "     ".to_string(),
    };
    let _line_no_style = ui.push_style_color(StyleColor::Text, [0.55, 0.60, 0.70, 1.0]);
    ui.text(&line_text);
    drop(_line_no_style);
    ui.same_line_with_spacing(0.0, 0.0);
    if let Some(fg_rgba) = fg {
        ui.text_colored(fg_rgba, &row.text);
    } else {
        ui.text(&row.text);
    }
}
