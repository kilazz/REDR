use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use wildmatch::WildMatch;

#[derive(Clone, Debug)]
pub struct ScanSettings {
    pub ignore_files: Vec<String>,
    pub ignore_dirs: Vec<String>,
    pub ignore_hidden: bool,
    pub keep_system: bool,
    pub min_age_hours: u32,
    pub max_depth: i32,
    pub consider_empty_files_empty: bool,
}

#[derive(Clone, Debug)]
pub struct DirectoryNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: i32,
    pub status: i32,
    pub has_children: bool,
    pub is_expanded: bool,
    pub is_last_sibling: bool,
}

pub fn scan_empty_dirs<F: Fn(&str)>(
    root: &Path,
    settings: &ScanSettings,
    _log: &F,
) -> Result<Vec<DirectoryNode>, String> {
    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();
    let dir_matchers: Vec<String> = settings
        .ignore_dirs
        .iter()
        .map(|s| s.replace('\\', "/").to_lowercase())
        .collect();

    let root_depth = root.components().count() as i32;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false);

    let (tx, rx) = std::sync::mpsc::channel();
    builder.build_parallel().run(|| {
        let tx = tx.clone();
        Box::new(move |result| {
            if let Ok(entry) = result {
                let _ = tx.send(entry);
            }
            ignore::WalkState::Continue
        })
    });
    drop(tx);

    let entries: Vec<ignore::DirEntry> = rx.into_iter().collect();

    use rayon::prelude::*;

    // Compute metadata-heavy properties in parallel
    let mut entry_states: Vec<_> = entries
        .into_par_iter()
        .map(|entry| {
            let p = entry.path().to_path_buf();
            let depth = entry.depth();
            let file_type = entry.file_type();
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let mut is_young_dir = false;
            let mut is_empty_file = false;

            let is_dir = file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false);
            let is_file = file_type.as_ref().map(|ft| ft.is_file()).unwrap_or(false);

            if is_dir && settings.min_age_hours > 0 {
                if let Ok(metadata) = fs::metadata(&p)
                    && let Ok(created) = metadata.created().or_else(|_| metadata.modified())
                    && let Ok(elapsed) = created.elapsed()
                    && elapsed.as_secs() < (settings.min_age_hours as u64 * 3600)
                {
                    is_young_dir = true;
                }
            } else if is_file
                && settings.consider_empty_files_empty
                && let Ok(meta) = fs::metadata(&p)
                && meta.len() == 0
            {
                is_empty_file = true;
            }

            (
                p,
                depth,
                is_dir,
                is_file,
                file_name,
                is_young_dir,
                is_empty_file,
            )
        })
        .collect();

    // Sort by depth descending (bottom-up processing)
    entry_states.sort_by_key(|(_, d, _, _, _, _, _)| std::cmp::Reverse(*d));

    let mut dir_status: std::collections::HashMap<PathBuf, bool> = std::collections::HashMap::new();
    let mut included_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut empty_dirs_found: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for (p, depth, is_dir, is_file, child_name, is_young_dir, is_empty_file) in entry_states {
        if settings.max_depth >= 0 && (depth as i32) > settings.max_depth {
            if let Some(parent) = p.parent() {
                dir_status.insert(parent.to_path_buf(), false);
            }
            continue;
        }

        if is_dir {
            let dir_name = &child_name;
            let full_path_lower = p.to_string_lossy().replace('\\', "/").to_lowercase();

            // It might already be marked as not empty from its children
            let mut is_empty = *dir_status.get(&p).unwrap_or(&true);

            if is_empty
                && (dir_matchers.iter().any(|m| full_path_lower.contains(m))
                    || ((settings.ignore_hidden || settings.keep_system)
                        && dir_name.starts_with('.'))
                    || is_young_dir)
            {
                is_empty = false;
            }

            dir_status.insert(p.clone(), is_empty);

            if is_empty && p != root {
                empty_dirs_found.insert(p.clone());
                included_dirs.insert(p.clone());

                // Add parents up to root to form a tree
                let mut parent = p.parent();
                while let Some(par) = parent {
                    included_dirs.insert(par.to_path_buf());
                    if par == root {
                        break;
                    }
                    parent = par.parent();
                }
            } else if !is_empty
                && p != root
                && let Some(parent) = p.parent()
            {
                dir_status.insert(parent.to_path_buf(), false);
            }
        } else if is_file
            && !is_empty_file
            && !file_matchers.iter().any(|m| m.matches(&child_name))
            && let Some(parent) = p.parent()
        {
            dir_status.insert(parent.to_path_buf(), false);
        }
    }

    let mut sorted_paths: Vec<PathBuf> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::new();
    for p in sorted_paths {
        let is_empty = empty_dirs_found.contains(&p);
        let depth = (p.components().count() as i32) - root_depth;
        let name = if p == root {
            p.to_string_lossy().into_owned()
        } else {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        result.push(DirectoryNode {
            path: p,
            name,
            depth,
            status: if is_empty { 1 } else { 0 },
            has_children: false,
            is_expanded: true,
            is_last_sibling: false,
        });
    }

    for i in 0..result.len() {
        if i + 1 < result.len() && result[i + 1].depth > result[i].depth {
            result[i].has_children = true;
        }

        let mut last = true;
        for j in (i + 1)..result.len() {
            if result[j].depth < result[i].depth {
                break;
            }
            if result[j].depth == result[i].depth {
                last = false;
                break;
            }
        }
        result[i].is_last_sibling = last;
    }

    Ok(result)
}

