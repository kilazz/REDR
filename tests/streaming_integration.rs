use redr::scanner::{NodeStatus, ScanSettings, cache, rescan, scan_streaming};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("redr_stream_{tag}_{}", std::process::id()));
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
fn streaming_scan_reports_empty_directories() {
    let root = temp_dir("basic");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("full")).unwrap();
    fs::write(root.join("full").join("file.txt"), b"data").unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let mut found = Vec::new();
    scan_streaming(&root, &settings(), &cancel, &mut |node| {
        found.push(node);
    })
    .unwrap();

    let empty: Vec<&str> = found
        .iter()
        .filter(|n| n.status == NodeStatus::Empty)
        .map(|n| n.path_str.as_str())
        .collect();

    assert!(empty.contains(&root.join("empty").to_string_lossy().as_ref()));
    assert!(!empty.contains(&root.join("full").to_string_lossy().as_ref()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn streaming_scan_marks_orphaned_directories() {
    let root = temp_dir("orphan");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir").join("skip.tmp"), b"data").unwrap();

    let mut s = settings();
    s.ignore_files = vec!["*.tmp".to_string()];

    let cancel = Arc::new(AtomicBool::new(false));
    let mut found = Vec::new();
    scan_streaming(&root, &s, &cancel, &mut |node| {
        found.push(node);
    })
    .unwrap();

    let dir = root.join("dir").to_string_lossy().to_string();
    let node = found.iter().find(|n| n.path_str == dir).unwrap();
    assert_eq!(node.status, NodeStatus::Orphaned);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn incremental_rescan_reuses_unchanged_subtrees() {
    let root = temp_dir("incremental");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));

    let first = rescan(&root, &settings(), None, &cancel).unwrap();
    assert!(first.reused == 0);
    assert!(first.rescanned > 0);

    let second = rescan(&root, &settings(), Some(&first.cache), &cancel).unwrap();
    assert!(second.reused > 0, "nothing was reused");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn incremental_rescan_notices_new_directories() {
    let root = temp_dir("incremental_new");
    fs::create_dir_all(root.join("a")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let first = rescan(&root, &settings(), None, &cancel).unwrap();

    fs::create_dir_all(root.join("a").join("new")).unwrap();

    let second = rescan(&root, &settings(), Some(&first.cache), &cancel).unwrap();
    let paths: Vec<&str> = second.nodes.iter().map(|n| n.path_str.as_str()).collect();
    assert!(
        paths.contains(&root.join("a").join("new").to_string_lossy().as_ref()),
        "new directory missing: {paths:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn settings_fingerprint_changes_with_settings() {
    let a = cache::settings_fingerprint(&settings());
    let mut b = settings();
    b.max_depth = 5;
    assert_ne!(a, cache::settings_fingerprint(&b));
}

#[test]
fn the_streaming_scan_produces_a_properly_ordered_tree() {
    let root = temp_dir("tree_order");
    fs::create_dir_all(root.join("a").join("b")).unwrap();
    fs::create_dir_all(root.join("a.txt")).unwrap();
    fs::create_dir_all(root.join("z")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let mut nodes = Vec::new();
    scan_streaming(&root, &settings(), &cancel, &mut |node| nodes.push(node)).unwrap();

    // The parent has to come before its children.
    for (i, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.path.parent()
            && node.path.as_ref() != root
            && let Some(parent_index) = nodes.iter().position(|n| n.path.as_ref() == parent)
        {
            assert!(
                parent_index < i,
                "{} comes after its parent {}",
                node.path_str,
                parent.display()
            );
        }
    }

    // The children of a directory have to be next to each other.
    for (i, node) in nodes.iter().enumerate() {
        let Some(parent) = node.path.parent() else {
            continue;
        };
        let mut j = i + 1;
        while j < nodes.len() && nodes[j].path.parent() == Some(parent) {
            j += 1;
        }
        for node in &nodes[(i + 1)..j] {
            assert_eq!(
                node.path.parent(),
                Some(parent),
                "{} interrupts the children of {}",
                node.path_str,
                parent.display()
            );
        }
    }

    // The depth has to be relative to the scanned root.
    assert_eq!(nodes.first().map(|n| n.depth), Some(0));
    for node in &nodes {
        if let Some(parent) = node.path.parent()
            && node.path.as_ref() != root
            && let Some(parent_node) = nodes.iter().find(|n| n.path.as_ref() == parent)
        {
            assert_eq!(
                node.depth,
                parent_node.depth + 1,
                "{} has the wrong depth",
                node.path_str
            );
        }
    }

    let _ = fs::remove_dir_all(&root);
}
