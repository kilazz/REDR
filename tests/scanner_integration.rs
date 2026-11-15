use redr::config;
use redr::scanner::{
    DeleteSettings, NodeStatus, ScanSettings, delete_empty_dirs, is_protected_path,
    scan_empty_dirs, scan_streaming,
};

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
fn the_tree_knows_the_children_and_the_last_sibling() {
    let root = temp_dir("relations");
    // root
    // ├── a
    // │   ├── a1
    // │   └── a2
    // └── b
    fs::create_dir_all(root.join("a").join("a1")).unwrap();
    fs::create_dir_all(root.join("a").join("a2")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let result = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let find = |name: &str| {
        result
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
    };

    // The root has children.
    assert!(find(&root.to_string_lossy()).has_children);

    // "a" has children, "b" does not.
    assert!(find("a").has_children);
    assert!(!find("b").has_children);

    // "a" is followed by "b", so it is not the last of its siblings.
    assert!(!find("a").is_last_sibling);
    assert!(find("b").is_last_sibling);

    // The last child of "a" is "a2".
    assert!(!find("a1").is_last_sibling);
    assert!(find("a2").is_last_sibling);

    // The tree keeps its order: a parent comes before its children.
    let names: Vec<&str> = result.iter().map(|n| n.name.as_str()).collect();
    let pos = |name: &str| names.iter().position(|n| *n == name).unwrap();
    assert!(pos("a") < pos("a1"));
    assert!(pos("a1") < pos("a2"));
    assert!(pos("a2") < pos("b"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_root_of_the_scan_is_never_a_removal_candidate() {
    let root = temp_dir("root_status");
    // Every directory below the root is empty, so the root looks empty too.
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));

    // The parallel walker.
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();
    let root_node = nodes
        .iter()
        .find(|n| n.depth == 0)
        .expect("the root is missing");
    assert_eq!(root_node.status, NodeStatus::Normal);

    // The streaming walker.
    let mut streamed = Vec::new();
    scan_streaming(&root, &settings(), &cancel, &mut |node| streamed.push(node)).unwrap();
    let root_node = streamed
        .iter()
        .find(|n| n.depth == 0)
        .expect("the root is missing");
    assert_eq!(root_node.status, NodeStatus::Normal);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_root_of_the_scan_is_never_deleted() {
    let root = temp_dir("root_delete");
    fs::create_dir_all(root.join("a")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let mut nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    // Even when the scan marks the root as empty, it stays.
    for node in nodes.iter_mut() {
        if node.depth == 0 {
            node.status = NodeStatus::Empty;
        }
    }

    let delete_settings = DeleteSettings {
        move_to_trash: false,
        ignore_errors: true,
        pause_ms: 0,
        ignore_files: vec![],
        consider_empty_files_empty: false,
        dry_run: false,
        protected_paths: vec![],
    };

    let (deleted, failed) = delete_empty_dirs(
        &mut nodes,
        &delete_settings,
        &|_index, _status| {},
        &|_progress| {},
        &cancel,
    );

    assert_eq!(failed, 0);
    assert_eq!(deleted, 1, "only the empty child may be deleted");
    assert!(root.exists(), "the root of the scan was deleted");

    let _ = fs::remove_dir_all(&root);
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
