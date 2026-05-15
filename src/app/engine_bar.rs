//! Per-tab toolbar for tweaking the live `DiffOptions`. Currently only
//! the Whitespace mode is exposed here — engine, move detection, and
//! sub-line granularity are tab-stable choices that live in the
//! Preferences modal as defaults for new tabs.

use crate::app::theme;
use crate::diff::{DiffOptions, Whitespace};
use crate::session::{SessionId, SessionStore};

/// (label, tooltip glyph, mode). The glyph is a Font-Awesome Nerd-Font
/// codepoint; tooltip text is the human label.
struct WsMode {
    label: &'static str,
    icon: &'static str,
    mode: Whitespace,
}

const WHITESPACE_MODES: &[WsMode] = &[
    // nf-fa-paragraph — pilcrow, "treat whitespace literally"
    WsMode { label: "Significant", icon: "\u{f1dd}", mode: Whitespace::None },
    // nf-fa-eye_slash — "ignore all whitespace"
    WsMode { label: "Ignore all", icon: "\u{f070}", mode: Whitespace::IgnoreAll },
    // nf-fa-align_left — left-aligned text, "ignore leading"
    WsMode { label: "Ignore leading", icon: "\u{f036}", mode: Whitespace::IgnoreLeading },
    // nf-fa-align_right — right-aligned text, "ignore trailing + EOL"
    WsMode { label: "Ignore trailing+EOL", icon: "\u{f038}", mode: Whitespace::IgnoreTrailingEol },
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

    // Make each whitespace button twice as wide as its natural (icon +
    // frame-padding) width so the row reads as a clear toggle bar.
    let style = ui.clone_style();
    let pad_x = style.frame_padding[0];
    let frame_h = ui.frame_height();
    let widest_icon = WHITESPACE_MODES
        .iter()
        .map(|m| ui.calc_text_size(m.icon)[0])
        .fold(0.0_f32, f32::max);
    let btn_w = (widest_icon + pad_x * 2.0) * 2.0;

    let mut clicked: Option<Whitespace> = None;
    for (i, m) in WHITESPACE_MODES.iter().enumerate() {
        if i > 0 {
            ui.same_line_with_spacing(0.0, 4.0);
        }
        let active = current_options.whitespace == m.mode;
        // Tint the button when it's the active mode so the current
        // selection reads at a glance, matching the active-tab fill.
        let _tint = active.then(|| {
            (
                ui.push_style_color(imgui::StyleColor::Button, theme::SURFACE1()),
                ui.push_style_color(imgui::StyleColor::ButtonHovered, theme::SURFACE2()),
                ui.push_style_color(imgui::StyleColor::ButtonActive, theme::OVERLAY0()),
            )
        });
        if ui.button_with_size(format!("{}##ws_{}", m.icon, i), [btn_w, frame_h]) {
            clicked = Some(m.mode);
        }
        if ui.is_item_hovered() {
            ui.tooltip_text(m.label);
        }
    }

    if let Some(new_ws) = clicked {
        if new_ws != current_options.whitespace {
            let mut opts = current_options;
            opts.whitespace = new_ws;
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
