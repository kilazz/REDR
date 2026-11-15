//! The controls of the result tree: search, grouping and the cached results.

use super::AppState;
use super::helpers::{GROUP_BY, SEARCH_QUERY, to_slint_model, visible_items};
use super::state::GuiState;
use redr::scanner;
use slint::ComponentHandle;
use slint::SharedString;
use std::path::PathBuf;

/// Registers the handlers of the result tree controls.
pub fn register(state: &GuiState, app_state: &AppState) {
    let state_search = state.clone();
    app_state.on_search_changed(move |query| {
        *SEARCH_QUERY.lock().unwrap() = query.to_string();
        let folders = state_search.folders_snapshot();
        let items = visible_items(&folders);
        if let Some(ui) = state_search.ui.upgrade() {
            ui.global::<AppState>()
                .set_directories(to_slint_model(items));
        }
    });

    let state_cache = state.clone();
    app_state.on_load_cached_scan(move || {
        let Some(ui) = state_cache.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();
        let root = PathBuf::from(ui_state.get_selected_folder().to_string());
        if root.as_os_str().is_empty() {
            ui_state.set_status_msg("Select a folder first.".into());
            return;
        }

        let Some(cached) = scanner::cache::load(&root) else {
            ui_state.set_status_msg("No cached results for this folder.".into());
            return;
        };

        let entries: Vec<(String, i32)> = cached
            .all_dirs
            .iter()
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        let nodes = scanner::nodes_from_cached(&entries, &root);
        let count = nodes.len();

        state_cache.set_folders(nodes);
        let folders = state_cache.folders_snapshot();
        let items = visible_items(&folders);

        ui_state.set_directories(to_slint_model(items));
        ui_state.set_empty_count(count as i32);
        ui_state.set_status_msg(SharedString::from(format!(
            "Loaded {} cached directories.",
            count
        )));
    });

    let state_group = state.clone();
    app_state.on_group_mode_changed(move |mode| {
        *GROUP_BY.lock().unwrap() = mode;
        let folders = state_group.folders_snapshot();
        let items = visible_items(&folders);
        if let Some(ui) = state_group.ui.upgrade() {
            ui.global::<AppState>()
                .set_directories(to_slint_model(items));
        }
    });
}
