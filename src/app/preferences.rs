//! App-level preferences (default engine + default diff options) persisted
//! to `settings.json` in the platform's config dir.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::theme::Flavor;
use crate::diff::DiffOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppPreferences {
    pub default_engine: String,
    pub default_options: DiffOptions,
    #[serde(default)]
    pub theme: Flavor,
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
