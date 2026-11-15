//! Scanning for empty directories.

use super::AppState;
use super::helpers::{GROUP_BY, SCAN_ROOT, to_slint_model, ui_to_settings, visible_items};
use super::state::GuiState;
use redr::config;
use redr::scanner::{self, ScanSettings};
use redr::sys;
use slint::ComponentHandle;
use slint::SharedString;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;

/// Registers the handler of the scan action.
pub fn register(state: &GuiState, app_state: &AppState) {
    let state_scan = state.clone();
    app_state.on_search_folders(move || {
        let state = state_scan.clone();
        let Some(ui) = state.ui.upgrade() else {
            return;
        };
        let ui_state = ui.global::<AppState>();

        let folder_path = ui_state.get_selected_folder().to_string();
        let settings = match read_settings(&ui_state) {
            Ok(settings) => settings,
            Err(message) => {
                state.logger.log(&message);
                ui_state.set_status_msg(message.into());
                return;
            }
        };

        *SCAN_ROOT.lock().unwrap() = folder_path.clone();
        *GROUP_BY.lock().unwrap() = ui_state.get_group_by();

        config::save_settings(&ui_to_settings(&ui_state));

        ui_state.set_is_scanning(true);
        ui_state.set_status_msg("Scanning...".into());
        ui_state.set_progress(0.0);

        let path = PathBuf::from(&folder_path);
        if folder_path.is_empty() {
            state.logger.log("Please select a folder first.");
            ui_state.set_is_scanning(false);
            return;
        }
        if !path.is_dir() {
            state.logger.log(&format!(
                "[!] Path does not exist or is not a directory: {:?}",
                path
            ));
            ui_state.set_is_scanning(false);
            return;
        }

        let use_mft = ui_state.get_mft_scan() && cfg!(target_os = "windows") && sys::is_admin();
        let incremental = ui_state.get_incremental_scan();
        let streaming = ui_state.get_streaming_scan() && !incremental;

        state.cancel.store(false, Ordering::Relaxed);

        let ui_weak_thread = ui.as_weak();
        thread::spawn(move || {
            let logger = state.logger.clone();
            let cancel_flag = state.cancel.clone();
            logger.log(&format!(
                "[*] Scanning for empty directories in: {:?}",
                path
            ));

            let scan_res = run_scan(
                &path,
                &settings,
                use_mft,
                incremental,
                streaming,
                &cancel_flag,
                &logger,
            );

            match scan_res {
                Ok(empty_dirs) => {
                    let count = empty_dirs.len();
                    let empty_count = empty_dirs
                        .iter()
                        .filter(|d| {
                            d.status == scanner::NodeStatus::Empty
                                || d.status == scanner::NodeStatus::Orphaned
                        })
                        .count();
                    logger.log(&format!(
                        "[+] Found {} empty directories ({} shown in tree).",
                        empty_count, count
                    ));

                    // Compare with the previous scan of the same root and cache the result.
                    let current: Vec<(String, i32)> = empty_dirs
                        .iter()
                        .filter(|d| {
                            d.status == scanner::NodeStatus::Empty
                                || d.status == scanner::NodeStatus::Orphaned
                        })
                        .map(|d| (d.path_str.clone(), d.status as i32))
                        .collect();
                    if let Some(cached) = scanner::cache::load(&path) {
                        let delta = scanner::cache::diff(&cached, &current);
                        if !delta.is_empty() {
                            logger.log(&format!(
                                "[i] Since last scan: {} newly empty, {} no longer empty.",
                                delta.newly_empty.len(),
                                delta.no_longer_empty.len()
                            ));
                        }
                    }
                    scanner::cache::store(&path, &current);

                    let folders_guard = {
                        let mut folders = state.folders.lock().unwrap();
                        *folders = empty_dirs;
                        visible_items(&folders)
                    };

                    let _ = ui_weak_thread.upgrade_in_event_loop(move |ui| {
                        let ui_state = ui.global::<AppState>();
                        ui_state.set_directories(to_slint_model(folders_guard));
                        ui_state.set_empty_count(empty_count as i32);
                        ui_state.set_deleted_count(0);
                        ui_state.set_failed_count(0);
                        ui_state.set_is_scanning(false);
                        ui_state.set_status_msg(SharedString::from(format!(
                            "Found {} empty directories.",
                            empty_count
                        )));
                        ui_state.set_progress(1.0);
                    });
                }
                Err(e) => {
                    logger.log(&format!("[!] {}", e));
                    let status = if e.contains("cancelled") {
                        "Scan cancelled."
                    } else {
                        "Scan failed."
                    };
                    let _ = ui_weak_thread.upgrade_in_event_loop(move |ui| {
                        let ui_state = ui.global::<AppState>();
                        ui_state.set_is_scanning(false);
                        ui_state.set_status_msg(status.into());
                    });
                }
            }
        });
    });
}

