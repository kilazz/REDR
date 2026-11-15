//! Streaming directory scan.
//!
//! Unlike the parallel walker, this scan never holds the whole tree in memory:
//! it walks the directory tree depth-first and hands every finished directory
//! to a callback as soon as its contents are known. Memory use is therefore
//! proportional to the depth of the tree, not to the number of directories.
//!
//! When a previous scan is supplied, the directories whose modification time
//! did not change are not walked again: their previous result is reused.

use super::cache::{CachedScan, dir_stamp};
use super::filters::DirFilter;
use super::rules::{
    FileKind, classify_file, dir_facts_from_meta, dir_outcome, file_facts_from_meta,
    is_protected_dir,
};
use super::types::{DirectoryNode, NodeStatus, ScanSettings};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wildmatch::WildMatch;

/// State of a directory, cached between scans.
pub const STATE_NON_EMPTY: u8 = 0;
pub const STATE_EMPTY: u8 = 1;
pub const STATE_ORPHANED: u8 = 2;
pub const STATE_PROTECTED: u8 = 3;

/// Outcome of scanning one directory.
struct DirOutcome {
    /// The directory itself is empty (no content, all children empty).
    is_empty: bool,
    /// The directory is protected and must never be deleted.
    is_protected: bool,
    /// Something inside this subtree was reported.
    has_report: bool,
}

/// The metadata of a visited directory.
#[derive(Clone, Copy, Debug)]
pub struct DirStats {
    /// The modification time of the directory.
    pub stamp: Option<u64>,
    /// The number of entries in the directory.
    pub child_count: usize,
}

/// Everything a scan produced.
pub struct ScanOutcome {
    /// The directories that were reported.
    pub nodes: Vec<DirectoryNode>,
    /// The metadata of every visited directory.
    pub stats: Vec<(PathBuf, DirStats)>,
}

impl ScanOutcome {
    /// The number of reported directories.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

/// Scans `root` and reports every directory as soon as it is resolved.
pub fn scan_streaming(
    root: &Path,
    settings: &ScanSettings,
    cancel_flag: &Arc<AtomicBool>,
    on_dir: &mut dyn FnMut(DirectoryNode),
) -> Result<usize, String> {
    let outcome = scan_streaming_with_cache(root, settings, cancel_flag, None)?;
    let count = outcome.len();
    for node in outcome.nodes {
        on_dir(node);
    }
    Ok(count)
}

/// Scans `root`, reusing the unchanged subtrees of `previous`.
pub fn scan_streaming_with_cache(
    root: &Path,
    settings: &ScanSettings,
    cancel_flag: &Arc<AtomicBool>,
    previous: Option<&CachedScan>,
) -> Result<ScanOutcome, String> {
    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();
    let dir_filter = DirFilter::new(&settings.ignore_dirs);
    let root_depth = root.components().count() as i32;

    let mut nodes: Vec<DirectoryNode> = Vec::new();
    let mut stats: Vec<(PathBuf, DirStats)> = Vec::new();

    let outcome = scan_dir(
        root,
        root_depth,
        settings,
        &file_matchers,
        &dir_filter,
        cancel_flag,
        previous,
        &mut nodes,
        &mut stats,
    )?;

    // The root itself is never reported as empty, but it shows up in the tree
    // when the scan found something.
    if outcome.has_report {
        nodes.push(DirectoryNode {
            path: Arc::from(root.to_path_buf()),
            name: root.to_string_lossy().into_owned(),
            path_str: root.to_string_lossy().into_owned(),
            depth: 0,
            status: NodeStatus::Normal,
            has_children: true,
            is_expanded: true,
            is_last_sibling: true,
            is_hidden: false,
            is_symlink: false,
        });
    }

    Ok(ScanOutcome { nodes, stats })
}

/// Scans one directory and its children.
#[allow(clippy::too_many_arguments)]
fn scan_dir(
    dir: &Path,
    depth: i32,
    settings: &ScanSettings,
    file_matchers: &[WildMatch],
    dir_filter: &DirFilter,
    cancel_flag: &Arc<AtomicBool>,
    previous: Option<&CachedScan>,
    nodes: &mut Vec<DirectoryNode>,
    stats: &mut Vec<(PathBuf, DirStats)>,
) -> Result<DirOutcome, String> {
    if cancel_flag.load(Ordering::Relaxed) {
        return Err("Operation cancelled by user".to_string());
    }

    let dir_key = dir.to_string_lossy().to_string();

    // Reuse the previous result when the directory and its whole subtree did
    // not change at all.
    if let Some(cache) = previous
        && let Some(current_stamp) = dir_stamp(dir)
        && cache.dir_stamps.get(&dir_key) == Some(&current_stamp)
        && let Some(state) = cache.dir_states.get(&dir_key)
        && cache
            .dir_child_counts
            .get(&dir_key)
            .map(|count| *count == entry_count(dir))
            .unwrap_or(true)
        && subtree_unchanged(cache, &dir_key)
    {
        return Ok(reuse_cached(dir, depth, cache, *state, nodes, stats));
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("Cannot read {}: {}", dir.display(), e);
            return Ok(DirOutcome {
                is_empty: false,
                is_protected: false,
                has_report: false,
            });
        }
    };

