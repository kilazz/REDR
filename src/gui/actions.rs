//! The general GUI actions: browse, exit, cancel, expand and the context menu.

use super::AppState;
use super::helpers::{to_slint_model, ui_to_settings, visible_items};
use super::state::GuiState;
use redr::config;
use redr::scanner;
use slint::ComponentHandle;
use slint::SharedString;
use std::sync::atomic::Ordering;

/// Registers the handlers of the general actions.
pub fn register(state: &GuiState, app_state: &AppState) {
    app_state.on_browse_folder(move || {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            SharedString::from(path.to_string_lossy().into_owned())
        } else {
            SharedString::new()
        }
    });

    let state_exit = state.clone();
    app_state.on_exit_app(move || {
        if let Some(ui) = state_exit.ui.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
        std::process::exit(0);
    });

    let state_cancel = state.clone();
    app_state.on_cancel_operation(move || {
        state_cancel.cancel.store(true, Ordering::Relaxed);
        if let Some(ui) = state_cancel.ui.upgrade() {
            ui.global::<AppState>()
                .set_status_msg("Cancellation requested...".into());
        }
    });

    let state_toggle = state.clone();
    app_state.on_toggle_expand(move |id| {
        let mut folders = state_toggle.folders.lock().unwrap();
        if let Some(node) = (id >= 0)
            .then_some(id as usize)
            .and_then(|i| folders.get_mut(i))
        {
            node.is_expanded = !node.is_expanded;
        }
        let list_items = visible_items(&folders);
        drop(folders);
        if let Some(ui) = state_toggle.ui.upgrade() {
            ui.global::<AppState>()
                .set_directories(to_slint_model(list_items));
        }
    });

    app_state.on_open_in_explorer(move |path| {
        let path = path.as_str();
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer").arg(path).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(path).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
        }
    });

    // Restore a single deleted folder from the recycle bin.
    let state_restore = state.clone();
    app_state.on_restore_context_item(move || {
        let Some(ui) = state_restore.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();
        let path = ui_state.get_selected_context_item_path().to_string();
        if path.is_empty() {
            return;
        }

        match scanner::restore_single_path(std::path::Path::new(&path)) {
            Ok(0) => {
                ui_state.set_status_msg("This item is not in the recycle bin.".into());
            }
            Ok(_) => {
                ui_state.set_status_msg(SharedString::from(format!("Restored: {}", path)));
                // Mark the node as restored in the tree.
                {
                    let mut folders = state_restore.folders.lock().unwrap();
                    if let Some(node) = folders.iter_mut().find(|n| n.path_str == path) {
                        node.status = scanner::NodeStatus::Normal;
                    }
                }
                let folders = state_restore.folders_snapshot();
                let items = visible_items(&folders);
                ui_state.set_directories(to_slint_model(items));
            }
            Err(e) => {
                ui_state.set_status_msg(SharedString::from(format!("Restore failed: {}", e)));
            }
        }
    });

    // Flexible Exclusion Protection (by Name or by Full Path)
    let state_exclude = state.clone();
    app_state.on_add_to_exclusions(move |id, use_full_path| {
        let Some(ui) = state_exclude.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();

        let mut folders = state_exclude.folders.lock().unwrap();
        let Some(target_idx) = (id >= 0)
            .then_some(id as usize)
            .filter(|&i| i < folders.len())
        else {
            return;
        };

        let entry_to_add = if use_full_path {
            folders[target_idx].path_str.clone()
        } else {
            folders[target_idx].name.clone()
        };
        let target_path_buf = folders[target_idx].path.to_path_buf();

        let mut current_list = ui_state.get_ignore_list_text().to_string();
        let current_items: std::collections::HashSet<&str> =
            current_list.split('\n').map(|s| s.trim()).collect();

        if !current_items.contains(entry_to_add.as_str()) {
            if !current_list.is_empty() && !current_list.ends_with('\n') {
                current_list.push('\n');
            }
            current_list.push_str(&entry_to_add);
            ui_state.set_ignore_list_text(current_list.into());
        }

        // Mark node and descendants as Protected; demote ancestors from Empty to Normal
        for node in folders.iter_mut() {
            if node.path.starts_with(&target_path_buf) {
                node.status = scanner::NodeStatus::Protected;
            } else if target_path_buf.starts_with(&*node.path)
                && node.status == scanner::NodeStatus::Empty
            {
                node.status = scanner::NodeStatus::Normal;
            }
        }

        let list_items = visible_items(&folders);
        drop(folders);
        ui_state.set_directories(to_slint_model(list_items));
    });
}
