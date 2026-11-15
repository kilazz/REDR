//! Presentation logic for the result tree.
//!
//! Kept free of Slint types so it can be unit tested: the GUI only converts
//! [`VisibleNodeData`] into the Slint model.

use crate::scanner::DirectoryNode;
use std::path::Path;

/// Grouping modes for the result list.
pub const GROUP_TREE: i32 = 0;
pub const GROUP_DRIVE: i32 = 1;
pub const GROUP_TOP_LEVEL: i32 = 2;

/// One row of the result tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleNodeData {
    pub name: String,
    pub path: String,
    pub depth: i32,
    pub status: i32,
    pub has_children: bool,
    pub is_expanded: bool,
    pub id: i32,
    pub is_root: bool,
    pub tree_lines: Vec<i32>,
    pub is_hidden: bool,
    pub is_symlink: bool,
    pub is_group_header: bool,
}

/// Escapes a value for a CSV cell.
pub fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Escapes a value as a JSON string, quotes included.
pub fn serde_json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Rebuilds the tree rows, keeping the ancestors of every match.
pub fn rebuild_visible_items(folders: &[DirectoryNode], query: &str) -> Vec<VisibleNodeData> {
    let query_lower = query.to_lowercase();
    let matches: Vec<bool> = folders
        .iter()
        .map(|n| {
            query_lower.is_empty()
                || n.name.to_lowercase().contains(&query_lower)
                || n.path_str.to_lowercase().contains(&query_lower)
        })
        .collect();

    let mut include = vec![true; folders.len()];
    if !query_lower.is_empty() {
        include = vec![false; folders.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (i, node) in folders.iter().enumerate() {
            while stack.len() > node.depth as usize {
                stack.pop();
            }
            if matches[i] {
                include[i] = true;
                for &anc in &stack {
                    include[anc] = true;
                }
            }
            stack.push(i);
        }
    }
    let mut result = Vec::new();
    let mut hide_depth = i32::MAX;
    let mut active_depths: Vec<bool> = Vec::new();

    for (i, node) in folders.iter().enumerate() {
        if !include[i] {
            continue;
        }
        if node.depth <= hide_depth {
            hide_depth = i32::MAX;

            if node.depth > 0 {
                let idx = (node.depth - 1) as usize;
                if active_depths.len() <= idx {
                    active_depths.resize(idx + 1, false);
                }
                active_depths[idx] = !node.is_last_sibling;
            }

            let mut tree_lines_vec = Vec::new();
            for d in 0..(node.depth - 1) {
                let d = d as usize;
                if d < active_depths.len() && active_depths[d] {
                    tree_lines_vec.push(1);
                } else {
                    tree_lines_vec.push(0);
                }
            }
            if node.depth > 0 {
                if node.is_last_sibling {
                    tree_lines_vec.push(3);
                } else {
                    tree_lines_vec.push(2);
                }
            }

            result.push(VisibleNodeData {
                name: node.name.clone(),
                path: node.path_str.clone(),
                depth: node.depth,
                status: node.status as i32,
                has_children: node.has_children,
                is_expanded: node.is_expanded,
                id: i as i32,
                is_root: node.depth == 0,
                tree_lines: tree_lines_vec,
                is_hidden: node.is_hidden,
                is_symlink: node.is_symlink,
                is_group_header: false,
            });

            if !node.is_expanded {
                hide_depth = node.depth;
            }
        }
    }
    result
}

/// Returns the group a path belongs to for the given grouping mode.
pub fn group_key(path: &Path, root: &Path, mode: i32) -> String {
    let raw = path.to_string_lossy();
    match mode {
        GROUP_DRIVE => {
            if let Some(prefix) = path.components().next().and_then(|c| match c {
                std::path::Component::Prefix(p) => {
                    Some(p.as_os_str().to_string_lossy().into_owned())
                }
                _ => None,
            }) {
                return prefix;
            }
            raw.chars().take(2).collect()
        }
        GROUP_TOP_LEVEL => {
            let relative = path.strip_prefix(root).unwrap_or(path);
            relative
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| raw.into_owned())
        }
        _ => String::new(),
    }
}

