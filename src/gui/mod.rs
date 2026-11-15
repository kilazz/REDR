//! The graphical interface.
//!
//! The module is split into the parts that handle the different areas of the
//! application; `mod.rs` only wires them together.

slint::include_modules!();

mod actions;
mod delete;
mod export;
mod helpers;
mod scan;
mod settings;
mod state;
mod view_controls;

use crate::cli::Cli;
use helpers::{apply_settings, ui_to_settings};
use redr::config;
use redr::logger::{LogEvent, UiLogger};
use redr::sys;
use slint::winit_030::{WinitWindowAccessor, winit};
use state::GuiState;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};

/// Forwards the tracing events to the log pane.
struct LogEventWriter {
    sender: mpsc::Sender<LogEvent>,
    buf: String,
}

impl std::io::Write for LogEventWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogEventWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let msg = std::mem::take(&mut self.buf);
            let _ = self.sender.send(LogEvent::Msg(msg));
        }
    }
}

/// Sends the tracing events to the log pane.
fn init_tracing(sender: mpsc::Sender<LogEvent>) {
    let _ = tracing_subscriber::fmt()
        .with_writer(move || LogEventWriter {
            sender: sender.clone(),
            buf: String::new(),
        })
        .with_ansi(false)
        .try_init();
}

/// Starts the graphical interface.
pub fn run_gui(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let ui_handle = ui.as_weak();

    let app_state = ui.global::<AppState>();
    let settings = config::load_settings();

    apply_settings(&app_state, &settings);

    app_state.set_is_integrated(sys::check_registry_integration());
    app_state.set_is_admin(sys::is_admin());

    let ui_weak_dnd = ui_handle.clone();
    ui.window().on_winit_window_event(move |_, event| {
        use winit::event::WindowEvent;
        if let WindowEvent::DroppedFile(path_buf) = event
            && let Some(ui) = ui_weak_dnd.upgrade()
        {
            ui.global::<AppState>()
                .set_selected_folder(slint::SharedString::from(
                    path_buf.to_string_lossy().into_owned(),
                ));
        }
        slint::winit_030::EventResult::Propagate
    });

    if let Some(path) = cli.path {
        app_state.set_selected_folder(path.into());
    }

    let (log_tx, log_rx) = mpsc::channel::<LogEvent>();
    init_tracing(log_tx.clone());
    let logger = UiLogger::new_gui(log_tx);

    let state = GuiState {
        ui: ui_handle.clone(),
        folders: Arc::new(Mutex::new(Vec::new())),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        logger: logger.clone(),
    };

    start_log_pane(state.clone(), log_rx);

    actions::register(&state, &app_state);
    settings::register(&state, &app_state);
    export::register(&state, &app_state);
    scan::register(&state, &app_state);
    delete::register(&state, &app_state);
    view_controls::register(&state, &app_state);

    let run_result = ui.run();

    if let Some(ui) = ui_handle.upgrade() {
        config::save_settings(&ui_to_settings(&ui.global::<AppState>()));
    }

    Ok(run_result?)
}

/// Keeps the log pane in sync with the logger.
fn start_log_pane(state: GuiState, log_rx: mpsc::Receiver<LogEvent>) {
    let ui_weak_log = state.ui.clone();
    let found_folders_log = state.folders.clone();

    std::thread::spawn(move || {
        let mut logs = VecDeque::with_capacity(300);
        let mut last_rebuild_time = std::time::Instant::now();
        let mut pending_status_updates = false;

        let log_file_path = config::get_config_dir().map(|d| d.join("logs").join("redr.log"));
        if let Some(ref p) = log_file_path {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
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
            while let Ok(m) = log_rx.try_recv() {
                process_event(m);
            }

            if !batch_log_msgs.is_empty()
                && helpers::AUTO_SAVE_LOGS.load(Ordering::Relaxed)
                && let Some(ref path) = log_file_path
                && let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
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

            let mut list_items_opt = None;
            if pending_status_updates {
                let now = std::time::Instant::now();
                let elapsed_ms = now.duration_since(last_rebuild_time).as_millis();
                let is_finished = progress_update.map(|p| p >= 1.0).unwrap_or(false);

                let mut folders = found_folders_log.lock().unwrap();
                for &(index, status) in &status_updates {
                    if let Some(node) = folders.get_mut(index) {
                        node.status = redr::scanner::NodeStatus::from_i32(status);
                    }
                }

                let should_rebuild = elapsed_ms >= 120 || folders.len() < 150 || is_finished;
                if should_rebuild {
                    last_rebuild_time = now;
                    pending_status_updates = false;
                    list_items_opt = Some(helpers::visible_items(&folders));
                }
            }

            let combined = if logs_changed {
                Some(logs.iter().cloned().collect::<String>())
            } else {
                None
            };

            let _ = ui_weak_log.upgrade_in_event_loop(move |ui| {
                let ui_state = ui.global::<AppState>();
                if let Some(log_str) = combined {
                    ui_state.set_log_text(log_str.into());
                }
                if let Some(p) = progress_update {
                    ui_state.set_progress(p);
                }
                if let Some(items) = list_items_opt {
                    ui_state.set_directories(helpers::to_slint_model(items));
                }
            });
        }
    });
}
