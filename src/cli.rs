use crate::scanner::{self, DeleteSettings, ScanSettings};
use crate::sys::is_admin;
use clap::Parser;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// CLI argument definition structure parsed via clap
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// The target directory path to scan.
    #[arg(index = 1)]
    pub path: Option<String>,

    /// Run without launching the graphical user interface.
    #[arg(short, long)]
    pub quiet: bool,

    /// Automatically delete empty directories found during the scan.
    #[arg(short, long)]
    pub delete: bool,

    /// Simulate deletion without modifying any files (Dry-Run).
    #[arg(long)]
    pub dry_run: bool,

    /// Format CLI output as structured JSON.
    #[arg(long)]
    pub json: bool,

    /// Enable experimental Direct MFT Scan on Windows (Requires Administrator).
    #[arg(long)]
    pub mft_scan: bool,

    /// Maximum folder recursion depth (-1 = unlimited)
    #[arg(long, default_value_t = -1)]
    pub max_depth: i32,

    /// Delete matched folders permanently instead of sending them to the trash
    #[arg(long)]
    pub delete_permanently: bool,

    /// List of comma-separated filenames to ignore (e.g., desktop.ini)
    #[arg(long, default_value = "desktop.ini,Thumbs.db,.DS_Store")]
    pub ignore_files: String,

    /// List of comma-separated directories to ignore
    #[arg(
        long,
        default_value = "System Volume Information,RECYCLER,Recycled,$RECYCLE.BIN"
    )]
    pub ignore_dirs: String,

    /// Skip hidden directories during scan
    #[arg(long, default_value_t = true)]
    pub ignore_hidden: bool,

    /// Skip OS system directories during scan
    #[arg(long, default_value_t = true)]
    pub keep_system: bool,

    /// Ignore folders younger than N hours old
    #[arg(long, default_value_t = 0)]
    pub min_age_hours: u32,

    /// Consider folders containing only empty files as empty
    #[arg(long, default_value_t = true)]
    pub consider_empty_files_empty: bool,

    /// Collapse access-denied style errors into a single summary line
    #[arg(long, default_value_t = true)]
    pub hide_search_errors: bool,
}

#[derive(Serialize)]
struct JsonReport {
    scan_path: String,
    empty_directories_found: Vec<JsonDir>,
    deletion_summary: Option<JsonDeletionSummary>,
}

#[derive(Serialize)]
struct JsonDir {
    path: String,
    status: &'static str,
}

#[derive(Serialize)]
struct JsonDeletionSummary {
    deleted: usize,
    failed: usize,
    dry_run: bool,
}

/// Executes the headless command line operations (scanning, deleting, and printing results)
pub fn run_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path_str = cli
        .path
        .ok_or("Path is required for CLI/Quiet/JSON mode.")?;
    let path = PathBuf::from(&path_str);
    let dummy_cancel = Arc::new(AtomicBool::new(false));

    let scan_settings = ScanSettings {
        ignore_files: cli
            .ignore_files
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        ignore_dirs: cli
            .ignore_dirs
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        ignore_hidden: cli.ignore_hidden,
        keep_system: cli.keep_system,
        min_age_hours: cli.min_age_hours,
        max_depth: cli.max_depth,
        consider_empty_files_empty: cli.consider_empty_files_empty,
        hide_search_errors: cli.hide_search_errors,
    };

    if !cli.quiet && !cli.json {
        println!("[*] Scanning: {:?}", path);
    }

    let use_mft = cli.mft_scan && cfg!(target_os = "windows") && is_admin();

    let mut dirs = if use_mft {
        #[cfg(target_os = "windows")]
        match scanner::scan_empty_dirs_mft(
            &path,
            &scan_settings,
            &|msg| {
                if !cli.quiet && !cli.json {
                    println!("{}", msg);
                }
            },
            &dummy_cancel,
        ) {
            Ok(res) => res,
            Err(_) => scanner::scan_empty_dirs(
                &path,
                &scan_settings,
                &|msg| {
                    if !cli.quiet && !cli.json {
                        println!("{}", msg);
                    }
                },
                &dummy_cancel,
            )?,
        }
        #[cfg(not(target_os = "windows"))]
        scanner::scan_empty_dirs(
            &path,
            &scan_settings,
            &|msg| {
                if !cli.quiet && !cli.json {
                    println!("{}", msg);
                }
            },
            &dummy_cancel,
        )?
    } else {
        scanner::scan_empty_dirs(
            &path,
            &scan_settings,
            &|msg| {
                if !cli.quiet && !cli.json {
                    println!("{}", msg);
                }
            },
            &dummy_cancel,
        )?
    };

    let empty_count = dirs.iter().filter(|d| d.status == 1).count();
    if !cli.quiet && !cli.json {
        println!("[+] Found {} empty directories.", empty_count);
    }

    let mut json_dirs = Vec::new();
    if cli.json {
        for d in &dirs {
            let status_str = match d.status {
                1 => "empty",
                3 => "protected",
                _ => "normal",
            };
            json_dirs.push(JsonDir {
                path: d.path_str.to_string(),
                status: status_str,
            });
        }
    }

    let mut deletion_summary = None;
    if cli.delete && empty_count > 0 {
        let delete_settings = DeleteSettings {
            move_to_trash: !cli.delete_permanently,
            ignore_errors: true,
            pause_ms: 0,
            ignore_files: scan_settings.ignore_files.clone(),
            consider_empty_files_empty: cli.consider_empty_files_empty,
            dry_run: cli.dry_run,
        };

        let (deleted, failed) = scanner::delete_empty_dirs(
            &mut dirs,
            &delete_settings,
            &|msg, _, _| {
                if !cli.quiet && !cli.json {
                    println!("{}", msg);
                }
            },
            &|_| {},
            &dummy_cancel,
        );

        if !cli.quiet && !cli.json {
            println!(
                "[+] Deletion complete. Deleted: {}, Failed: {}",
                deleted, failed
            );
        }

        if cli.json {
            deletion_summary = Some(JsonDeletionSummary {
                deleted,
                failed,
                dry_run: cli.dry_run,
            });
        }
    }

    if cli.json {
        let report = JsonReport {
            scan_path: path_str,
            empty_directories_found: json_dirs,
            deletion_summary,
        };
        if let Ok(json_str) = serde_json::to_string_pretty(&report) {
            println!("{}", json_str);
        }
    }

    Ok(())
}
