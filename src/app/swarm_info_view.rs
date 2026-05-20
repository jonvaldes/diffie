//! Read-only info tab for a Swarm review/changelist. Displays metadata,
//! a progress bar while files are still loading, the file list (clickable
//! to switch to that file's tab), reviewers/votes (reviews only), and an
//! "Open in browser" button.

use imgui::Ui;

use crate::swarm::model::{ReviewMeta, TargetKind};

pub struct InfoContext<'a> {
    pub meta: &'a ReviewMeta,
    pub progress: Option<(usize, usize)>,
    /// File-list rows the user can click to jump to that file's tab.
    pub file_rows: &'a [InfoFileRow],
    /// Index in `file_rows` the user clicked, if any.
    pub click_index: &'a mut Option<usize>,
    pub open_in_browser: &'a mut bool,
}

pub struct InfoFileRow {
    pub depot_path: String,
    pub action_label: String,
    /// `None` if the tab hasn't been created yet (still loading).
    pub session_id: Option<crate::session::SessionId>,
}

pub fn render(ui: &Ui, ctx: InfoContext<'_>) {
    let title = match ctx.meta.kind {
        TargetKind::Review => format!("Review #{}", ctx.meta.id),
        TargetKind::Change => format!("Change #{}", ctx.meta.id),
    };
    ui.text(&title);
    ui.same_line();
    ui.text_disabled(format!("  {}  ", ctx.meta.state));
    ui.same_line();
    ui.text(format!("by {}", ctx.meta.author));
    ui.same_line();
    if ui.button("Open in browser") { *ctx.open_in_browser = true; }

    ui.separator();
    if let Some((done, total)) = ctx.progress {
        if done < total {
            imgui::ProgressBar::new(done as f32 / total.max(1) as f32)
                .overlay_text(format!("Loaded {done} / {total}"))
                .build(ui);
        } else {
            ui.text_disabled(format!("Loaded {total} files"));
        }
        ui.separator();
    }

    ui.text("Description:");
    let mut desc = ctx.meta.description.clone();
    ui.input_text_multiline("##desc", &mut desc, [-1.0, 120.0])
        .read_only(true)
        .build();

    if matches!(ctx.meta.kind, TargetKind::Review) && !ctx.meta.participants.is_empty() {
        ui.separator();
        ui.text("Reviewers:");
        for p in &ctx.meta.participants {
            let v = match p.vote.signum() {
                1 => "+1",
                -1 => "-1",
                _ => " 0",
            };
            ui.text(format!("  {v}  {}", p.user));
        }
    }

    ui.separator();
    ui.text("Files:");
    if let Some(_t) = ui.begin_table("swarm_files", 3) {
        ui.table_setup_column("Action");
        ui.table_setup_column("Path");
        ui.table_setup_column("");
        ui.table_headers_row();
        for (i, row) in ctx.file_rows.iter().enumerate() {
            ui.table_next_row();
            ui.table_next_column(); ui.text(&row.action_label);
            ui.table_next_column();
            let label = format!("{}##file{i}", row.depot_path);
            if row.session_id.is_some() {
                if ui.selectable(label) { *ctx.click_index = Some(i); }
            } else {
                ui.text_disabled(&row.depot_path);
            }
            ui.table_next_column();
            if row.session_id.is_none() { ui.text_disabled("loading…"); }
        }
    }
}
