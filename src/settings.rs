//! User preferences persisted to ~/.config/portagehub/settings.json.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// File name used inside package.* directories for entries PortageHub writes.
    pub target_file: String,
    /// Show advanced/expert options everywhere.
    pub show_advanced: bool,
    /// Show the confirmation dialog with a diff before writing config files.
    pub review_writes: bool,
    /// Keep a `~` backup of every file before it is replaced.
    pub backup_on_write: bool,
    /// Run previews as root (more accurate for some setups, but asks for the password).
    pub pretend_as_root: bool,
    /// Re-run the preview automatically after applying suggested fixes.
    pub auto_repretend: bool,
    pub ui_scale: f32,
    pub font_family: Option<String>,
    /// Show plain-language explanations and tips.
    pub beginner_tips: bool,
    pub log_wrap: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            target_file: "zz-portagehub".into(),
            show_advanced: false,
            review_writes: true,
            backup_on_write: false,
            pretend_as_root: false,
            auto_repretend: true,
            ui_scale: 1.0,
            font_family: None,
            beginner_tips: true,
            log_wrap: false,
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("portagehub").join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, t);
        }
    }
}
