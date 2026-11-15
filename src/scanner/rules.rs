//! The rules that decide whether a directory is empty, orphaned or protected.
//!
//! All three scan back-ends (the parallel walker, the streaming walker and
//! the MFT reader) feed their findings into this module, so they cannot drift
//! apart: they only differ in *where* the information comes from.

use super::filters::DirFilter;
use super::types::{NodeStatus, ScanSettings};
use super::{is_offline_file, is_protected_path, is_reparse_point};
use std::path::Path;
use wildmatch::WildMatch;

/// How a file affects the emptiness of the directory that holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// The file is real content: the directory is not empty.
    Content,
    /// The file is ignored, empty or too small: the directory stays a removal
    /// candidate, but it is flagged as "orphaned".
    Minor,
}

/// The facts about a file that the rules need.
#[derive(Clone, Debug)]
pub struct FileFacts {
    /// The file name, matched against the ignore patterns.
    pub name: String,
    /// The file size in bytes, when known.
    pub size: Option<u64>,
    /// The modification time as a Unix timestamp in seconds.
    pub modified: Option<i64>,
    /// Whether the file is a cloud placeholder that is not on disk yet.
    pub is_offline: bool,
}

/// Decides how a file affects its directory.
pub fn classify_file(
    facts: &FileFacts,
    settings: &ScanSettings,
    matchers: &[WildMatch],
) -> FileKind {
    let ignored_by_name = matchers.iter().any(|m| m.matches(&facts.name));
    let is_empty_file = settings.consider_empty_files_empty
        && facts
            .size
            .map(|size| size <= settings.empty_file_max_size)
            .unwrap_or(false);
    let is_old = settings.ignore_files_older_than_hours > 0
        && facts
            .modified
            .map(|modified| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                (now - modified).max(0) as u64
                    >= settings.ignore_files_older_than_hours as u64 * 3600
            })
            .unwrap_or(false);

    let ignored = ignored_by_name || is_old;

    // A placeholder that is not on disk yet still counts as content: the user
    // has a file there, even if the bytes live in the cloud.
    if (!ignored && !is_empty_file) || facts.is_offline {
        FileKind::Content
    } else {
        FileKind::Minor
    }
}

/// The facts about a directory that the rules need.
#[derive(Clone, Copy, Debug)]
pub struct DirFacts {
    /// Whether the directory is hidden (or starts with a dot).
    pub is_hidden: bool,
    /// Whether the directory was created recently.
    pub is_young: bool,
    /// Whether the directory belongs to the operating system.
    pub is_system: bool,
    /// Whether the directory is a symlink, junction or another reparse point.
    pub is_symlink: bool,
}

/// Decides whether a directory must be left alone.
pub fn is_protected_dir(
    path: &Path,
    facts: &DirFacts,
    settings: &ScanSettings,
    dir_filter: &DirFilter,
) -> bool {
    let matches_filter = dir_filter.is_match(
        path,
        &path.to_string_lossy().replace('\\', "/").to_lowercase(),
    );
    let matches_hidden = settings.ignore_hidden && facts.is_hidden;
    let matches_system = settings.keep_system && facts.is_system;
    let matches_symlink = settings.protect_symlinks && facts.is_symlink;
    let matches_whitelist = is_protected_path(path, &settings.protected_paths);

    // A folder that is younger than the configured age is left alone. The age
    // filter is off by default, in which case `is_young` is never set.
    matches_filter
        || matches_hidden
        || matches_system
        || matches_symlink
        || matches_whitelist
        || facts.is_young
}

/// The status a directory gets from the scan findings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirOutcome {
    /// The directory holds no content at all.
    pub is_empty: bool,
    /// The directory holds only ignored or empty files.
    pub is_orphaned: bool,
    /// The directory must not be deleted.
    pub is_protected: bool,
}

impl DirOutcome {
    /// Combines the findings into the status shown in the tree.
    pub fn status(&self) -> NodeStatus {
        status_of(self.is_empty, self.is_orphaned, self.is_protected)
    }
}

/// The status of a directory whose emptiness is already known.
pub fn status_of(is_empty: bool, is_orphaned: bool, is_protected: bool) -> NodeStatus {
    if is_protected {
        NodeStatus::Protected
    } else if is_empty {
        if is_orphaned {
            NodeStatus::Orphaned
        } else {
            NodeStatus::Empty
        }
    } else {
        NodeStatus::Normal
    }
}

