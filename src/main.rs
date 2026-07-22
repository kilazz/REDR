// Global window attributes (must be at the very top of the file)
// Hides the console window on Windows when compiled in release mode.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

// Set the global memory allocator to mimalloc for massive multithreading performance gains
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Declare all application modules
mod cli;
mod config;
mod gui;
mod logger;
mod scanner;
mod sys;

use clap::Parser;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Attach to the parent console on Windows so CLI output is visible
    // even when compiled with the graphical "windows" subsystem.
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }

    // Parse command-line arguments
    let args = cli::Cli::parse();

    // Route the application flow: Headless CLI/JSON mode vs Graphical UI mode
    if args.quiet || args.delete || args.json {
        cli::run_cli(args)?;
    } else {
        gui::run_gui(args)?;
    }

    Ok(())
}
