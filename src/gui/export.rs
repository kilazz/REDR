//! Exporting the results and the log, and opening the log folder.

use super::AppState;
use super::state::GuiState;
use redr::config;
use redr::scanner::NodeStatus;
use redr::view;
use slint::ComponentHandle;
use std::fs;

/// The name of a status in the exported files.
fn status_name(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Empty => "empty",
        NodeStatus::Orphaned => "orphaned",
        NodeStatus::Deleted => "deleted",
        NodeStatus::Protected => "protected",
        NodeStatus::Failed => "failed",
        NodeStatus::Normal => "normal",
    }
}

/// Registers the handlers of the export actions.
pub fn register(state: &GuiState, app_state: &AppState) {
    let state_export = state.clone();
    app_state.on_export_log(move || {
        let Some(ui) = state_export.ui.upgrade() else {
            return;
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

    let state_results = state.clone();
    app_state.on_export_results(move || {
        let Some(ui) = state_results.ui.upgrade() else {
            return;
        };

        let folders = state_results.folders_snapshot();
        let path = match rfd::FileDialog::new()
            .set_file_name("redr_results.csv")
            .add_filter("CSV", &["csv"])
            .add_filter("JSON", &["json"])
            .save_file()
        {
            Some(p) => p,
            None => return,
        };

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if ext == "json" {
            let mut json = String::from("[\n");
            for (i, d) in folders.iter().enumerate() {
                json.push_str(&format!(
                    "  {{\"path\":{},\"status\":\"{}\",\"depth\":{}}}",
                    view::serde_json_string(&d.path_str),
                    status_name(d.status),
                    d.depth
                ));
                if i + 1 < folders.len() {
                    json.push(',');
                }
                json.push('\n');
            }
            json.push(']');
            let _ = fs::write(path, json);
        } else {
            let mut csv = String::from("path,status,depth,hidden,symlink\n");
            for d in &folders {
                csv.push_str(&format!(
                    "{},{},{},{},{}\n",
                    view::csv_escape(&d.path_str),
                    status_name(d.status),
                    d.depth,
                    d.is_hidden,
                    d.is_symlink
                ));
            }
            let _ = fs::write(path, csv);
        }

        ui.global::<AppState>()
            .set_status_msg("Results exported.".into());
    });

    app_state.on_open_logs_folder(move || {
        let log_dir = config::get_config_dir().map(|d| d.join("logs"));
        let Some(path) = log_dir else {
            return;
        };
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
    });
}
