//! The state shared by all GUI handlers.

use super::AppWindow;
use redr::logger::UiLogger;
use redr::scanner::DirectoryNode;
use slint::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Everything the GUI handlers need to reach.
#[derive(Clone)]
pub struct GuiState {
    /// The window, used to update the UI.
    pub ui: Weak<AppWindow>,
    /// The directories of the last scan.
    pub folders: Arc<Mutex<Vec<DirectoryNode>>>,
    /// The flag that cancels a running scan or deletion.
    pub cancel: Arc<AtomicBool>,
    /// The logger that feeds the log pane.
    pub logger: UiLogger,
}

impl GuiState {
    /// A copy of the directories of the last scan.
    pub fn folders_snapshot(&self) -> Vec<DirectoryNode> {
        self.folders.lock().unwrap().clone()
    }

    /// Replaces the directories of the last scan and returns the new list.
    pub fn set_folders(&self, folders: Vec<DirectoryNode>) -> Vec<DirectoryNode> {
        let mut state = self.folders.lock().unwrap();
        *state = folders;
        state.clone()
    }
}
