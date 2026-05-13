//! Per-tab toolbar for choosing the diff engine and `DiffOptions`
//! (whitespace handling, sub-line granularity, move detection).

use crate::diff::{
    available_engines, DiffOptions, EngineCapabilities, SubLineGranularity, Whitespace,
};
use crate::session::{engine_capabilities, SessionId, SessionStore};

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
    current_engine: &str,
    current_options: DiffOptions,
    status: &mut String,
) {
    let engines: Vec<(String, EngineCapabilities)> = available_engines();
    let engine_names: Vec<String> = engines.iter().map(|(n, _)| n.clone()).collect();
    let mut engine_idx = engines
        .iter()
        .position(|(n, _)| n == current_engine)
        .unwrap_or(0);

    ui.text("Engine:");
    ui.same_line();
    ui.set_next_item_width(120.0);
    if ui.combo_simple_string("##engine", &mut engine_idx, &engine_names) {
        if let Some((new_name, _)) = engines.get(engine_idx) {
            if new_name != current_engine {
                match store.set_engine(session_id, new_name.clone()) {
                    Ok(()) => *status = format!("engine: {new_name}"),
                    Err(e) => *status = format!("engine error: {e}"),
                }
            }
        }
    }

    ui.same_line();
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

    ui.same_line();
    let caps = engine_capabilities(current_engine).unwrap_or_default();
    let mut moves = current_options.detect_moves;
    let _enabled = caps.supports_moves;
    // imgui's checkbox doesn't have a disabled style in this binding; we
    // gate the toggle by ignoring user clicks when the engine can't do
    // moves, and grey the label.
    if caps.supports_moves {
        if ui.checkbox("Detect moves", &mut moves) {
            let mut opts = current_options;
            opts.detect_moves = moves;
            apply_options(store, session_id, opts, status);
        }
    } else {
        let mut dummy = false;
        let token = ui.begin_disabled(true);
        ui.checkbox("Detect moves", &mut dummy);
        drop(token);
        if ui.is_item_hovered() {
            ui.tooltip_text("This engine does not support move detection.");
        }
    }
}

fn apply_options(store: &SessionStore, id: SessionId, opts: DiffOptions, status: &mut String) {
    match store.set_options(id, opts) {
        Ok(()) => *status = "diff options updated".to_string(),
        Err(e) => *status = format!("options error: {e}"),
    }
}
