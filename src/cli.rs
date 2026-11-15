use clap::Parser;
use redr::scanner::{self, DeleteSettings, ScanSettings};
use redr::sys::is_admin;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(index = 1)]
    pub path: Option<String>,

    #[arg(short, long)]
    pub quiet: bool,

    #[arg(short, long)]
    pub delete: bool,

    /// Simulate the deletion without removing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Actually delete (turns the safe dry-run default off)
    #[arg(long, conflicts_with = "dry_run")]
    pub execute: bool,

    #[arg(long)]
    pub json: bool,

    #[arg(long, help = "Force CLI execution mode")]
    pub cli: bool,

    #[arg(long, help = "Force graphical interface execution mode")]
    pub gui: bool,

    #[arg(long)]
    pub mft_scan: bool,

    #[arg(long, default_value_t = -1)]
    pub max_depth: i32,

    #[arg(long)]
    pub delete_permanently: bool,

    #[arg(long, default_value = "desktop.ini,Thumbs.db,.DS_Store")]
    pub ignore_files: String,

    #[arg(
        long,
        default_value = "System Volume Information,RECYCLER,Recycled,$RECYCLE.BIN,.git,.svn,.hg,node_modules"
    )]
    pub ignore_dirs: String,

    #[arg(long, default_value_t = true)]
    pub ignore_hidden: bool,

    #[arg(long, default_value_t = true)]
    pub keep_system: bool,

    #[arg(long, default_value_t = 0)]
    pub min_age_hours: u32,

    #[arg(long, default_value_t = true)]
    pub consider_empty_files_empty: bool,

    #[arg(long, default_value_t = true)]
    pub hide_search_errors: bool,

    /// Paths that must never be deleted (comma or semicolon separated)
    #[arg(long, default_value = "")]
    pub protect_paths: String,

    /// Never delete permanently: everything goes to the recycle bin
    #[arg(long, default_value_t = false)]
    pub trash_only: bool,
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

pub fn run_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path_str = cli
        .path
        .ok_or("Path is required for CLI/Quiet/JSON mode.")?;
    let path = PathBuf::from(&path_str);

    if !path.exists() {
        return Err(format!("Directory does not exist: {}", path.display()).into());
    }
    if !path.is_dir() {
        return Err(format!("Path is not a directory: {}", path.display()).into());
    }

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
        include_deleted: false,
        empty_file_max_size: 0,
        ignore_files_older_than_hours: 0,
        max_results: 0,
        protect_symlinks: true,
        protected_paths: cli
            .protect_paths
            .split([',', ';'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    };

    if !cli.quiet && !cli.json {
        println!("[*] Scanning: {:?}", path);
    }

    let use_mft = cli.mft_scan && cfg!(target_os = "windows") && is_admin();

    let mut dirs = if use_mft {
        #[cfg(target_os = "windows")]
        match scanner::scan_empty_dirs_mft(&path, &scan_settings, &dummy_cancel) {
            Ok(res) => res,
            Err(_) => scanner::scan_empty_dirs(&path, &scan_settings, &dummy_cancel)?,
        }
        #[cfg(not(target_os = "windows"))]
        scanner::scan_empty_dirs(&path, &scan_settings, &dummy_cancel)?
    } else {
        scanner::scan_empty_dirs(&path, &scan_settings, &dummy_cancel)?
    };

    let empty_count = dirs
        .iter()
        .filter(|d| {
            d.status == scanner::NodeStatus::Empty || d.status == scanner::NodeStatus::Orphaned
        })
        .count();
    if !cli.quiet && !cli.json {
        println!("[+] Found {} empty directories.", empty_count);
    }

    let mut json_dirs = Vec::new();
    if cli.json {
        for d in &dirs {
            let status_str = match d.status {
                scanner::NodeStatus::Empty => "empty",
                scanner::NodeStatus::Orphaned => "orphaned",
                scanner::NodeStatus::Protected => "protected",
                _ => "normal",
            };
            json_dirs.push(JsonDir {
                path: d.path_str.clone(),
                status: status_str,
            });
        }
    }

    let mut deletion_summary = None;
    if cli.delete && empty_count > 0 {
        // Deleting is opt-in: without --execute the run only simulates.
        let dry_run = !cli.execute;
        if dry_run && !cli.quiet && !cli.json {
            println!(
                "[i] Simulation mode: nothing will be deleted. Pass --execute to really delete."
            );
        }
        let delete_settings = DeleteSettings {
            move_to_trash: !cli.delete_permanently || cli.trash_only,
            ignore_errors: true,
            pause_ms: 0,
            ignore_files: scan_settings.ignore_files.clone(),
            consider_empty_files_empty: cli.consider_empty_files_empty,
            dry_run,
            protected_paths: scan_settings.protected_paths.clone(),
        };

        let (deleted, failed) = scanner::delete_empty_dirs(
            &mut dirs,
            &delete_settings,
            &|_, _| {},
            &|_| {},
            &dummy_cancel,
        );

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
                dry_run,
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
