//! Tests of the presentation logic: the tree building, the search filter and
//! the grouping. The GUI itself is not covered, only the logic behind it.

use redr::scanner::{NodeStatus, ScanSettings, cache, rescan, scan_empty_dirs, scan_streaming};
use redr::view::{GROUP_DRIVE, GROUP_TREE, build_visible_items, csv_escape, serde_json_string};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("redr_ui_{tag}_{}", std::process::id()));
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
fn tree_rows_show_the_scan_results() {
    let root = temp_dir("tree");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("full")).unwrap();
    fs::write(root.join("full").join("f.txt"), b"x").unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let rows = build_visible_items(&nodes, &root, GROUP_TREE, "");
    assert!(rows.iter().any(|r| r.name == "empty"));
    assert!(!rows.iter().any(|r| r.name == "full"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_filters_the_tree_rows() {
    let root = temp_dir("search");
    fs::create_dir_all(root.join("alpha")).unwrap();
    fs::create_dir_all(root.join("beta")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let rows = build_visible_items(&nodes, &root, GROUP_TREE, "alpha");
    assert!(rows.iter().any(|r| r.name == "alpha"));
    assert!(!rows.iter().any(|r| r.name == "beta"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn grouping_by_drive_adds_a_header_row() {
    let root = temp_dir("group");
    fs::create_dir_all(root.join("empty")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let rows = build_visible_items(&nodes, &root, GROUP_DRIVE, "");
    assert!(rows.iter().any(|r| r.is_group_header));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn exported_csv_has_a_header_and_one_row_per_directory() {
    let root = temp_dir("csv");
    fs::create_dir_all(root.join("empty")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let mut csv = String::from("path,status,depth\n");
    for node in &nodes {
        csv.push_str(&format!(
            "{},{},{}\n",
            csv_escape(&node.path_str),
            node.status as i32,
            node.depth
        ));
    }

    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "path,status,depth");
    assert_eq!(lines.len(), nodes.len() + 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn exported_json_escapes_the_paths() {
    let value = serde_json_string("c:\\a \"b\"");
    assert_eq!(value, "\"c:\\\\a \\\"b\\\"\"");
}

#[test]
fn cached_scan_roundtrip_reports_the_same_directories() {
    let root = temp_dir("cache");
    fs::create_dir_all(root.join("empty")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let first = rescan(&root, &settings(), None, &cancel).unwrap();

    let entries: Vec<(String, i32)> = first
        .nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Empty || n.status == NodeStatus::Orphaned)
        .map(|n| (n.path_str.clone(), n.status as i32))
        .collect();
    cache::store(&root, &entries);

    let cached = cache::load(&root).expect("cache exists");
    let empty = cached.empty_dirs();
    assert_eq!(empty.len(), entries.len());
    assert!(empty.contains_key(&root.join("empty").to_string_lossy().to_string()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_tree_rows_are_in_order_and_indented_correctly() {
    let root = temp_dir("rows");
    fs::create_dir_all(root.join("parent").join("child")).unwrap();
    fs::create_dir_all(root.join("sibling")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let mut nodes = Vec::new();
    scan_streaming(&root, &settings(), &cancel, &mut |node| nodes.push(node)).unwrap();

    let rows = build_visible_items(&nodes, &root, GROUP_TREE, "");

    // The root comes first, then its children, then the grandchildren.
    assert_eq!(rows.first().map(|r| r.depth), Some(0));
    assert_eq!(
        rows.first().map(|r| r.name.as_str()),
        Some(&*root.to_string_lossy())
    );

    let depths: Vec<i32> = rows.iter().map(|r| r.depth).collect();
    assert_eq!(depths, vec![0, 1, 2, 1], "unexpected depths: {depths:?}");

    // Every row has as many tree lines as its depth.
    for row in &rows {
        assert_eq!(
            row.tree_lines.len(),
            row.depth as usize,
            "wrong number of tree lines for {}",
            row.name
        );
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_parallel_scan_produces_the_same_tree_shape() {
    let root = temp_dir("parallel");
    fs::create_dir_all(root.join("parent").join("child")).unwrap();
    fs::create_dir_all(root.join("sibling")).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let nodes = scan_empty_dirs(&root, &settings(), &cancel).unwrap();

    let rows = build_visible_items(&nodes, &root, GROUP_TREE, "");

    let depths: Vec<i32> = rows.iter().map(|r| r.depth).collect();
    assert_eq!(depths, vec![0, 1, 2, 1], "unexpected depths: {depths:?}");

    for row in &rows {
        assert_eq!(
            row.tree_lines.len(),
            row.depth as usize,
            "wrong number of tree lines for {}",
            row.name
        );
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_tree_lines_connect_the_branches() {
    use redr::scanner::DirectoryNode;
    use std::sync::Arc;

    // root
    // ├── a
    // │   └── a1
    // └── b
    let make = |path: &str, depth: i32, last: bool| DirectoryNode {
        path: Arc::from(PathBuf::from(path)),
        name: path.to_string(),
        path_str: path.to_string(),
        depth,
        status: NodeStatus::Empty,
        has_children: false,
        is_expanded: true,
        is_last_sibling: last,
        is_hidden: false,
        is_symlink: false,
    };

    let nodes = vec![
        make("c:\\", 0, true),
        make("c:\\a", 1, false),
        make("c:\\a\\a1", 2, true),
        make("c:\\b", 1, true),
    ];

    let rows = build_visible_items(&nodes, std::path::Path::new("c:\\"), GROUP_TREE, "");

    // The root has no lines.
    assert!(rows[0].tree_lines.is_empty());

    // "a" is not the last sibling: the line continues below (├).
    assert_eq!(rows[1].tree_lines, vec![2]);

    // "a1" sits under "a", whose branch continues: a vertical line plus its own elbow.
    assert_eq!(rows[2].tree_lines, vec![1, 3]);

    // "b" is the last child: only its own elbow.
    assert_eq!(rows[3].tree_lines, vec![3]);
}

#[test]
fn the_siblings_share_one_vertical_line() {
    use redr::scanner::DirectoryNode;
    use std::sync::Arc;

    // root
    // ├── one
    // ├── two
    // └── three
    let make = |path: &str, depth: i32, last: bool| DirectoryNode {
        path: Arc::from(PathBuf::from(path)),
        name: path.to_string(),
        path_str: path.to_string(),
        depth,
        status: NodeStatus::Empty,
        has_children: false,
        is_expanded: true,
        is_last_sibling: last,
        is_hidden: false,
        is_symlink: false,
    };

    let nodes = vec![
        make("c:\\", 0, true),
        make("c:\\one", 1, false),
        make("c:\\two", 1, false),
        make("c:\\three", 1, true),
    ];

    let rows = build_visible_items(&nodes, std::path::Path::new("c:\\"), GROUP_TREE, "");

    // Every sibling draws its line in the same column, so the vertical line
    // of the group is continuous.
    let columns: Vec<usize> = rows[1..].iter().map(|r| r.tree_lines.len()).collect();
    assert_eq!(columns, vec![1, 1, 1]);

    // The first two continue downwards, the last one stops.
    assert_eq!(rows[1].tree_lines, vec![2]);
    assert_eq!(rows[2].tree_lines, vec![2]);
    assert_eq!(rows[3].tree_lines, vec![3]);
}

#[test]
fn the_branch_of_a_parent_keeps_its_line() {
    use redr::scanner::DirectoryNode;
    use std::sync::Arc;

    // root
    // ├── a
    // │   ├── a1
    // │   └── a2
    // └── b
    let make = |path: &str, depth: i32, last: bool| DirectoryNode {
        path: Arc::from(PathBuf::from(path)),
        name: path.to_string(),
        path_str: path.to_string(),
        depth,
        status: NodeStatus::Empty,
        has_children: depth < 2,
        is_expanded: true,
        is_last_sibling: last,
        is_hidden: false,
        is_symlink: false,
    };

    let nodes = vec![
        make("c:\\", 0, true),
        make("c:\\a", 1, false),
        make("c:\\a\\a1", 2, false),
        make("c:\\a\\a2", 2, true),
        make("c:\\b", 1, true),
    ];

    let rows = build_visible_items(&nodes, std::path::Path::new("c:\\"), GROUP_TREE, "");

    // The children of "a" keep the line of the group of "a", because "b"
    // still follows below.
    assert_eq!(rows[2].tree_lines, vec![1, 2]);
    assert_eq!(rows[3].tree_lines, vec![1, 3]);

    // "b" is the last one, so its children would not keep the line.
    assert_eq!(rows[4].tree_lines, vec![3]);
}

#[test]
fn the_binary_cache_is_much_smaller_than_json() {
    let mut cache = cache::CachedScan {
        root: "c:\\temp".to_string(),
        timestamp: 1,
        settings_hash: 2,
        ..Default::default()
    };
    for i in 0..2000 {
        let path = format!("c:\\temp\\folder-{i}\\sub");
        cache.dir_states.insert(path.clone(), 1);
        cache.dir_depths.insert(path.clone(), 2);
        cache.dir_stamps.insert(path.clone(), 123_456_789);
        cache.dir_child_counts.insert(path.clone(), 3);
        cache.all_dirs.insert(path, 1);
    }

    let binary = redr::scanner::binary_cache::BinaryCache {
        root: cache.root.clone(),
        timestamp: cache.timestamp,
        settings_hash: cache.settings_hash,
        all_dirs: cache.all_dirs.clone(),
        dir_stamps: cache.dir_stamps.clone(),
        dir_states: cache.dir_states.clone(),
        dir_depths: cache.dir_depths.clone(),
        dir_child_counts: cache.dir_child_counts.clone(),
    };

    let mut buffer = Vec::new();
    redr::scanner::binary_cache::write(&mut buffer, &binary).unwrap();

    let json = serde_json::to_string(&serde_json::json!({
        "root": cache.root,
        "timestamp": cache.timestamp,
        "settings_hash": cache.settings_hash,
        "all_dirs": cache.all_dirs,
        "dir_stamps": cache.dir_stamps,
        "dir_states": cache.dir_states,
        "dir_depths": cache.dir_depths,
        "dir_child_counts": cache.dir_child_counts,
    }))
    .unwrap();
    // The binary format stores every path once, JSON five times.
    assert!(
        buffer.len() * 3 < json.len(),
        "binary {} bytes, json {} bytes",
        buffer.len(),
        json.len()
    );
}
