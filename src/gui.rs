slint::include_modules!();

use crate::cli::Cli;
use crate::config::{self, AppSettings};
use crate::logger::{LogEvent, UiLogger};
use crate::scanner;
use crate::sys;
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ModelRc, SharedString, VecModel};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

pub static AUTO_SAVE_LOGS: AtomicBool = AtomicBool::new(false);

/// Thread-safe intermediate Data Transfer Object (DTO)
/// Standard Rust types are used here so this struct can cross thread boundaries (Send).
struct VisibleNodeData {
    name: String,
    path: String,
    depth: i32,
    status: i32,
    has_children: bool,
    is_expanded: bool,
    id: i32,
    is_root: bool,
    tree_lines: Vec<i32>,
    is_hidden: bool,
    is_symlink: bool,
}

/// Helper function to transform scanner nodes into pure thread-safe Rust models
fn rebuild_visible_items(folders: &[scanner::DirectoryNode]) -> Vec<VisibleNodeData> {
    let mut result = Vec::new();
    let mut hide_depth = i32::MAX;
    let mut active_depths: Vec<bool> = Vec::new();

    for (i, node) in folders.iter().enumerate() {
        if node.depth <= hide_depth {
            hide_depth = i32::MAX;

            if node.depth > 0 {
                let idx = (node.depth - 1) as usize;
                if active_depths.len() <= idx {
                    active_depths.resize(idx + 1, false);
                }
                active_depths[idx] = !node.is_last_sibling;
            }

            let mut tree_lines_vec = Vec::new();
            for d in 0..(node.depth - 1) {
                let d = d as usize;
                if d < active_depths.len() && active_depths[d] {
                    tree_lines_vec.push(1);
                } else {
                    tree_lines_vec.push(0);
                }
            }
            if node.depth > 0 {
                if node.is_last_sibling {
                    tree_lines_vec.push(3);
                } else {
                    tree_lines_vec.push(2);
                }
            }

            result.push(VisibleNodeData {
                name: node.name.to_string(),
                path: node.path_str.to_string(),
                depth: node.depth,
                status: node.status,
                has_children: node.has_children,
                is_expanded: node.is_expanded,
                id: i as i32,
                is_root: node.depth == 0,
                tree_lines: tree_lines_vec,
                is_hidden: node.is_hidden,
                is_symlink: node.is_symlink,
            });

            if !node.is_expanded {
                hide_depth = node.depth;
            }
        }
    }
    result
}

/// Converts the thread-safe intermediate models into Slint-specific UI models (must be called inside the UI Thread)
fn to_slint_model(raw_items: Vec<VisibleNodeData>) -> ModelRc<DirectoryItem> {
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
            tree_prefix: SharedString::new(),
            tree_lines: std::rc::Rc::new(slint::VecModel::from(item.tree_lines)).into(),
            is_hidden: item.is_hidden,
            is_symlink: item.is_symlink,
        })
        .collect();
    Rc::new(VecModel::from(slint_items)).into()
}

/// Extracts the current configuration state from the Slint Global State
fn ui_to_settings(app_state: &AppState) -> AppSettings {
    AppSettings {
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
        ignore_list_text: app_state.get_ignore_list_text().to_string(),
        ignore_files_text: app_state.get_ignore_files_text().to_string(),
        dry_run: app_state.get_dry_run(),
    }
}

