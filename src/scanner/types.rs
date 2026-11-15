use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum NodeStatus {
    Normal = 0,
    Empty = 1,
    Deleted = 2,
    Protected = 3,
    Failed = 4,
    Orphaned = 5,
}

impl NodeStatus {
    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => Self::Empty,
            2 => Self::Deleted,
            3 => Self::Protected,
            4 => Self::Failed,
            5 => Self::Orphaned,
            _ => Self::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NodeStatus;

    #[test]
    fn from_i32_maps_known_values() {
        assert_eq!(NodeStatus::from_i32(1), NodeStatus::Empty);
        assert_eq!(NodeStatus::from_i32(2), NodeStatus::Deleted);
        assert_eq!(NodeStatus::from_i32(3), NodeStatus::Protected);
        assert_eq!(NodeStatus::from_i32(4), NodeStatus::Failed);
        assert_eq!(NodeStatus::from_i32(0), NodeStatus::Normal);
        assert_eq!(NodeStatus::from_i32(99), NodeStatus::Normal);
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanSettings {
    pub ignore_files: Vec<String>,
    pub ignore_dirs: Vec<String>,
    pub ignore_hidden: bool,
    pub keep_system: bool,
    pub min_age_hours: u32,
    pub max_depth: i32,
    pub consider_empty_files_empty: bool,
    pub hide_search_errors: bool,
    pub include_deleted: bool,
    pub empty_file_max_size: u64,
    pub ignore_files_older_than_hours: u32,
    pub max_results: usize,
    pub protect_symlinks: bool,
    /// Paths (or path prefixes) that must never be deleted nor reported as empty.
    pub protected_paths: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DirectoryNode {
    pub path: Arc<Path>,
    pub name: String,
    pub path_str: String,
    pub depth: i32,
    pub status: NodeStatus,
    pub has_children: bool,
    pub is_expanded: bool,
    pub is_last_sibling: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
}

#[derive(Clone, Debug)]
pub struct DeleteSettings {
    pub move_to_trash: bool,
    pub ignore_errors: bool,
    pub pause_ms: u32,
    pub ignore_files: Vec<String>,
    pub consider_empty_files_empty: bool,
    pub dry_run: bool,
    /// Paths (or path prefixes) that must never be deleted.
    pub protected_paths: Vec<String>,
}