/// Combines the findings about a directory into its outcome.
///
/// `has_content` tells whether the directory holds a file that counts,
/// `all_children_empty` whether every directory below it is empty itself.
pub fn dir_outcome(
    has_content: bool,
    has_minor_content: bool,
    all_children_empty: bool,
    is_protected: bool,
) -> DirOutcome {
    let is_empty = !has_content && all_children_empty;
    DirOutcome {
        is_empty,
        is_orphaned: is_empty && has_minor_content,
        is_protected,
    }
}

/// Collects the file facts from the file system.
pub fn file_facts_from_meta(name: &str, meta: Option<&std::fs::Metadata>) -> FileFacts {
    FileFacts {
        name: name.to_string(),
        size: meta.map(|m| m.len()),
        modified: meta.and_then(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        }),
        is_offline: meta.map(is_offline_file).unwrap_or(false),
    }
}

/// Collects the directory facts from the file system.
pub fn dir_facts_from_meta(
    name: &str,
    meta: Option<&std::fs::Metadata>,
    min_age_hours: u32,
) -> DirFacts {
    DirFacts {
        is_hidden: meta
            .map(|m| super::is_hidden_metadata(m, name))
            .unwrap_or_else(|| name.starts_with('.')),
        is_young: meta
            .map(|m| super::is_metadata_too_young(m, min_age_hours))
            .unwrap_or(false),
        is_system: meta.map(super::is_system_metadata).unwrap_or(false),
        is_symlink: meta.map(is_reparse_point).unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ScanSettings {
        ScanSettings::default()
    }

    #[test]
    fn a_normal_file_is_content() {
        let facts = FileFacts {
            name: "data.txt".to_string(),
            size: Some(100),
            modified: None,
            is_offline: false,
        };
        assert_eq!(classify_file(&facts, &settings(), &[]), FileKind::Content);
    }

    #[test]
    fn an_ignored_file_is_minor() {
        let mut s = settings();
        s.ignore_files = vec!["*.tmp".to_string()];
        let matchers = vec![WildMatch::new("*.tmp")];

        let facts = FileFacts {
            name: "skip.tmp".to_string(),
            size: Some(100),
            modified: None,
            is_offline: false,
        };
        assert_eq!(classify_file(&facts, &s, &matchers), FileKind::Minor);
    }

    #[test]
    fn a_small_file_is_minor_when_enabled() {
        let mut s = settings();
        s.consider_empty_files_empty = true;
        s.empty_file_max_size = 10;

        let facts = FileFacts {
            name: "small.bin".to_string(),
            size: Some(5),
            modified: None,
            is_offline: false,
        };
        assert_eq!(classify_file(&facts, &s, &[]), FileKind::Minor);
    }

    #[test]
    fn an_offline_placeholder_is_content() {
        let mut s = settings();
        s.consider_empty_files_empty = true;
        s.empty_file_max_size = 100;

        let facts = FileFacts {
            name: "cloud.bin".to_string(),
            size: Some(0),
            modified: None,
            is_offline: true,
        };
        assert_eq!(classify_file(&facts, &s, &[]), FileKind::Content);
    }

    #[test]
    fn an_old_file_is_minor() {
        let mut s = settings();
        s.ignore_files_older_than_hours = 1;

        let facts = FileFacts {
            name: "old.txt".to_string(),
            size: Some(100),
            modified: Some(0),
            is_offline: false,
        };
        assert_eq!(classify_file(&facts, &s, &[]), FileKind::Minor);
    }

    #[test]
    fn outcome_maps_to_the_status() {
        let empty = dir_outcome(false, false, true, false);
        assert_eq!(empty.status(), NodeStatus::Empty);

        let orphaned = dir_outcome(false, true, true, false);
        assert_eq!(orphaned.status(), NodeStatus::Orphaned);

        let protected = dir_outcome(false, false, true, true);
        assert_eq!(protected.status(), NodeStatus::Protected);

        let normal = dir_outcome(true, false, true, false);
        assert_eq!(normal.status(), NodeStatus::Normal);
    }

    #[test]
    fn a_non_empty_child_makes_the_parent_non_empty() {
        let outcome = dir_outcome(false, false, false, false);
        assert!(!outcome.is_empty);
    }
}
