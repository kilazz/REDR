//! Scan result cache: keeps the last scan of a root around so it can be
//! reused without walking the tree again, and so a new scan can report what
//! changed since the previous one.
//!
//! The cache is stored in the compact binary format of [`super::binary_cache`];
//! a JSON document would grow to hundreds of megabytes on a large volume.

use super::binary_cache::{self, BinaryCache};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One cached scan of a root directory.
#[derive(Clone, Debug, Default)]
pub struct CachedScan {
    /// The root the scan was made on.
    pub root: String,
    /// Unix timestamp of the scan.
    pub timestamp: u64,
    /// Empty directories found, with their status.
    pub empty_dirs: BTreeMap<String, i32>,
    /// Modification time of every visited directory, used to decide which
    /// subtrees can be reused during an incremental rescan.
    pub dir_stamps: BTreeMap<String, u64>,
    /// The settings fingerprint the scan was made with.
    pub settings_hash: u64,
    /// The state of every visited directory (empty, orphaned, protected, ...).
    pub dir_states: BTreeMap<String, u8>,
    /// The depth of every reported directory, used to rebuild the tree.
    pub dir_depths: BTreeMap<String, i32>,
    /// The number of entries of every visited directory.
    ///
    /// The modification time alone is not enough: on Windows the clock
    /// granularity is around 15 ms, so a change made right after a scan can
    /// keep the same timestamp. The entry count catches that case.
    pub dir_child_counts: BTreeMap<String, usize>,
}

impl CachedScan {
    /// Converts the cache into its binary representation.
    fn to_binary(&self) -> BinaryCache {
        BinaryCache {
            root: self.root.clone(),
            timestamp: self.timestamp,
            settings_hash: self.settings_hash,
            empty_dirs: self.empty_dirs.clone(),
            dir_stamps: self.dir_stamps.clone(),
            dir_states: self.dir_states.clone(),
            dir_depths: self.dir_depths.clone(),
            dir_child_counts: self.dir_child_counts.clone(),
        }
    }

    /// Reads a cache back from its binary representation.
    fn from_binary(cache: BinaryCache) -> Self {
        Self {
            root: cache.root,
            timestamp: cache.timestamp,
            settings_hash: cache.settings_hash,
            empty_dirs: cache.empty_dirs,
            dir_stamps: cache.dir_stamps,
            dir_states: cache.dir_states,
            dir_depths: cache.dir_depths,
            dir_child_counts: cache.dir_child_counts,
        }
    }
}

/// Computes a stable fingerprint of the scan settings.
///
/// Two scans with the same fingerprint can reuse each other's results.
pub fn settings_fingerprint(settings: &crate::scanner::ScanSettings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    settings.ignore_files.hash(&mut hasher);
    settings.ignore_dirs.hash(&mut hasher);
    settings.ignore_hidden.hash(&mut hasher);
    settings.keep_system.hash(&mut hasher);
    settings.min_age_hours.hash(&mut hasher);
    settings.max_depth.hash(&mut hasher);
    settings.consider_empty_files_empty.hash(&mut hasher);
    settings.include_deleted.hash(&mut hasher);
    settings.empty_file_max_size.hash(&mut hasher);
    settings.ignore_files_older_than_hours.hash(&mut hasher);
    settings.protect_symlinks.hash(&mut hasher);
    settings.protected_paths.hash(&mut hasher);
    hasher.finish()
}

/// Returns the modification time of a directory as a Unix timestamp in
/// nanoseconds.
///
/// The nanosecond resolution matters: with a one second resolution a change
/// made right after a scan would still look "unchanged".
pub fn dir_stamp(path: &Path) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as u64)
}

/// Returns the file the cache for the last scanned root is stored in.
fn cache_file() -> Option<PathBuf> {
    crate::config::get_config_dir().map(|d| d.join("scan_cache.bin"))
}

