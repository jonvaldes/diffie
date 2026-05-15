//! Per-tab toolbar for tweaking the live `DiffOptions` that affect what
//! lines actually compare equal (whitespace handling, sub-line
//! granularity). Engine selection and move-detection live in the
//! Preferences modal as defaults for new tabs — they're tab-stable
//! settings, not per-frame knobs.

use crate::diff::{DiffOptions, SubLineGranularity, Whitespace};
use crate::session::{SessionId, SessionStore};

const WHITESPACE_OPTIONS: &[(&str, Whitespace)] = &[
    ("Significant", Whitespace::None),
    ("Ignore all", Whitespace::IgnoreAll),
    ("Ignore leading", Whitespace::IgnoreLeading),
    ("Ignore trailing+EOL", Whitespace::IgnoreTrailingEol),
];

const GRANULARITY_OPTIONS: &[(&str, SubLineGranularity)] = &[
    ("None", SubLineGranularity::None),
    ("Word", SubLineGranularity::Word),
    ("Char", SubLineGranularity::Char),
    ("Grapheme", SubLineGranularity::Grapheme),
];

pub fn render(
    ui: &imgui::Ui,
    store: &SessionStore,
    session_id: SessionId,
    _current_engine: &str,
    current_options: DiffOptions,
    status: &mut String,
) {
    ui.text("Whitespace:");
    ui.same_line();
    ui.set_next_item_width(150.0);
    let mut ws_idx = WHITESPACE_OPTIONS
        .iter()
        .position(|(_, v)| *v == current_options.whitespace)
        .unwrap_or(0);
    let ws_labels: Vec<&str> = WHITESPACE_OPTIONS.iter().map(|(l, _)| *l).collect();
    if ui.combo_simple_string("##whitespace", &mut ws_idx, &ws_labels) {
        let new_ws = WHITESPACE_OPTIONS[ws_idx].1;
        if new_ws != current_options.whitespace {
            let mut opts = current_options;
            opts.whitespace = new_ws;
            apply_options(store, session_id, opts, status);
        }
    }

    ui.same_line();
    ui.text("Sub-line:");
    ui.same_line();
    ui.set_next_item_width(110.0);
    let mut g_idx = GRANULARITY_OPTIONS
        .iter()
        .position(|(_, v)| *v == current_options.sub_line)
        .unwrap_or(0);
    let g_labels: Vec<&str> = GRANULARITY_OPTIONS.iter().map(|(l, _)| *l).collect();
    if ui.combo_simple_string("##sub_line", &mut g_idx, &g_labels) {
        let new_g = GRANULARITY_OPTIONS[g_idx].1;
        if new_g != current_options.sub_line {
            let mut opts = current_options;
            opts.sub_line = new_g;
            apply_options(store, session_id, opts, status);
        }
    }
}

fn apply_options(store: &SessionStore, id: SessionId, opts: DiffOptions, status: &mut String) {
    match store.set_options(id, opts) {
        Ok(()) => *status = "diff options updated".to_string(),
        Err(e) => *status = format!("options error: {e}"),
    }
}
