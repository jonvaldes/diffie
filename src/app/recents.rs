//! "Recents" persistence — the File > Recents submenu remembers comparisons
//! across runs by writing a JSON file to the platform's config directory.
//!
//! Linux:   ~/.config/diffie/recents.json     (or $XDG_CONFIG_HOME/...)
//! macOS:   ~/Library/Application Support/diffie/recents.json
//! Windows: %APPDATA%\diffie\recents.json
//!
//! The file is small and writes are best-effort: failures to read/write are
//! silently ignored. Worst case the user just doesn't have a recents list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const APP_DIRNAME: &str = "diffie";
const FILENAME: &str = "recents.json";
const MAX_RECENTS: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RecentEntry {
    TwoWay { a: PathBuf, b: PathBuf },
    ThreeWay { base: PathBuf, local: PathBuf, remote: PathBuf },
}

impl RecentEntry {
    pub fn label(&self) -> String {
        match self {
            RecentEntry::TwoWay { a, b } => {
                format!("{} ↔ {}", basename(a), basename(b))
            }
            RecentEntry::ThreeWay { base, local, remote } => {
                format!(
                    "{} (3-way: {} ↔ {})",
                    basename(base),
                    basename(local),
                    basename(remote)
                )
            }
        }
    }
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn recents_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_DIRNAME).join(FILENAME))
}

pub fn load() -> Vec<RecentEntry> {
    let Some(p) = recents_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(entries: &[RecentEntry]) {
    let Some(p) = recents_path() else {
        return;
    };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(&p, text);
    }
}

/// Move-to-front: remove any existing copy of `entry`, prepend it, and cap
/// the list at MAX_RECENTS. Persists to disk afterward.
pub fn add(entries: &mut Vec<RecentEntry>, entry: RecentEntry) {
    entries.retain(|e| e != &entry);
    entries.insert(0, entry);
    if entries.len() > MAX_RECENTS {
        entries.truncate(MAX_RECENTS);
    }
    save(entries);
}