/// Reads the scan settings from the UI.
fn read_settings(state: &AppState) -> Result<ScanSettings, String> {
    let ignore_files = state.get_ignore_files_text().to_string();
    let ignore_dirs = state.get_ignore_list_text().to_string();
    let whitelist = state.get_whitelist_text().to_string();

    Ok(ScanSettings {
        ignore_files: ignore_files
            .split('\n')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        ignore_dirs: ignore_dirs
            .split('\n')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        ignore_hidden: state.get_ignore_hidden(),
        keep_system: state.get_skip_system(),
        min_age_hours: state.get_min_age_hours() as u32,
        max_depth: state.get_max_depth(),
        consider_empty_files_empty: state.get_consider_empty_files_empty(),
        hide_search_errors: state.get_hide_search_errors(),
        include_deleted: state.get_mft_include_deleted(),
        empty_file_max_size: state.get_empty_file_max_size() as u64,
        ignore_files_older_than_hours: state.get_ignore_files_older_than_hours() as u32,
        max_results: state.get_max_scan_results() as usize,
        protect_symlinks: state.get_protect_symlinks(),
        protected_paths: whitelist
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    })
}

/// Runs the scan with the requested back-end.
fn run_scan(
    path: &std::path::Path,
    settings: &ScanSettings,
    use_mft: bool,
    incremental: bool,
    streaming: bool,
    cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    logger: &redr::logger::UiLogger,
) -> Result<Vec<scanner::DirectoryNode>, String> {
    if use_mft {
        #[cfg(target_os = "windows")]
        {
            logger.log("[*] Running in Direct MFT Scan mode...");
            return match scanner::scan_empty_dirs_mft(path, settings, cancel_flag) {
                Ok(res) => Ok(res),
                Err(e) => {
                    logger.log(&format!(
                        "[!] Direct MFT Scan failed: {}. Falling back to standard scan...",
                        e
                    ));
                    scanner::scan_empty_dirs(path, settings, cancel_flag)
                }
            };
        }
        #[cfg(not(target_os = "windows"))]
        {
            return scanner::scan_empty_dirs(path, settings, cancel_flag);
        }
    }

    if incremental {
        // Reuse the subtrees that did not change since the last scan.
        let previous = scanner::cache::load(path);
        return match scanner::rescan(path, settings, previous.as_ref(), cancel_flag) {
            Ok(result) => {
                logger.log(&format!(
                    "[i] Incremental scan: {} reused, {} rescanned.",
                    result.reused, result.rescanned
                ));
                scanner::cache::store_cache(&result.cache);
                Ok(result.nodes)
            }
            Err(e) => {
                logger.log(&format!("[!] Incremental scan failed: {}", e));
                scanner::scan_empty_dirs(path, settings, cancel_flag)
            }
        };
    }

    if streaming {
        // Memory use stays proportional to the depth of the tree.
        let mut collected: Vec<scanner::DirectoryNode> = Vec::new();
        let result = scanner::scan_streaming(path, settings, cancel_flag, &mut |node| {
            collected.push(node)
        });
        return result.map(|_| collected);
    }

    scanner::scan_empty_dirs(path, settings, cancel_flag)
}
