use std::sync::mpsc;

/// Events passed through the cross-thread logging channel to keep the GUI responsive
pub enum LogEvent {
    /// Standard string message to append directly to the log viewer pane
    Msg(String),
    /// State synchronization action mapping a node index to its operational results
    StatusChange(usize, i32), // (Index, Target Status ID)
    /// Metric notification used to accurately compute the progression bar UI widget
    Progress(f32),
}

/// Thread-safe logger bridge that handles either CLI printing or GUI message passing
#[derive(Clone)]
pub struct UiLogger {
    sender: Option<mpsc::Sender<LogEvent>>,
}

impl UiLogger {
    /// Instantiates a logger optimized for graphical user interface operations
    pub fn new_gui(tx: mpsc::Sender<LogEvent>) -> Self {
        Self { sender: Some(tx) }
    }

    /// Dispatches a log entry. Dispatches to the UI channel if in GUI mode, otherwise prints to stdout.
    pub fn log(&self, msg: &str) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(LogEvent::Msg(format!("{}\n", msg)));
        } else {
            println!("{}", msg);
        }
    }

    /// Dispatches operational status adjustments and synchronizes the active node state
    pub fn status(&self, msg: &str, index: usize, status: i32) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(LogEvent::Msg(format!("{}\n", msg)));
            let _ = sender.send(LogEvent::StatusChange(index, status));
        } else {
            println!("{}", msg);
        }
    }

    /// Updates the global operation progress bar value [0.0 - 1.0]
    pub fn progress(&self, val: f32) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(LogEvent::Progress(val));
        }
    }
}