pub fn run_gui(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let ui_handle = ui.as_weak();

    // Extracted global state handle for bridging Logic to GUI seamlessly
    let app_state = ui.global::<AppState>();

    // Load persisted settings on startup
    let settings = config::load_settings();

    // Set atomic flag for logs immediately
    AUTO_SAVE_LOGS.store(settings.auto_save_logs, Ordering::Relaxed);

    // Apply loaded parameters directly to Slint UI State properties
    app_state.set_consider_empty_files_empty(settings.consider_empty_files_empty);
    app_state.set_ignore_hidden(settings.ignore_hidden);
    app_state.set_ignore_errors(settings.ignore_errors);
    app_state.set_hide_search_errors(settings.hide_search_errors);
    app_state.set_skip_system(settings.skip_system);
    app_state.set_delete_mode(settings.delete_mode);
    app_state.set_max_depth(settings.max_depth);
    app_state.set_pause_ms(settings.pause_ms);
    app_state.set_min_age_hours(settings.min_age_hours);
    app_state.set_auto_save_logs(settings.auto_save_logs);
    app_state.set_mft_scan(settings.mft_scan);
    app_state.set_ignore_list_text(settings.ignore_list_text.into());
    app_state.set_ignore_files_text(settings.ignore_files_text.into());
    app_state.set_dry_run(settings.dry_run);

    // Initialize OS-specific options
    app_state.set_is_integrated(sys::check_registry_integration());
    app_state.set_is_admin(sys::is_admin());

    // Drag-and-Drop Integration
    let ui_weak_dnd = ui_handle.clone();
    ui.window().on_winit_window_event(move |_, event| {
        if let winit::event::WindowEvent::DroppedFile(path_buf) = event
            && let Some(ui) = ui_weak_dnd.upgrade()
        {
            ui.global::<AppState>()
                .set_selected_folder(SharedString::from(path_buf.to_string_lossy().into_owned()));
        }
        slint::winit_030::EventResult::Propagate
    });

    if let Some(path) = cli.path {
        app_state.set_selected_folder(SharedString::from(path));
    }

    let (log_tx, log_rx) = mpsc::channel::<LogEvent>();
    let logger = UiLogger::new_gui(log_tx);

    let found_folders = Arc::new(Mutex::new(Vec::<scanner::DirectoryNode>::new()));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    app_state.set_directories(ModelRc::from(Rc::new(VecModel::from(vec![]))));

    let ui_weak_log = ui_handle.clone();
    let found_folders_log = found_folders.clone();

    // Background thread to manage logs, progress metrics, and UI state updates
    thread::spawn(move || {
        let mut logs = VecDeque::with_capacity(300);
        let mut last_rebuild_time = std::time::Instant::now();
        let mut pending_status_updates = false;

        let log_file_path = config::get_config_dir().map(|d| d.join("logs").join("redr.log"));
        if let Some(ref p) = log_file_path {
            let _ = fs::create_dir_all(p.parent().unwrap());
        }

        while let Ok(evt) = log_rx.recv() {
            let mut status_updates = Vec::new();
            let mut progress_update = None;
            let mut logs_changed = false;
            let mut batch_log_msgs = Vec::new();

            let mut process_event = |e: LogEvent| match e {
                LogEvent::Msg(msg) => {
                    batch_log_msgs.push(msg.clone());
                    logs.push_back(msg);
                    logs_changed = true;
                }
                LogEvent::StatusChange(index, status) => {
                    status_updates.push((index, status));
                    pending_status_updates = true;
                }
                LogEvent::Progress(p) => progress_update = Some(p),
            };

            process_event(evt);

            // Drain all pending events rapidly from the channel to prevent lockups
            while let Ok(m) = log_rx.try_recv() {
                process_event(m);
            }

            // Batch-write log entries in a single OS transaction
            if !batch_log_msgs.is_empty()
                && AUTO_SAVE_LOGS.load(Ordering::Relaxed)
                && let Some(ref path) = log_file_path
                && let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path)
            {
                use std::io::Write;
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                for msg in batch_log_msgs {
                    let _ = writeln!(file, "[{}] {}", timestamp, msg.trim_end());
                }
            }

            while logs.len() > 250 {
                logs.pop_front();
                logs_changed = true;
            }

            let folders_clone = {
                let mut folders = found_folders_log.lock().unwrap();
                for &(index, status) in &status_updates {
                    if let Some(node) = folders.get_mut(index) {
                        node.status = status;
                    }
                }
                folders.clone()
            };

            let now = std::time::Instant::now();
            let elapsed_ms = now.duration_since(last_rebuild_time).as_millis();
            let is_finished = progress_update.map(|p| p >= 1.0).unwrap_or(false);

            let should_rebuild = pending_status_updates
                && (elapsed_ms >= 120 || folders_clone.len() < 150 || is_finished);

            let combined = if logs_changed {
                Some(logs.iter().cloned().collect::<String>())
            } else {
                None
            };

            // Perform heavy filtering in the background thread (using pure Rust types)
            let list_items_opt = if should_rebuild {
                last_rebuild_time = now;
                pending_status_updates = false;
                Some(rebuild_visible_items(&folders_clone))
            } else {
                None
            };

            // Pass the Send-safe data across the thread boundary
            let _ = ui_weak_log.upgrade_in_event_loop(move |ui| {
                let state = ui.global::<AppState>();
                if let Some(log_str) = combined {
                    state.set_log_text(log_str.into());
                }

                if let Some(p) = progress_update {
                    state.set_progress(p);
                }

                // Smoothly inject pre-built UI components without halting the main thread
                if let Some(items) = list_items_opt {
                    state.set_directories(to_slint_model(items));
                }
            });
            thread::sleep(std::time::Duration::from_millis(16));
        }
    });

    app_state.on_browse_folder(move || {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            SharedString::from(path.to_string_lossy().into_owned())
        } else {
            SharedString::new()
        }
    });

    let ui_weak_exit = ui_handle.clone();
    app_state.on_exit_app(move || {
        if let Some(ui) = ui_weak_exit.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
        std::process::exit(0);
    });

    let ui_weak_cancel = ui_handle.clone();
    let cancel_flag_cancel = cancel_flag.clone();
    app_state.on_cancel_operation(move || {
        cancel_flag_cancel.store(true, Ordering::Relaxed);
        if let Some(ui) = ui_weak_cancel.upgrade() {
            ui.global::<AppState>()
                .set_status_msg("Cancellation requested...".into());
        }
    });

    let ui_weak_toggle = ui_handle.clone();
    let found_folders_toggle = found_folders.clone();
    app_state.on_toggle_expand(move |id| {
        let mut folders = found_folders_toggle.lock().unwrap();
        if let Some(node) = folders.get_mut(id as usize) {
            node.is_expanded = !node.is_expanded;
        }
        let list_items = rebuild_visible_items(&folders);
        if let Some(ui) = ui_weak_toggle.upgrade() {
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

    let ui_weak_exclude = ui_handle.clone();
    let found_folders_exclude = found_folders.clone();
    app_state.on_add_to_exclusions(move |id| {
        let ui = match ui_weak_exclude.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        let state = ui.global::<AppState>();

        let mut folders = found_folders_exclude.lock().unwrap();
        if let Some(node) = folders.get_mut(id as usize) {
            let folder_name = node.name.clone();

            let mut current_list = state.get_ignore_list_text().to_string();
            let current_items: std::collections::HashSet<&str> =
                current_list.split('\n').map(|s| s.trim()).collect();

            if !current_items.contains(folder_name.as_str()) {
                if !current_list.is_empty() && !current_list.ends_with('\n') {
                    current_list.push('\n');
                }
                current_list.push_str(&folder_name);
                state.set_ignore_list_text(current_list.into());
            }

            node.status = 3;
            let target_path_buf = node.path.to_path_buf();
            for other_node in folders.iter_mut() {
                if other_node.path.starts_with(&target_path_buf) {
                    other_node.status = 3;
                }
            }
        }

        let list_items = rebuild_visible_items(&folders);
        state.set_directories(to_slint_model(list_items));

        let empty_count = folders.iter().filter(|d| d.status == 1).count();
        state.set_empty_count(empty_count as i32);

        config::save_settings(&ui_to_settings(&state));
    });

    let ui_weak_reset = ui_handle.clone();
    app_state.on_reset_defaults(move || {
        let ui = match ui_weak_reset.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        let state = ui.global::<AppState>();

        state.set_consider_empty_files_empty(true);
        state.set_ignore_hidden(true);
        state.set_ignore_errors(true);
        state.set_hide_search_errors(true);
        state.set_skip_system(true);
        state.set_max_depth(-1);
        state.set_pause_ms(0);
        state.set_min_age_hours(0);
        state.set_delete_mode(0);
        state.set_dry_run(false);
        state.set_ignore_files_text("desktop.ini\nThumbs.db\n.DS_Store".into());
        state.set_ignore_list_text(config::get_default_ignore_dirs().into());

        config::save_settings(&ui_to_settings(&state));
    });

    let ui_weak_context = ui_handle.clone();
    app_state.on_toggle_context_menu(move |integrate| {
        if let Err(e) = sys::set_registry_integration(integrate) {
            eprintln!("[!] Context menu integration failed: {}", e);
        }
        if let Some(ui) = ui_weak_context.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
    });

    let ui_weak_autosave = ui_handle.clone();
    app_state.on_toggle_auto_save_logs(move |save| {
        AUTO_SAVE_LOGS.store(save, Ordering::Relaxed);
        if let Some(ui) = ui_weak_autosave.upgrade() {
            config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
        }
    });

    let ui_weak_export = ui_handle.clone();
    app_state.on_export_log(move || {
        let ui = match ui_weak_export.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        let current_log = ui.global::<AppState>().get_log_text().to_string();

        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("redr_session.log")
            .add_filter("Log Files", &["log", "txt"])
            .save_file()
        {
            let _ = fs::write(path, current_log);
        }
    });

    app_state.on_open_logs_folder(move || {
        let log_dir = config::get_config_dir().map(|d| d.join("logs"));
        if let Some(path) = log_dir {
            let _ = fs::create_dir_all(&path);
            let path_str = path.to_string_lossy().into_owned();
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("explorer")
                    .arg(&path_str)
                    .spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open").arg(&path_str).spawn();
            }
            #[cfg(target_os = "linux")]
            {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&path_str)
                    .spawn();
            }
        }
    });

    let ui_weak_scan = ui_handle.clone();
    let logger_scan = logger.clone();
    let found_folders_scan = found_folders.clone();
    let cancel_flag_scan = cancel_flag.clone();
    app_state.on_search_folders(move || {
        let ui_weak = ui_weak_scan.clone();
        let logger = logger_scan.clone();
        let folders_state = found_folders_scan.clone();
        let cancel_flag_thread = cancel_flag_scan.clone();

        let ui = match ui_weak.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        let state = ui.global::<AppState>();

        let folder_path = state.get_selected_folder().to_string();
        let ignore_files = state.get_ignore_files_text().to_string();
        let ignore_dirs = state.get_ignore_list_text().to_string();
        let ignore_hidden = state.get_ignore_hidden();
        let keep_system = state.get_skip_system();
        let min_age_hours = state.get_min_age_hours();
        let max_depth = state.get_max_depth();
        let consider_empty_files_empty = state.get_consider_empty_files_empty();
        let hide_search_errors = state.get_hide_search_errors();
        let use_mft = state.get_mft_scan() && cfg!(target_os = "windows") && sys::is_admin();

        config::save_settings(&ui_to_settings(&state));

        state.set_is_scanning(true);
        state.set_status_msg("Scanning...".into());
        state.set_progress(0.0);

        if folder_path.is_empty() {
            logger.log("Please select a folder first.");
            state.set_is_scanning(false);
            return;
        }

        let path = PathBuf::from(folder_path);

        let settings = scanner::ScanSettings {
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
            ignore_hidden,
            keep_system,
            min_age_hours: min_age_hours as u32,
            max_depth,
            consider_empty_files_empty,
            hide_search_errors,
        };

        cancel_flag_thread.store(false, Ordering::Relaxed);

        let ui_weak_thread = ui.as_weak();
        thread::spawn(move || {
            logger.log(&format!(
                "[*] Scanning for empty directories in: {:?}",
                path
            ));

            let scan_res = if use_mft {
                #[cfg(target_os = "windows")]
                {
                    logger.log("[*] Running in Direct MFT Scan mode...");
                    match scanner::scan_empty_dirs_mft(
                        &path,
                        &settings,
                        &|msg| logger.log(msg),
                        &cancel_flag_thread,
                    ) {
                        Ok(res) => Ok(res),
                        Err(e) => {
                            logger.log(&format!(
                                "[!] Direct MFT Scan failed: {}. Falling back to standard scan...",
                                e
                            ));
                            scanner::scan_empty_dirs(
                                &path,
                                &settings,
                                &|msg| logger.log(msg),
                                &cancel_flag_thread,
                            )
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    scanner::scan_empty_dirs(
                        &path,
                        &settings,
                        &|msg| logger.log(msg),
                        &cancel_flag_thread,
                    )
                }
            } else {
                scanner::scan_empty_dirs(
                    &path,
                    &settings,
                    &|msg| logger.log(msg),
                    &cancel_flag_thread,
                )
            };

            match scan_res {
                Ok(empty_dirs) => {
                    let count = empty_dirs.len();
                    let empty_count = empty_dirs.iter().filter(|d| d.status == 1).count();
                    logger.log(&format!(
                        "[+] Found {} empty directories ({} shown in tree).",
                        empty_count, count
                    ));

                    let folders_clone = {
                        let mut state = folders_state.lock().unwrap();
                        *state = empty_dirs;
                        state.clone()
                    };

                    // Compute UI models purely in background thread
                    let list_items = rebuild_visible_items(&folders_clone);

                    let _ = ui_weak_thread.upgrade_in_event_loop(move |ui| {
                        let state = ui.global::<AppState>();
                        state.set_directories(to_slint_model(list_items));
                        state.set_empty_count(empty_count as i32);
                        state.set_deleted_count(0);
                        state.set_failed_count(0);
                        state.set_is_scanning(false);
                        state.set_status_msg(SharedString::from(format!(
                            "Found {} empty directories.",
                            empty_count
                        )));
                        state.set_progress(1.0);
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
                        let state = ui.global::<AppState>();
                        state.set_is_scanning(false);
                        state.set_status_msg(status.into());
                    });
                }
            }
        });
    });

    let ui_weak_del = ui_handle.clone();
    let logger_del = logger.clone();
    let found_folders_del = found_folders.clone();
    let cancel_flag_del = cancel_flag.clone();
    app_state.on_delete_folders(move || {
        let ui_weak = ui_weak_del.clone();
        let logger = logger_del.clone();
        let folders_state = found_folders_del.clone();
        let cancel_flag_thread = cancel_flag_del.clone();

        let ui = match ui_weak.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        let state = ui.global::<AppState>();

        let move_to_trash = state.get_delete_mode() == 0;
        let ignore_errors = state.get_ignore_errors();
        let pause_ms = state.get_pause_ms();
        let ignore_files = state.get_ignore_files_text().to_string();
        let consider_empty_files_empty = state.get_consider_empty_files_empty();
        let dry_run = state.get_dry_run();

        config::save_settings(&ui_to_settings(&state));

        state.set_is_deleting(true);
        state.set_status_msg("Deleting...".into());
        state.set_progress(0.0);

        let ui_weak_thread = ui.as_weak();
        cancel_flag_thread.store(false, Ordering::Relaxed);

        thread::spawn(move || {
            let mut dirs = {
                let state = folders_state.lock().unwrap();
                state.clone()
            };

            if dirs.is_empty() {
                let _ = ui_weak_thread.upgrade_in_event_loop(|ui| {
                    ui.global::<AppState>().set_is_deleting(false);
                });
                return;
            }

            let settings = scanner::DeleteSettings {
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
            };

            logger.log("[*] Starting deletion process...");

            let (deleted, failed) = scanner::delete_empty_dirs(
                &mut dirs,
                &settings,
                &|msg, idx, stat| logger.status(msg, idx, stat),
                &|p| logger.progress(p),
                &cancel_flag_thread,
            );

            let was_cancelled = cancel_flag_thread.load(Ordering::Relaxed);
            logger.log(&format!(
                "[+] Deletion finished. Deleted: {}, Failed: {}",
                deleted, failed
            ));

            *folders_state.lock().unwrap() = dirs;

            let _ = ui_weak_thread.upgrade_in_event_loop(move |ui| {
                let state = ui.global::<AppState>();
                state.set_deleted_count(deleted as i32);
                state.set_failed_count(failed as i32);
                state.set_is_deleting(false);
                if was_cancelled {
                    state.set_status_msg("Deletion cancelled.".into());
                } else {
                    state.set_status_msg("Deletion complete.".into());
                }
                state.set_progress(1.0);
            });
        });
    });

    let run_result = ui.run();

    if let Some(ui) = ui_handle.upgrade() {
        config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
    }

    Ok(run_result?)
}
