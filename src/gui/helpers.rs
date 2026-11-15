//! Helpers shared by the GUI handlers.

use super::{AppState, DirectoryItem};
use redr::config::{self, AppSettings};
use redr::scanner::DirectoryNode;
use redr::view::{self, GROUP_TREE};
use slint::{ModelRc, SharedString, VecModel};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// Whether the logs are written to a file automatically.
pub static AUTO_SAVE_LOGS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// The current search query of the result tree.
pub static SEARCH_QUERY: Mutex<String> = Mutex::new(String::new());
/// The current grouping mode of the result tree.
pub static GROUP_BY: Mutex<i32> = Mutex::new(GROUP_TREE);
/// The root of the last scan, used for the grouping.
pub static SCAN_ROOT: Mutex<String> = Mutex::new(String::new());

/// Refreshes the list of saved profiles shown in the settings tab.
pub fn refresh_profile_list(state: &AppState) {
    let names: Vec<String> = config::load_profiles()
        .into_iter()
        .map(|p| p.name)
        .collect();
    let text = if names.is_empty() {
        String::new()
    } else {
        format!("Saved profiles: {}", names.join(", "))
    };
    state.set_profile_list(text.into());
}

/// Builds the visible list honouring the current grouping mode and search query.
pub fn visible_items(folders: &[DirectoryNode]) -> Vec<view::VisibleNodeData> {
    let query = SEARCH_QUERY.lock().unwrap().clone();
    let mode = *GROUP_BY.lock().unwrap();
    let root = PathBuf::from(SCAN_ROOT.lock().unwrap().clone());
    view::build_visible_items(folders, &root, mode, &query)
}

/// Converts the rows into the model the Slint list view uses.
pub fn to_slint_model(raw_items: Vec<view::VisibleNodeData>) -> ModelRc<DirectoryItem> {
    let slint_items: Vec<DirectoryItem> = raw_items
        .into_iter()
        .map(|item| DirectoryItem {
            name: SharedString::from(item.name),
            path: SharedString::from(item.path),
            depth: item.depth,
            status: item.status,
            has_children: item.has_children,
            is_expanded: item.is_expanded,
            id: item.id,
            is_root: item.is_root,
            tree_lines: Rc::new(VecModel::from(item.tree_lines)).into(),
            is_hidden: item.is_hidden,
            is_symlink: item.is_symlink,
            is_group_header: item.is_group_header,
        })
        .collect();
    Rc::new(VecModel::from(slint_items)).into()
}

/// Reads the current settings from the UI.
pub fn ui_to_settings(app_state: &AppState) -> AppSettings {
    AppSettings {
        version: config::CURRENT_CONFIG_VERSION,
        consider_empty_files_empty: app_state.get_consider_empty_files_empty(),
        ignore_hidden: app_state.get_ignore_hidden(),
        ignore_errors: app_state.get_ignore_errors(),
        hide_search_errors: app_state.get_hide_search_errors(),
        skip_system: app_state.get_skip_system(),
        delete_mode: app_state.get_delete_mode(),
        max_depth: app_state.get_max_depth(),
        pause_ms: app_state.get_pause_ms(),
        min_age_hours: app_state.get_min_age_hours(),
        auto_save_logs: app_state.get_auto_save_logs(),
        mft_scan: app_state.get_mft_scan(),
        mft_include_deleted: app_state.get_mft_include_deleted(),
        empty_file_max_size: app_state.get_empty_file_max_size() as u64,
        ignore_files_older_than_hours: app_state.get_ignore_files_older_than_hours() as u32,
        max_scan_results: app_state.get_max_scan_results() as usize,
        protect_symlinks: app_state.get_protect_symlinks(),
        ignore_list_text: app_state.get_ignore_list_text().to_string(),
        ignore_files_text: app_state.get_ignore_files_text().to_string(),
        whitelist_text: app_state.get_whitelist_text().to_string(),
        trash_only: app_state.get_trash_only(),
        group_by: app_state.get_group_by(),
        dark_theme: app_state.get_dark_theme(),
        incremental_scan: app_state.get_incremental_scan(),
        streaming_scan: app_state.get_streaming_scan(),
        dry_run: app_state.get_dry_run(),
    }
}

/// Applies the saved settings to the UI.
pub fn apply_settings(state: &AppState, settings: &AppSettings) {
    state.set_consider_empty_files_empty(settings.consider_empty_files_empty);
    state.set_ignore_hidden(settings.ignore_hidden);
    state.set_ignore_errors(settings.ignore_errors);
    state.set_hide_search_errors(settings.hide_search_errors);
    state.set_skip_system(settings.skip_system);
    state.set_delete_mode(settings.delete_mode);
    state.set_max_depth(settings.max_depth);
    state.set_pause_ms(settings.pause_ms);
    state.set_min_age_hours(settings.min_age_hours);
    state.set_auto_save_logs(settings.auto_save_logs);
    state.set_mft_scan(settings.mft_scan);
    state.set_mft_include_deleted(settings.mft_include_deleted);
    state.set_empty_file_max_size(settings.empty_file_max_size as i32);
    state.set_ignore_files_older_than_hours(settings.ignore_files_older_than_hours as i32);
    state.set_max_scan_results(settings.max_scan_results as i32);
    state.set_protect_symlinks(settings.protect_symlinks);
    state.set_ignore_list_text(settings.ignore_list_text.clone().into());
    state.set_ignore_files_text(settings.ignore_files_text.clone().into());
    state.set_whitelist_text(settings.whitelist_text.clone().into());
    state.set_trash_only(settings.trash_only);
    state.set_group_by(settings.group_by);
    state.set_dark_theme(settings.dark_theme);
    state.set_incremental_scan(settings.incremental_scan);
    state.set_streaming_scan(settings.streaming_scan);
    state.set_dry_run(settings.dry_run);

    *GROUP_BY.lock().unwrap() = settings.group_by;
    AUTO_SAVE_LOGS.store(settings.auto_save_logs, Ordering::Relaxed);
    refresh_profile_list(state);
}
