#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cli;
mod config;
mod gui;
mod logger;
mod scanner;
mod sys;

use clap::Parser;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut attached_console = false;

    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetStdHandle,
        };
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            attached_console = true;
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            if !stdout.is_null() {
                SetStdHandle(STD_OUTPUT_HANDLE, stdout);
            }
            let stderr = GetStdHandle(STD_ERROR_HANDLE);
            if !stderr.is_null() {
                SetStdHandle(STD_ERROR_HANDLE, stderr);
            }
        }
    }

    let args = cli::Cli::parse();

    let force_gui = args.gui;

    // Run CLI when explicit flags are passed or when invoked directly from a console with a path
    let should_run_cli = !force_gui
        && (args.cli
            || args.quiet
            || args.delete
            || args.dry_run
            || args.json
            || (args.path.is_some() && attached_console));

    if should_run_cli {
        cli::run_cli(args)?;
    } else {
        gui::run_gui(args)?;
    }

    Ok(())
}