/// Loads the cached scan of `root`, if any.
pub fn load(root: &Path) -> Option<CachedScan> {
    let path = cache_file()?;
    let cache = binary_cache::read_file(&path).ok()?;
    if cache.root.eq_ignore_ascii_case(&root.to_string_lossy()) {
        Some(CachedScan::from_binary(cache))
    } else {
        None
    }
}

/// Stores the result of a scan of `root`.
///
/// Only the empty directories are updated: the directory metadata of a
/// previous run is kept, so the incremental rescan can still reuse it. A stale
/// stamp is harmless, the rescan compares it with the current one.
pub fn store(root: &Path, empty_dirs: &[(String, i32)]) {
    let Some(path) = cache_file() else {
        return;
    };

    let mut cache = load(root).unwrap_or_default();
    cache.root = root.to_string_lossy().into_owned();
    cache.timestamp = chrono::Utc::now().timestamp() as u64;
    cache.empty_dirs = empty_dirs.iter().cloned().collect();

    // Every result must be in the directory table, otherwise the binary
    // format cannot refer to it.
    for path in cache.empty_dirs.keys() {
        cache.dir_states.entry(path.clone()).or_insert(0);
        cache.dir_depths.entry(path.clone()).or_insert(0);
        cache.dir_stamps.entry(path.clone()).or_insert(0);
        cache.dir_child_counts.entry(path.clone()).or_insert(0);
    }

    let _ = binary_cache::write_file(&path, &cache.to_binary());
}

/// Stores a complete cache, including the directory metadata.
pub fn store_cache(cache: &CachedScan) {
    let Some(path) = cache_file() else {
        return;
    };
    let _ = binary_cache::write_file(&path, &cache.to_binary());
}

/// The difference between two scans of the same root.
#[derive(Default, Clone, Debug)]
pub struct ScanDelta {
    /// Directories that became empty since the previous scan.
    pub newly_empty: Vec<String>,
    /// Directories that are no longer empty.
    pub no_longer_empty: Vec<String>,
    /// Directories that were empty in both scans.
    pub unchanged: usize,
}

impl ScanDelta {
    pub fn is_empty(&self) -> bool {
        self.newly_empty.is_empty() && self.no_longer_empty.is_empty()
    }
}

/// Compares the current scan with the cached one.
pub fn diff(cached: &CachedScan, current: &[(String, i32)]) -> ScanDelta {
    let current_map: BTreeMap<&str, i32> = current.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let cached_map: BTreeMap<&str, i32> = cached
        .empty_dirs
        .iter()
        .map(|(p, s)| (p.as_str(), *s))
        .collect();

    let mut delta = ScanDelta::default();
    for path in current_map.keys() {
        if !cached_map.contains_key(path) {
            delta.newly_empty.push((*path).to_string());
        } else {
            delta.unchanged += 1;
        }
    }
    for path in cached_map.keys() {
        if !current_map.contains_key(path) {
            delta.no_longer_empty.push((*path).to_string());
        }
    }
    delta
}

/// Removes the cached scan of `root`.
pub fn clear(root: &Path) {
    if let Some(path) = cache_file()
        && let Ok(cache) = binary_cache::read_file(&path)
        && cache.root.eq_ignore_ascii_case(&root.to_string_lossy())
    {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_new_and_removed_entries() {
        let mut empty_dirs = BTreeMap::new();
        empty_dirs.insert("c:\\a".to_string(), 1);
        empty_dirs.insert("c:\\b".to_string(), 1);
        let cached = CachedScan {
            root: "c:\\".to_string(),
            timestamp: 0,
            empty_dirs,
            ..Default::default()
        };

        let current = vec![("c:\\a".to_string(), 1), ("c:\\c".to_string(), 1)];

        let delta = diff(&cached, &current);
        assert_eq!(delta.newly_empty, vec!["c:\\c".to_string()]);
        assert_eq!(delta.no_longer_empty, vec!["c:\\b".to_string()]);
        assert_eq!(delta.unchanged, 1);
    }
}
