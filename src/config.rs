use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const CURRENT_CONFIG_VERSION: u32 = 3;

/// Complete persistent configuration struct mapped to settings.json
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct AppSettings {
    pub version: u32,
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
    pub mft_include_deleted: bool,
    pub empty_file_max_size: u64,
    pub ignore_files_older_than_hours: u32,
    pub max_scan_results: usize,
    pub protect_symlinks: bool,
    pub ignore_list_text: String,
    pub ignore_files_text: String,
    pub whitelist_text: String,
    pub trash_only: bool,
    pub group_by: i32,
    pub dark_theme: bool,
    pub incremental_scan: bool,
    pub streaming_scan: bool,
    pub dry_run: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: CURRENT_CONFIG_VERSION,
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
            mft_include_deleted: false,
            empty_file_max_size: 0,
            ignore_files_older_than_hours: 0,
            max_scan_results: 0,
            protect_symlinks: true,
            ignore_list_text: get_default_ignore_dirs(),
            ignore_files_text: "desktop.ini\nThumbs.db\n.DS_Store".to_string(),
            whitelist_text: get_default_whitelist(),
            trash_only: false,
            group_by: 0,
            dark_theme: true,
            incremental_scan: true,
            streaming_scan: true,
            dry_run: true,
        }
    }
}

/// Generates platform-specific and safety-critical default ignore directories
pub fn get_default_ignore_dirs() -> String {
    let mut ignore_dirs = vec![
        // System & OS Protected Folders
        "System Volume Information".to_string(),
        "RECYCLER".to_string(),
        "Recycled".to_string(),
        "NtUninstall".to_string(),
        "$RECYCLE.BIN".to_string(),
        "GAC_MSIL".to_string(),
        "GAC_32".to_string(),
        "winsxs".to_string(),
        "System32".to_string(),
        // Developer, VCS & Package Manager Guards
        ".git".to_string(),
        ".svn".to_string(),
        ".hg".to_string(),
        "node_modules".to_string(),
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

/// Generates the default whitelist: system locations that must never be deleted
pub fn get_default_whitelist() -> String {
    let mut entries: Vec<String> = Vec::new();

    if cfg!(target_os = "windows") {
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        for sub in [
            "\\Windows",
            "\\Program Files",
            "\\Program Files (x86)",
            "\\ProgramData",
            "\\System Volume Information",
            "\\$Recycle.Bin",
            "\\Recovery",
            "\\Users\\All Users",
            "\\Users\\Default",
        ] {
            entries.push(format!("{}{}", system_drive, sub));
        }
    }

    entries.join("\n")
}

/// Automatically injects missing VCS/package manager guard exclusions during config upgrades
fn migrate_safety_exclusions(ignore_list: &mut String) {
    let safety_dirs = [".git", ".svn", ".hg", "node_modules"];
    let existing: FxHashSet<String> = ignore_list.lines().map(|s| s.trim().to_string()).collect();

    for &dir in &safety_dirs {
        if !existing.contains(dir) {
            if !ignore_list.is_empty() && !ignore_list.ends_with('\n') {
                ignore_list.push('\n');
            }
            ignore_list.push_str(dir);
        }
    }
}

pub fn get_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("REDR"))
}

pub fn save_settings(settings: &AppSettings) {
    if let Some(config_dir) = get_config_dir() {
        let _ = fs::create_dir_all(&config_dir);
        let settings_path = config_dir.join("settings.json");
        if let Ok(json_str) = serde_json::to_string_pretty(settings) {
            let _ = fs::write(settings_path, json_str);
        }
    }
}

pub fn load_settings() -> AppSettings {
    if let Some(config_dir) = get_config_dir() {
        let settings_path = config_dir.join("settings.json");
        if let Ok(json_str) = fs::read_to_string(&settings_path)
            && let Ok(mut settings) = serde_json::from_str::<AppSettings>(&json_str)
        {
            // One-time non-destructive migration for existing configs
            if settings.version < CURRENT_CONFIG_VERSION {
                migrate_safety_exclusions(&mut settings.ignore_list_text);
                settings.version = CURRENT_CONFIG_VERSION;
                save_settings(&settings);
            }
            return settings;
        }
    }
    AppSettings::default()
}

// ==========================================
// SETTINGS PROFILES
// ==========================================

/// A named snapshot of the scan configuration the user can switch between.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SettingsProfile {
    pub name: String,
    pub settings: AppSettings,
}

/// Returns the directory where profiles are stored.
pub fn get_profiles_path() -> Option<PathBuf> {
    get_config_dir().map(|d| d.join("profiles.json"))
}

/// Loads every saved profile, newest configuration version first.
pub fn load_profiles() -> Vec<SettingsProfile> {
    let Some(path) = get_profiles_path() else {
        return Vec::new();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<SettingsProfile>>(&s).ok())
        .unwrap_or_default()
}

/// Writes the full profile list back to disk.
pub fn save_profiles(profiles: &[SettingsProfile]) -> Result<(), String> {
    let path = get_profiles_path().ok_or_else(|| "Config dir not found".to_string())?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(profiles).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

/// Creates or replaces a profile with the given name.
pub fn save_profile(name: &str, settings: &AppSettings) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }
    let mut profiles = load_profiles();
    profiles.retain(|p| !p.name.eq_ignore_ascii_case(name));
    profiles.push(SettingsProfile {
        name: name.to_string(),
        settings: settings.clone(),
    });
    save_profiles(&profiles)
}

/// Removes a profile by name; returns true when something was removed.
pub fn delete_profile(name: &str) -> bool {
    let mut profiles = load_profiles();
    let before = profiles.len();
    profiles.retain(|p| !p.name.eq_ignore_ascii_case(name.trim()));
    if profiles.len() == before {
        return false;
    }
    let _ = save_profiles(&profiles);
    true
}

/// Looks a profile up by name.
pub fn find_profile(name: &str) -> Option<SettingsProfile> {
    load_profiles()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}
