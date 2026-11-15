use redr::config;
use redr::scanner::{NodeStatus, ScanSettings, is_protected_path, scan_empty_dirs};

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("redr_it_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn settings() -> ScanSettings {
    ScanSettings {
        ignore_files: vec![],
        ignore_dirs: vec![],
        ignore_hidden: false,
        keep_system: false,
        min_age_hours: 0,
        max_depth: -1,
        consider_empty_files_empty: false,
        hide_search_errors: true,
        include_deleted: false,
        empty_file_max_size: 0,
        ignore_files_older_than_hours: 0,
        max_results: 0,
        protect_symlinks: true,
        protected_paths: vec![],
    }
}

#[test]
fn finds_only_truly_empty_directories() {
    let root = temp_dir("empty");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("full")).unwrap();
    fs::write(root.join("full").join("file.txt"), b"data").unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let result = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let empty: Vec<String> = result
        .iter()
        .filter(|n| n.status == NodeStatus::Empty)
        .map(|n| n.path_str.clone())
        .collect();

    assert!(empty.contains(&root.join("empty").to_string_lossy().to_string()));
    assert!(!empty.contains(&root.join("full").to_string_lossy().to_string()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ignored_files_make_directory_empty() {
    let root = temp_dir("ignored");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir").join("skip.tmp"), b"data").unwrap();

    let mut s = settings();
    s.ignore_files = vec!["*.tmp".to_string()];

    let cancel = Arc::new(AtomicBool::new(false));
    let result = scan_empty_dirs(&root, &s, &cancel).unwrap();

    assert!(
        result
            .iter()
            .any(|n| n.path_str == root.join("dir").to_string_lossy())
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn protected_paths_are_never_reported_empty() {
    let root = temp_dir("protected");
    fs::create_dir_all(root.join("keep")).unwrap();

    let mut s = settings();
    s.protected_paths = vec![root.join("keep").to_string_lossy().to_string()];

    let cancel = Arc::new(AtomicBool::new(false));
    let result = scan_empty_dirs(&root, &s, &cancel).unwrap();

    let protected = root.join("keep").to_string_lossy().to_string();
    let node = result.iter().find(|n| n.path_str == protected).unwrap();
    assert_eq!(node.status, NodeStatus::Protected);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn max_results_limits_the_output() {
    let root = temp_dir("limited");
    for i in 0..10 {
        fs::create_dir_all(root.join(format!("d{i}"))).unwrap();
    }

    let mut s = settings();
    s.max_results = 3;

    let cancel = Arc::new(AtomicBool::new(false));
    let result = scan_empty_dirs(&root, &s, &cancel).unwrap();

    assert!(result.len() <= 4, "got {} entries", result.len());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn protected_path_matching_handles_prefixes() {
    let protected = vec!["c:\\data".to_string()];
    assert!(is_protected_path(
        std::path::Path::new("c:\\data\\sub"),
        &protected
    ));
    assert!(is_protected_path(
        std::path::Path::new("c:\\data"),
        &protected
    ));
    assert!(!is_protected_path(
        std::path::Path::new("c:\\database"),
        &protected
    ));
    assert!(!is_protected_path(
        std::path::Path::new("d:\\data"),
        &protected
    ));
}

#[test]
fn default_whitelist_covers_system_directories() {
    let whitelist = config::get_default_whitelist();
    assert!(whitelist.to_lowercase().contains("windows"));
    assert!(whitelist.to_lowercase().contains("program files"));
}

#[test]
fn profile_roundtrip() {
    let name = format!("redr_test_profile_{}", std::process::id());
    let settings = config::AppSettings {
        max_depth: 7,
        ..config::AppSettings::default()
    };

    config::save_profile(&name, &settings).expect("save profile");
    let loaded = config::find_profile(&name).expect("profile exists");
    assert_eq!(loaded.settings.max_depth, 7);

    assert!(config::delete_profile(&name));
    assert!(config::find_profile(&name).is_none());
}
