use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Complete persistent configuration struct mapped to settings.json
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub consider_empty_files_empty: bool,
    pub ignore_hidden: bool,
    pub ignore_errors: bool,
    pub hide_search_errors: bool,
    pub skip_system: bool,
    pub delete_mode: i32,
    pub max_depth: i32,
    pub pause_ms: i32,
    pub min_age_hours: i32,
    pub auto_save_logs: bool,
    pub mft_scan: bool,
    pub ignore_list_text: String,
    pub ignore_files_text: String,
    pub dry_run: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            consider_empty_files_empty: true,
            ignore_hidden: true,
            ignore_errors: true,
            hide_search_errors: true,
            skip_system: true,
            delete_mode: 0,
            max_depth: -1,
            pause_ms: 0,
            min_age_hours: 0,
            auto_save_logs: false,
            mft_scan: false,
            ignore_list_text: get_default_ignore_dirs(),
            ignore_files_text: "desktop.ini\nThumbs.db\n.DS_Store".to_string(),
            dry_run: false,
        }
    }
}

/// Generates platform-specific default ignore directories dynamically
pub fn get_default_ignore_dirs() -> String {
    let mut ignore_dirs = vec![
        "System Volume Information".to_string(),
        "RECYCLER".to_string(),
        "Recycled".to_string(),
        "NtUninstall".to_string(),
        "$RECYCLE.BIN".to_string(),
        "GAC_MSIL".to_string(),
        "GAC_32".to_string(),
        "winsxs".to_string(),
        "System32".to_string(),
    ];

    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        ignore_dirs.push(
            PathBuf::from(local_appdata)
                .join("Packages")
                .to_string_lossy()
                .into_owned(),
        );
    } else if let Ok(user_profile) = std::env::var("USERPROFILE") {
        ignore_dirs.push(
            PathBuf::from(user_profile)
                .join("AppData")
                .join("Local")
                .join("Packages")
                .to_string_lossy()
                .into_owned(),
        );
    } else {
        ignore_dirs.push("AppData\\Local\\Packages".to_string());
    }
    ignore_dirs.join("\n")
}

/// Resolves the absolute path to the REDR configuration directory
pub fn get_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("REDR"))
}

/// Saves the configuration struct securely as pretty-printed JSON
pub fn save_settings(settings: &AppSettings) {
    if let Some(config_dir) = get_config_dir() {
        let _ = fs::create_dir_all(&config_dir);
        let settings_path = config_dir.join("settings.json");
        if let Ok(json_str) = serde_json::to_string_pretty(settings) {
            let _ = fs::write(settings_path, json_str);
        }
    }
}

/// Loads the configuration state from disk, falling back to Defaults on failure
pub fn load_settings() -> AppSettings {
    if let Some(config_dir) = get_config_dir() {
        let settings_path = config_dir.join("settings.json");
        if let Ok(json_str) = fs::read_to_string(settings_path)
            && let Ok(settings) = serde_json::from_str::<AppSettings>(&json_str)
        {
            return settings;
        }
    }
    AppSettings::default()
}
