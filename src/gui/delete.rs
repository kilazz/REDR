//! Deleting the empty directories and restoring them.

use super::AppState;
use super::helpers::ui_to_settings;
use super::state::GuiState;
use redr::config;
use redr::scanner::{self, DeleteSettings};
use slint::ComponentHandle;
use slint::SharedString;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;

/// Registers the handlers of the delete and restore actions.
pub fn register(state: &GuiState, app_state: &AppState) {
    let state_del = state.clone();
    app_state.on_delete_folders(move || {
        let state = state_del.clone();
        let Some(ui) = state.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();

        let move_to_trash = ui_state.get_delete_mode() == 0 || ui_state.get_trash_only();
        let ignore_errors = ui_state.get_ignore_errors();
        let pause_ms = ui_state.get_pause_ms();
        let ignore_files = ui_state.get_ignore_files_text().to_string();
        let consider_empty_files_empty = ui_state.get_consider_empty_files_empty();
        let dry_run = ui_state.get_dry_run();
        let whitelist = ui_state.get_whitelist_text().to_string();

        config::save_settings(&ui_to_settings(&ui_state));

        if !dry_run && !confirm_deletion(&state, move_to_trash) {
            return;
        }

        ui_state.set_is_deleting(true);
        ui_state.set_status_msg("Deleting...".into());
        ui_state.set_progress(0.0);

        let ui_weak_thread = ui.as_weak();
        state.cancel.store(false, Ordering::Relaxed);

        thread::spawn(move || {
            let logger = state.logger.clone();
            let cancel_flag = state.cancel.clone();
            let mut dirs = state.folders_snapshot();

            if dirs.is_empty() {
                let _ = ui_weak_thread.upgrade_in_event_loop(|ui| {
                    ui.global::<AppState>().set_is_deleting(false);
                });
                return;
            }

            let settings = DeleteSettings {
                move_to_trash,
                ignore_errors,
                pause_ms: pause_ms as u32,
                ignore_files: ignore_files
                    .split('\n')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                consider_empty_files_empty,
                dry_run,
                protected_paths: whitelist
                    .split('\n')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            };

            logger.log("[*] Starting deletion process...");

            let delete_start = std::time::Instant::now();
            let ui_weak_progress = ui_weak_thread.clone();
            let logger_for_progress = logger.clone();
            let total_for_progress = dirs.len();
            let progress_cb = move |p: f32| {
                let elapsed = delete_start.elapsed().as_secs_f64();
                let eta = if p > 0.001 {
                    (elapsed / p as f64 - elapsed).max(0.0)
                } else {
                    0.0
                };
                let processed = (p * total_for_progress as f32).round() as usize;
                let speed = if elapsed > 0.0 {
                    processed as f64 / elapsed
                } else {
                    0.0
                };
                let msg = format!(
                    "Deleting... {}/{} ({:.0}%, {:.1} items/s, ETA {:.0}s)",
                    processed,
                    total_for_progress,
                    p * 100.0,
                    speed,
                    eta
                );
                let ui_weak = ui_weak_progress.clone();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.global::<AppState>().set_status_msg(msg.into());
                });
                logger_for_progress.progress(p);
            };

            let (deleted, failed) = scanner::delete_empty_dirs(
                &mut dirs,
                &settings,
                &|idx, stat| logger.status_code(idx, stat),
                &progress_cb,
                &cancel_flag,
            );

            let was_cancelled = cancel_flag.load(Ordering::Relaxed);
            logger.log(&format!(
                "[+] Deletion finished. Deleted: {}, Failed: {}",
                deleted, failed
            ));

            let deleted_paths: Vec<PathBuf> = dirs
                .iter()
                .filter(|d| d.status == scanner::NodeStatus::Deleted)
                .map(|d| d.path.to_path_buf())
                .collect();
            // A simulation removes nothing, so it must not pollute the history.
            if !dry_run {
                scanner::append_deleted_paths(&deleted_paths);
                scanner::append_deletion_history(&deleted_paths);
            }

            state.set_folders(dirs);

            let _ = ui_weak_thread.upgrade_in_event_loop(move |ui| {
                let ui_state = ui.global::<AppState>();
                ui_state.set_deleted_count(deleted as i32);
                ui_state.set_failed_count(failed as i32);
                ui_state.set_is_deleting(false);
                if was_cancelled {
                    ui_state.set_status_msg("Deletion cancelled.".into());
                } else {
                    ui_state.set_status_msg("Deletion complete.".into());
                }
                ui_state.set_progress(1.0);
            });
        });
    });

    let state_restore = state.clone();
    app_state.on_restore_deleted(move || {
        let Some(ui) = state_restore.ui.upgrade() else {
            return;
        };
        match scanner::restore_deleted_paths() {
            Ok(n) => {
                ui.global::<AppState>()
                    .set_status_msg(SharedString::from(format!("Restored {} items.", n)));
            }
            Err(e) => {
                ui.global::<AppState>()
                    .set_status_msg(SharedString::from(format!("Restore failed: {}", e)));
            }
        }
    });
}

/// Asks the user to confirm the deletion, twice for a mass deletion.
fn confirm_deletion(state: &GuiState, move_to_trash: bool) -> bool {
    let empty_total = state
        .folders_snapshot()
        .iter()
        .filter(|d| {
            d.status == scanner::NodeStatus::Empty || d.status == scanner::NodeStatus::Orphaned
        })
        .count();

    let confirmed = rfd::MessageDialog::new()
        .set_title("Confirm deletion")
        .set_description(format!("Delete {} empty directories?", empty_total))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    if confirmed != rfd::MessageDialogResult::Yes {
        return false;
    }

    // A permanent or mass deletion gets a second confirmation.
    if !move_to_trash || empty_total > 1 {
        let confirmed = rfd::MessageDialog::new()
            .set_title("Confirm deletion")
            .set_description(format!(
                "Final confirmation: delete {} directories?",
                empty_total
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return false;
        }
    }

    true
}
