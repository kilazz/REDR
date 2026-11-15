//! Incremental rescan: reuses the results of a previous scan for the
//! subtrees that did not change since then.
//!
//! The cache keeps the modification time of every visited directory. On the
//! next scan a directory whose stamp is unchanged is not walked again: its
//! previous result is replayed instead. A directory whose stamp changed is
//! walked again, and so are all of its ancestors, because a change deep in the
//! tree can make a parent empty or non-empty.

use super::cache::{CachedScan, dir_stamp, settings_fingerprint};
use super::stream::{STATE_EMPTY, STATE_ORPHANED, STATE_PROTECTED, scan_streaming_with_cache};
use super::types::{DirectoryNode, NodeStatus, ScanSettings};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Result of an incremental rescan.
pub struct IncrementalResult {
    /// The directories found in this scan.
    pub nodes: Vec<DirectoryNode>,
    /// Directories reused from the previous scan.
    pub reused: usize,
    /// Directories visited again.
    pub rescanned: usize,
    /// The cache to store for the next run.
    pub cache: CachedScan,
}

/// Rescans `root`, reusing the unchanged subtrees of `previous`.
///
/// `previous` must have been produced with the same settings, otherwise
/// nothing is reused and the whole tree is walked again.
pub fn rescan(
    root: &Path,
    settings: &ScanSettings,
    previous: Option<&CachedScan>,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<IncrementalResult, String> {
    let fingerprint = settings_fingerprint(settings);
    let reusable = previous.filter(|c| {
        c.settings_hash == fingerprint
            && c.root.eq_ignore_ascii_case(&root.to_string_lossy())
            && !c.dir_stamps.is_empty()
    });

    let mut nodes: Vec<DirectoryNode> = Vec::new();
    let mut dir_stamps: BTreeMap<String, u64> = BTreeMap::new();
    let mut dir_states: BTreeMap<String, u8> = BTreeMap::new();
    let mut dir_depths: BTreeMap<String, i32> = BTreeMap::new();
    let mut dir_child_counts: BTreeMap<String, usize> = BTreeMap::new();

    // Count what can be reused by looking at the cache before the scan.
    let reused_before = reusable
        .map(|c| {
            c.dir_stamps
                .keys()
                .filter(|path| {
                    c.dir_stamps
                        .get(*path)
                        .zip(dir_stamp(Path::new(path)))
                        .map(|(cached, current)| cached == &current)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);

    let outcome = scan_streaming_with_cache(root, settings, cancel_flag, reusable)?;

    for (path, stats) in outcome.stats {
        if let Some(stamp) = stats.stamp {
            dir_stamps.insert(path.to_string_lossy().to_string(), stamp);
        }
        dir_child_counts.insert(path.to_string_lossy().to_string(), stats.child_count);
    }

    for node in outcome.nodes {
        let path = node.path_str.clone();
        dir_states.insert(path.clone(), state_of(node.status));
        dir_depths.insert(path.clone(), node.depth);
        nodes.push(node);
    }

    // The directories that were visited again.
    let rescanned = nodes.len().saturating_sub(reused_before);

    let all_dirs: Vec<(String, i32)> = nodes
        .iter()
        .map(|n| (n.path_str.clone(), n.status as i32))
        .collect();

    let cache = CachedScan {
        root: root.to_string_lossy().into_owned(),
        timestamp: chrono::Utc::now().timestamp() as u64,
        all_dirs: all_dirs.iter().cloned().collect(),
        dir_stamps,
        dir_states,
        dir_depths,
        dir_child_counts,
        settings_hash: fingerprint,
    };

    Ok(IncrementalResult {
        nodes,
        reused: reused_before,
        rescanned,
        cache,
    })
}

/// Maps a status to the state stored in the cache.
fn state_of(status: NodeStatus) -> u8 {
    match status {
        NodeStatus::Empty => STATE_EMPTY,
        NodeStatus::Orphaned => STATE_ORPHANED,
        NodeStatus::Protected => STATE_PROTECTED,
        _ => super::stream::STATE_NON_EMPTY,
    }
}