    let mut child_dirs: Vec<PathBuf> = Vec::new();
    let mut has_content = false;
    let mut has_minor_content = false;
    let mut entry_total = 0usize;

    for entry in entries.flatten() {
        entry_total += 1;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let meta = entry.metadata().ok();
        let name = entry.file_name().to_string_lossy().into_owned();

        if file_type.is_dir() {
            child_dirs.push(path);
            continue;
        }

        let facts = file_facts_from_meta(&name, meta.as_ref());
        if classify_file(&facts, settings, file_matchers) == FileKind::Content {
            has_content = true;
        } else {
            has_minor_content = true;
        }
    }

    stats.push((
        dir.to_path_buf(),
        DirStats {
            stamp: dir_stamp(dir),
            child_count: entry_total,
        },
    ));

    // A child directory that is not empty (or is protected) makes its parent
    // non-empty too: deleting the parent would delete the child.
    let mut child_reports = false;
    let mut all_children_empty = true;
    for path in &child_dirs {
        let child_outcome = scan_dir(
            path,
            depth + 1,
            settings,
            file_matchers,
            dir_filter,
            cancel_flag,
            previous,
            nodes,
            stats,
        )?;

        child_reports |= child_outcome.has_report;
        if !child_outcome.is_empty || child_outcome.is_protected {
            all_children_empty = false;
        }
    }

    let meta = std::fs::metadata(dir).ok();
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let facts = dir_facts_from_meta(&name, meta.as_ref(), settings.min_age_hours);
    let is_hidden = facts.is_hidden;
    let is_symlink = facts.is_symlink;

    let is_protected = is_protected_dir(dir, &facts, settings, dir_filter);
    let outcome = dir_outcome(
        has_content,
        has_minor_content,
        all_children_empty,
        is_protected,
    );
    let is_empty = outcome.is_empty;
    let is_protected = outcome.is_protected;
    let status = outcome.status();

    let has_report = is_empty || is_protected || child_reports;
    if has_report {
        nodes.push(DirectoryNode {
            path: Arc::from(dir.to_path_buf()),
            name: dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path_str: dir.to_string_lossy().into_owned(),
            depth,
            status,
            has_children: !child_dirs.is_empty(),
            is_expanded: true,
            is_last_sibling: true,
            is_hidden,
            is_symlink,
        });
    }

    Ok(DirOutcome {
        is_empty,
        is_protected,
        has_report,
    })
}

/// Checks that nothing below `dir_key` changed since the cached scan.
///
/// A directory keeps its own modification time when a file inside a
/// subdirectory changes, so the whole subtree has to be checked. The number of
/// entries is checked as well: the modification time alone is not reliable on
/// Windows, where the clock granularity is around 15 ms.
fn subtree_unchanged(cache: &CachedScan, dir_key: &str) -> bool {
    let prefix = format!("{}\\", dir_key.trim_end_matches('\\'));
    for (path, stamp) in &cache.dir_stamps {
        if !path.starts_with(&prefix) {
            continue;
        }
        match dir_stamp(Path::new(path)) {
            Some(current) if current == *stamp => {}
            _ => return false,
        }
        if let Some(count) = cache.dir_child_counts.get(path)
            && *count != entry_count(Path::new(path))
        {
            return false;
        }
    }
    true
}

/// Counts the entries of a directory.
fn entry_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0)
}

/// Replays the previous result of an unchanged directory.
fn reuse_cached(
    dir: &Path,
    depth: i32,
    cache: &CachedScan,
    state: u8,
    nodes: &mut Vec<DirectoryNode>,
    stats: &mut Vec<(PathBuf, DirStats)>,
) -> DirOutcome {
    let prefix = format!("{}\\", dir.to_string_lossy().trim_end_matches('\\'));
    let mut has_report = false;

    // Replay the metadata of the cached subtree.
    for (path, stamp) in &cache.dir_stamps {
        if !path.starts_with(&prefix) {
            continue;
        }
        stats.push((
            PathBuf::from(path),
            DirStats {
                stamp: Some(*stamp),
                child_count: cache.dir_child_counts.get(path).copied().unwrap_or(0),
            },
        ));
    }

    // Replay the cached entries of this subtree.
    for (path, cached_status) in cache.empty_dirs.range(prefix.clone()..) {
        if !path.starts_with(&prefix) {
            break;
        }
        let path_buf = PathBuf::from(path);
        let name = path_buf
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let cached_depth = cache.dir_depths.get(path).copied().unwrap_or(depth);
        nodes.push(DirectoryNode {
            path: Arc::from(path_buf),
            name,
            path_str: path.clone(),
            depth: cached_depth,
            status: NodeStatus::from_i32(*cached_status),
            has_children: false,
            is_expanded: true,
            is_last_sibling: true,
            is_hidden: false,
            is_symlink: false,
        });
        has_report = true;
    }

    DirOutcome {
        is_empty: state == STATE_EMPTY || state == STATE_ORPHANED,
        is_protected: state == STATE_PROTECTED,
        has_report,
    }
}