/// Builds a flat, grouped list: one header row per group, then its entries.
pub fn rebuild_grouped_items(
    folders: &[DirectoryNode],
    root: &Path,
    mode: i32,
    query: &str,
) -> Vec<VisibleNodeData> {
    let query_lower = query.to_lowercase();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();

    for (i, node) in folders.iter().enumerate() {
        if node.depth == 0 {
            continue;
        }
        if !query_lower.is_empty()
            && !node.name.to_lowercase().contains(&query_lower)
            && !node.path_str.to_lowercase().contains(&query_lower)
        {
            continue;
        }
        let key = group_key(&node.path, root, mode);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, items)) => items.push(i),
            None => groups.push((key, vec![i])),
        }
    }

    let mut result = Vec::new();
    for (key, items) in groups {
        result.push(VisibleNodeData {
            name: if key.is_empty() {
                "(root)".to_string()
            } else {
                key.clone()
            },
            path: key.clone(),
            depth: 0,
            status: 0,
            has_children: false,
            is_expanded: true,
            id: -1,
            is_root: false,
            tree_lines: Vec::new(),
            is_hidden: false,
            is_symlink: false,
            is_group_header: true,
        });

        for &i in &items {
            let node = &folders[i];
            result.push(VisibleNodeData {
                name: node.name.clone(),
                path: node.path_str.clone(),
                depth: 1,
                status: node.status as i32,
                has_children: false,
                is_expanded: true,
                id: i as i32,
                is_root: false,
                tree_lines: Vec::new(),
                is_hidden: node.is_hidden,
                is_symlink: node.is_symlink,
                is_group_header: false,
            });
        }
    }
    result
}

/// Builds the rows for the given grouping mode.
pub fn build_visible_items(
    folders: &[DirectoryNode],
    root: &Path,
    mode: i32,
    query: &str,
) -> Vec<VisibleNodeData> {
    if mode == GROUP_TREE {
        rebuild_visible_items(folders, query)
    } else {
        rebuild_grouped_items(folders, root, mode, query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::NodeStatus;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn node(path: &str, depth: i32, status: NodeStatus, last_sibling: bool) -> DirectoryNode {
        let name = match path.rsplit_once('\\') {
            Some((_, last)) if !last.is_empty() => last.to_string(),
            _ => path.to_string(),
        };
        DirectoryNode {
            path: Arc::from(PathBuf::from(path)),
            name,
            path_str: path.to_string(),
            depth,
            status,
            has_children: false,
            is_expanded: true,
            is_last_sibling: last_sibling,
            is_hidden: false,
            is_symlink: false,
        }
    }

    fn sample_tree() -> Vec<DirectoryNode> {
        vec![
            node("c:\\", 0, NodeStatus::Normal, true),
            node("c:\\a", 1, NodeStatus::Empty, false),
            node("c:\\a\\b", 2, NodeStatus::Empty, true),
            node("c:\\c", 1, NodeStatus::Normal, true),
        ]
    }

    #[test]
    fn search_keeps_ancestors_of_matches() {
        let tree = sample_tree();
        let rows = rebuild_visible_items(&tree, "b");
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["c:\\", "a", "b"]);
    }

    #[test]
    fn search_without_match_hides_everything() {
        let tree = sample_tree();
        assert!(rebuild_visible_items(&tree, "zzz").is_empty());
    }

    #[test]
    fn empty_query_keeps_the_whole_tree() {
        let tree = sample_tree();
        let rows = rebuild_visible_items(&tree, "");
        assert_eq!(rows.len(), tree.len());
    }

    #[test]
    fn grouping_by_top_level_folder_adds_headers() {
        let tree = sample_tree();
        let rows = rebuild_grouped_items(&tree, Path::new("c:\\"), GROUP_TOP_LEVEL, "");
        assert!(rows.iter().any(|r| r.is_group_header));
        assert!(rows.iter().any(|r| r.name == "a"));
        assert!(rows.iter().any(|r| r.name == "c"));
    }

    #[test]
    fn grouping_ignores_the_root_row() {
        let tree = sample_tree();
        let rows = rebuild_grouped_items(&tree, Path::new("c:\\"), GROUP_TOP_LEVEL, "");
        assert!(!rows.iter().any(|r| r.name == "\\"));
    }

    #[test]
    fn csv_escapes_quotes_and_commas() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn json_escapes_control_characters() {
        assert_eq!(serde_json_string("plain"), "\"plain\"");
        assert_eq!(serde_json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(serde_json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(serde_json_string("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn group_key_by_drive_uses_the_path_prefix() {
        let key = group_key(Path::new("c:\\folder"), Path::new("c:\\"), GROUP_DRIVE);
        assert!(key.to_lowercase().starts_with("c:"));
    }
}
