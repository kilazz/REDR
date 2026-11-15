pub mod binary_cache;
pub mod cache;
mod delete;
mod filters;
mod incremental;
mod mft;
mod rules;
mod stream;
mod types;
mod walk;

pub use cache::{CachedScan, ScanDelta};
pub use incremental::rescan;
pub use rules::{
    DirFacts, DirOutcome, FileFacts, FileKind, classify_file, dir_facts_from_meta, dir_outcome,
    file_facts_from_meta, is_protected_dir, status_of,
};
pub use stream::scan_streaming;

use ignore::WalkBuilder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wildmatch::WildMatch;

/// Rebuilds a result tree from a cached list of `(path, status)` entries.
pub fn nodes_from_cached(entries: &[(String, i32)], root: &Path) -> Vec<DirectoryNode> {
    let root_depth = root.components().count() as i32;
    let mut nodes: Vec<DirectoryNode> = entries
        .iter()
        .map(|(path, status)| {
            let path_buf = PathBuf::from(path);
            let name = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let depth = (path_buf.components().count() as i32) - root_depth;
            DirectoryNode {
                path: Arc::from(path_buf.clone()),
                name,
                path_str: path.clone(),
                depth,
                status: NodeStatus::from_i32(*status),
                has_children: false,
                is_expanded: true,
                is_last_sibling: false,
                is_hidden: false,
                is_symlink: false,
            }
        })
        .collect();

    // Sort by path components, so the parents come before their children and
    // the tree keeps its order.
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    compute_tree_relationships(&mut nodes);
    nodes
}
pub use delete::delete_empty_dirs;
pub use filters::DirFilter;
pub use mft::scan_empty_dirs_mft;
pub use types::*;
pub use walk::scan_empty_dirs;

/// Returns true when `path` is inside one of the protected entries.
///
/// An entry matches the path itself or any of its ancestors, so protecting
/// `C:\Data` also protects everything below it.
pub fn is_protected_path(path: &Path, protected_paths: &[String]) -> bool {
    if protected_paths.is_empty() {
        return false;
    }
    let path_lower = path.to_string_lossy().to_lowercase();
    protected_paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .any(|entry| {
            let entry = entry
                .trim()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase();
            if entry.is_empty() {
                return false;
            }
            path_lower == entry || path_lower.starts_with(&format!("{}\\", entry))
        })
}

pub(crate) fn lowercase_path(path: &Path) -> String {
    let s = path.to_string_lossy().to_lowercase();
    if s.ends_with('\\') && !s.ends_with(":\\") {
        s.trim_end_matches('\\').to_string()
    } else {
        s
    }
}

pub fn append_deleted_paths(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let config_dir = crate::config::get_config_dir();
    let Some(dir) = config_dir else {
        return;
    };
    let log_dir = dir.join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("deleted.log");
    let mut file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    use std::io::Write;
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    for path in paths {
        let _ = writeln!(file, "[{}] {}", timestamp, path.display());
    }
}

