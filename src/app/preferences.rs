//! App-level preferences (default engine + default diff options) persisted
//! to `settings.json` in the platform's config dir.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::fonts::CodeFont;
use crate::app::theme::Flavor;
use crate::diff::DiffOptions;

/// Default UI font size in pixels (pre-DPI scale). Applied to menus,
/// buttons, headers, status bar — anything in the chrome rendered with
/// the proportional font. Code panes scale off this via
/// `CODE_FONT_BASE_SCALE`, so changing it adjusts both axes proportionally.
pub const DEFAULT_UI_FONT_SIZE: f32 = 13.0;
pub const MIN_UI_FONT_SIZE: f32 = 9.0;
pub const MAX_UI_FONT_SIZE: f32 = 24.0;

fn default_ui_font_size() -> f32 {
    DEFAULT_UI_FONT_SIZE
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppPreferences {
    pub default_engine: String,
    pub default_options: DiffOptions,
    #[serde(default)]
    pub theme: Flavor,
    #[serde(default)]
    pub show_whitespace: bool,
    #[serde(default)]
    pub code_font: CodeFont,
    #[serde(default)]
    pub window: WindowPlacement,
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
}

/// Last known window geometry, restored at startup. All fields are
/// optional so a fresh install (or a settings file written by an older
/// build) falls back to the hard-coded initial size and the
/// OS-default position.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub maximized: bool,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            // Histogram is the only registered engine that supports move
            // detection, and detect_moves is on by default. Pick it
            // regardless of registry order so the default experience
            // surfaces moves out of the box.
            default_engine: "histogram".to_string(),
            default_options: DiffOptions::default(),
            theme: Flavor::default(),
            show_whitespace: false,
            code_font: CodeFont::default(),
            window: WindowPlacement::default(),
            ui_font_size: DEFAULT_UI_FONT_SIZE,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("diffie");
    Some(p.join("settings.json"))
}

pub fn load() -> AppPreferences {
    let Some(path) = settings_path() else {
        return AppPreferences::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AppPreferences::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(prefs: &AppPreferences) -> std::io::Result<()> {
    let Some(path) = settings_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(prefs).unwrap_or_default();
    std::fs::write(path, text)
}
