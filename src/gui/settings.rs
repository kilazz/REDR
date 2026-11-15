//! The settings tab: defaults, profiles and the Windows integration.

use super::AppState;
use super::helpers::{
    apply_settings, refresh_profile_list, to_slint_model, ui_to_settings, visible_items,
};
use super::state::GuiState;
use redr::config;
use redr::scanner;
use redr::sys;
use slint::ComponentHandle;
use slint::SharedString;
use std::path::PathBuf;

/// Registers the handlers of the settings tab.
pub fn register(state: &GuiState, app_state: &AppState) {
    let state_reset = state.clone();
    app_state.on_reset_defaults(move || {
        let Some(ui) = state_reset.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();

        ui_state.set_consider_empty_files_empty(true);
        ui_state.set_ignore_hidden(true);
        ui_state.set_ignore_errors(true);
        ui_state.set_hide_search_errors(true);
        ui_state.set_skip_system(true);
        ui_state.set_max_depth(-1);
        ui_state.set_pause_ms(0);
        ui_state.set_min_age_hours(0);
        ui_state.set_delete_mode(0);
        ui_state.set_dry_run(true);
        ui_state.set_trash_only(false);
        ui_state.set_whitelist_text(config::get_default_whitelist().into());
        ui_state.set_ignore_files_text("desktop.ini\nThumbs.db\n.DS_Store".into());
        ui_state.set_ignore_list_text(config::get_default_ignore_dirs().into());

        // The cached results belong to the old settings, drop them.
        let root = PathBuf::from(ui_state.get_selected_folder().to_string());
        if !root.as_os_str().is_empty() {
            scanner::cache::clear(&root);
        }

        config::save_settings(&ui_to_settings(&ui_state));
    });

    let state_save = state.clone();
    app_state.on_save_profile(move |name| {
        let Some(ui) = state_save.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();
        match config::save_profile(&name, &ui_to_settings(&ui_state)) {
            Ok(()) => {
                ui_state.set_status_msg(SharedString::from(format!("Profile '{}' saved.", name)));
            }
            Err(e) => {
                ui_state
                    .set_status_msg(SharedString::from(format!("Could not save profile: {}", e)));
            }
        }
        refresh_profile_list(&ui_state);
    });

    let state_load = state.clone();
    app_state.on_load_profile(move |name| {
        let Some(ui) = state_load.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();
        let Some(profile) = config::find_profile(&name) else {
            ui_state.set_status_msg(SharedString::from(format!("Profile '{}' not found.", name)));
            return;
        };

        apply_settings(&ui_state, &profile.settings);
        config::save_settings(&ui_to_settings(&ui_state));
        ui_state.set_status_msg(SharedString::from(format!("Profile '{}' loaded.", name)));

        let folders = state_load.folders_snapshot();
        let items = visible_items(&folders);
        ui_state.set_directories(to_slint_model(items));
    });

    let state_delete = state.clone();
    app_state.on_delete_profile(move |name| {
        let Some(ui) = state_delete.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();
        if config::delete_profile(&name) {
            ui_state.set_status_msg(SharedString::from(format!("Profile '{}' deleted.", name)));
        } else {
            ui_state.set_status_msg(SharedString::from(format!("Profile '{}' not found.", name)));
        }
        refresh_profile_list(&ui_state);
    });

    let state_context = state.clone();
    app_state.on_toggle_context_menu(move |integrate| {
        if let Err(e) = sys::set_registry_integration(integrate) {
            tracing::warn!("Context menu integration failed: {}", e);
        }
        if let Some(ui) = state_context.ui.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
    });

    let state_autosave = state.clone();
    app_state.on_toggle_auto_save_logs(move |save| {
        super::helpers::AUTO_SAVE_LOGS.store(save, std::sync::atomic::Ordering::Relaxed);
        if let Some(ui) = state_autosave.ui.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
    });
}