#[derive(Clone, Debug)]
pub struct DeleteSettings {
    pub move_to_trash: bool,
    pub ignore_errors: bool,
    pub pause_ms: u32,
    pub ignore_files: Vec<String>,
}

pub fn delete_empty_dirs<F: Fn(&str, usize, i32)>(
    nodes: &mut [DirectoryNode],
    settings: &DeleteSettings,
    log: &F,
) -> (usize, usize) {
    let mut deleted = 0;
    let mut failed = 0;

    // Group indices by depth
    let mut depths: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if node.status == 1 {
            depths.entry(node.depth).or_default().push(i);
        }
    }

    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();

    for (_depth, indices) in depths.into_iter().rev() {
        if settings.pause_ms == 0 {
            // Fast path: parallel deletion for independent directories at the same depth
            let results: Vec<_> = indices
                .par_iter()
                .map(|&i| {
                    let dir = &nodes[i].path;

                    // Pre-delete ignored files
                    if !file_matchers.is_empty()
                        && let Ok(entries) = fs::read_dir(dir)
                    {
                        for child in entries.flatten() {
                            let cp = child.path();
                            if cp.is_file() {
                                let child_name =
                                    cp.file_name().unwrap_or_default().to_string_lossy();
                                if file_matchers.iter().any(|m| m.matches(&child_name)) {
                                    let _ = fs::remove_file(&cp);
                                }
                            }
                        }
                    }

                    let res = if settings.move_to_trash {
                        trash::delete(dir).map_err(|e| e.to_string())
                    } else {
                        fs::remove_dir(dir).map_err(|e| e.to_string())
                    };

                    match res {
                        Ok(_) => (i, 2, format!("Deleted: {}", dir.display()), None),
                        Err(e) => (
                            i,
                            4,
                            format!("Failed to delete {}: {}", dir.display(), e),
                            Some(e),
                        ),
                    }
                })
                .collect();

            let mut abort = false;
            for (i, status, msg, _err) in results {
                log(&msg, i, status);
                nodes[i].status = status;
                if status == 2 {
                    deleted += 1;
                } else {
                    failed += 1;
                    if !settings.ignore_errors {
                        log("Aborting deletion due to error.", i, 4);
                        abort = true;
                        break;
                    }
                }
            }
            if abort {
                break;
            }
        } else {
            // Slow path: sequential deletion with pause
            let mut abort = false;
            for &i in &indices {
                let dir = &nodes[i].path;

                // Pre-delete ignored files
                if !file_matchers.is_empty()
                    && let Ok(entries) = fs::read_dir(dir)
                {
                    for child in entries.flatten() {
                        let cp = child.path();
                        if cp.is_file() {
                            let child_name = cp.file_name().unwrap_or_default().to_string_lossy();
                            if file_matchers.iter().any(|m| m.matches(&child_name)) {
                                let _ = fs::remove_file(&cp);
                            }
                        }
                    }
                }

                let res = if settings.move_to_trash {
                    trash::delete(dir).map_err(|e| e.to_string())
                } else {
                    fs::remove_dir(dir).map_err(|e| e.to_string())
                };

                match res {
                    Ok(_) => {
                        log(&format!("Deleted: {}", dir.display()), i, 2);
                        nodes[i].status = 2;
                        deleted += 1;
                    }
                    Err(e) => {
                        log(&format!("Failed to delete {}: {}", dir.display(), e), i, 4);
                        nodes[i].status = 4;
                        failed += 1;
                        if !settings.ignore_errors {
                            log("Aborting deletion due to error.", i, 4);
                            abort = true;
                            break;
                        }
                    }
                }

                if settings.pause_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(settings.pause_ms as u64));
                }
            }
            if abort {
                break;
            }
        }
    }

    (deleted, failed)
}