pub fn append_deletion_history(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let Some(dir) = crate::config::get_config_dir() else {
        return;
    };
    let log_dir = dir.join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let history_path = log_dir.join("deleted_history.json");

    let mut existing: Vec<String> = fs::read_to_string(&history_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for path in paths {
        existing.push(path.to_string_lossy().to_string());
    }
    let _ = fs::write(
        &history_path,
        serde_json::to_string_pretty(&existing).unwrap_or_default(),
    );
}

pub fn restore_deleted_paths() -> Result<usize, String> {
    let dir = crate::config::get_config_dir().ok_or_else(|| "Config dir not found".to_string())?;
    let history_path = dir.join("logs").join("deleted_history.json");
    let history: Vec<String> = fs::read_to_string(&history_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if history.is_empty() {
        return Ok(0);
    }

    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    let to_restore: Vec<_> = items
        .into_iter()
        .filter(|item| {
            history
                .iter()
                .any(|p| item.original_path() == std::path::Path::new(p))
        })
        .collect();
    let count = to_restore.len();
    if count > 0 {
        trash::os_limited::restore_all(to_restore).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_file(&history_path);
    Ok(count)
}

/// Restores a single deleted path from the recycle bin.
///
/// Returns the number of restored items (0 or 1).
pub fn restore_single_path(path: &Path) -> Result<usize, String> {
    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    let to_restore: Vec<_> = items
        .into_iter()
        .filter(|item| item.original_path() == path)
        .collect();
    let count = to_restore.len();
    if count > 0 {
        trash::os_limited::restore_all(to_restore).map_err(|e| e.to_string())?;
    }
    Ok(count)
}

/// Helper to correctly identify directories, including Windows NTFS Junctions & Directory Symlinks
fn is_entry_dir(ft: &fs::FileType) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        ft.is_dir() || ft.is_symlink_dir()
    }
    #[cfg(not(windows))]
    {
        ft.is_dir()
    }
}

struct DiscoveredDir {
    path: Arc<Path>,
    depth: i32,
    is_young: bool,
    is_hidden: bool,
    is_symlink: bool,
}

struct WalkBatch {
    dirs: Vec<DiscoveredDir>,
    occupied_parents: FxHashSet<Arc<Path>>,
    minor_occupied_parents: FxHashSet<Arc<Path>>,
    errors: Vec<String>,
}

impl WalkBatch {
    fn is_empty(&self) -> bool {
        self.dirs.is_empty()
            && self.occupied_parents.is_empty()
            && self.minor_occupied_parents.is_empty()
            && self.errors.is_empty()
    }
}

/// Collects walk results in batches and flushes the remainder when the thread ends.
///
/// Without the final flush a scan of a small tree would report nothing: the
/// last batch never reached the 512-entry threshold that triggers a send.
struct BatchSender {
    tx: std::sync::mpsc::Sender<WalkBatch>,
    batch: WalkBatch,
}

impl BatchSender {
    fn new(tx: std::sync::mpsc::Sender<WalkBatch>) -> Self {
        Self {
            tx,
            batch: WalkBatch {
                dirs: Vec::with_capacity(512),
                occupied_parents: FxHashSet::default(),
                minor_occupied_parents: FxHashSet::default(),
                errors: Vec::new(),
            },
        }
    }

    /// Sends the current batch when it grew past the flush threshold.
    fn flush_if_needed(&mut self) {
        if self.batch.dirs.len() >= 512
            || self.batch.occupied_parents.len() >= 512
            || self.batch.minor_occupied_parents.len() >= 512
        {
            self.flush();
        }
    }

    /// Sends the current batch, whatever its size.
    fn flush(&mut self) {
        let batch = std::mem::replace(
            &mut self.batch,
            WalkBatch {
                dirs: Vec::with_capacity(512),
                occupied_parents: FxHashSet::default(),
                minor_occupied_parents: FxHashSet::default(),
                errors: Vec::new(),
            },
        );
        if !batch.is_empty() {
            let _ = self.tx.send(batch);
        }
    }
}

impl Drop for BatchSender {
    fn drop(&mut self) {
        self.flush();
    }
}

fn add_ancestors(included: &mut FxHashSet<Arc<Path>>, start: &Path, root: &Path) {
    let mut parent = start.parent();
    while let Some(par) = parent {
        if !included.insert(Arc::from(par)) {
            break;
        }
        if par == root {
            break;
        }
        parent = par.parent();
    }
}

/// Derives the tree relationships of a depth-first ordered list.
///
/// The list has to be in tree order: a directory, then its children, then the
/// next sibling.
fn compute_tree_relationships(nodes: &mut [DirectoryNode]) {
    if nodes.is_empty() {
        return;
    }

    // Count the children of every parent, so the last one can be found. The
    // siblings of a directory are not next to each other in the list: the
    // children of the first sibling come first.
    let mut children_left: FxHashMap<PathBuf, usize> = FxHashMap::default();
    for node in nodes.iter() {
        if let Some(parent) = node.path.parent() {
            *children_left.entry(parent.to_path_buf()).or_insert(0) += 1;
        }
    }

    for i in 0..nodes.len() {
        let path = nodes[i].path.clone();
        let next = nodes.get(i + 1);

        // The next node is a child when it is deeper and lives under this node.
        let has_children = next
            .map(|n| n.depth > nodes[i].depth && n.path.parent() == Some(path.as_ref()))
            .unwrap_or(false);

        // The node is the last of its siblings when it is the last child of its
        // parent.
        let is_last_sibling = match path.parent() {
            Some(parent) => match children_left.get_mut(parent) {
                Some(left) => {
                    *left -= 1;
                    *left == 0
                }
                None => true,
            },
            None => true,
        };

        nodes[i].has_children = has_children;
        nodes[i].is_last_sibling = is_last_sibling;

        // The root of the scan is only the starting point of the search: it is
        // never a removal candidate, even when every directory below it is
        // empty.
        if nodes[i].depth == 0
            && matches!(nodes[i].status, NodeStatus::Empty | NodeStatus::Orphaned)
        {
            nodes[i].status = NodeStatus::Normal;
        }
    }
}

#[cfg(windows)]
fn is_system_metadata(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    meta.file_attributes() & FILE_ATTRIBUTE_SYSTEM != 0
}

#[cfg(not(windows))]
fn is_system_metadata(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub(crate) fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub(crate) fn is_offline_file(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40000;
    meta.file_attributes() & (FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn is_offline_file(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_hidden_metadata(meta: &fs::Metadata, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden_metadata(_meta: &fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

fn is_metadata_too_young(meta: &fs::Metadata, min_age_hours: u32) -> bool {
    if min_age_hours == 0 {
        return false;
    }
    meta.created()
        .or_else(|_| meta.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() < (min_age_hours as u64 * 3600))
        .unwrap_or(false)
}
