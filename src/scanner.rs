use ignore::WalkBuilder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub hide_search_errors: bool,
}

#[derive(Clone, Debug)]
pub struct DirectoryNode {
    pub path: Arc<Path>,
    pub name: String,
    pub path_str: String,
    pub depth: i32,
    pub status: i32, // 0: Normal, 1: Empty, 2: Deleted, 3: Protected, 4: Failed
    pub has_children: bool,
    pub is_expanded: bool,
    pub is_last_sibling: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
}

/// Advanced directory filtering supporting exact folder segment matching and wildcards
#[derive(Clone, Debug)]
pub struct DirFilter {
    exact_names: FxHashSet<String>,
    wildcards: Vec<WildMatch>,
}

impl DirFilter {
    pub fn new(ignore_dirs: &[String]) -> Self {
        let mut exact_names = FxHashSet::default();
        let mut wildcards = Vec::new();

        for s in ignore_dirs {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                continue;
            }
            let s_normalized = trimmed.replace('\\', "/").to_lowercase();
            // Patterns with wildcards or path separators use wildcard matching
            if s_normalized.contains('*')
                || s_normalized.contains('?')
                || s_normalized.contains('/')
            {
                let pattern = if s_normalized.contains('*') || s_normalized.contains('?') {
                    s_normalized
                } else {
                    format!("*{}*", s_normalized)
                };
                wildcards.push(WildMatch::new(&pattern));
            } else {
                // Exact directory names (e.g., ".git", "node_modules")
                exact_names.insert(s_normalized);
            }
        }

        Self {
            exact_names,
            wildcards,
        }
    }

    /// Evaluates if a given path matches the ignore rules
    pub fn is_match(&self, path: &Path, full_path_lower: &str) -> bool {
        // 1. O(depth) exact segment matching (e.g. any path inside .git or node_modules)
        if !self.exact_names.is_empty() {
            for comp in path.components() {
                let comp_str = comp.as_os_str().to_string_lossy().to_lowercase();
                if self.exact_names.contains(&comp_str) {
                    return true;
                }
            }
        }

        // 2. Fallback to path and wildcard patterns
        if !self.wildcards.is_empty() && self.wildcards.iter().any(|m| m.matches(full_path_lower)) {
            return true;
        }

        false
    }
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
    errors: Vec<String>,
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

fn compute_tree_relationships(nodes: &mut [DirectoryNode]) {
    if nodes.is_empty() {
        return;
    }

    for i in 0..nodes.len() {
        if i + 1 < nodes.len() && nodes[i + 1].depth > nodes[i].depth {
            nodes[i].has_children = true;
        }
    }

    let mut seen_depths: FxHashSet<i32> = FxHashSet::default();
    let mut prev_depth = i32::MAX;

    for i in (0..nodes.len()).rev() {
        let depth = nodes[i].depth;
        if depth < prev_depth {
            seen_depths.retain(|&d| d <= depth);
        }

        nodes[i].is_last_sibling = seen_depths.insert(depth);

        prev_depth = depth;
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

fn is_directory_protected(
    p: &Path,
    is_hidden: bool,
    is_young_dir: bool,
    is_system: bool,
    settings: &ScanSettings,
    dir_filter: &DirFilter,
) -> bool {
    let full_path_lower = p.to_string_lossy().replace('\\', "/").to_lowercase();
    let matches_ignore_dir = dir_filter.is_match(p, &full_path_lower);
    let matches_hidden = settings.ignore_hidden && is_hidden;
    let matches_system = settings.keep_system && is_system;

    matches_ignore_dir || matches_hidden || matches_system || is_young_dir
}

pub fn scan_empty_dirs(
    root: &Path,
    settings: &ScanSettings,
    log: &dyn Fn(&str),
    cancel_flag: &Arc<AtomicBool>,
) -> Result<Vec<DirectoryNode>, String> {
    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();

    let dir_filter = DirFilter::new(&settings.ignore_dirs);
    let root_depth = root.components().count() as i32;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false);

    let (tx, rx) = std::sync::mpsc::channel::<WalkBatch>();
    let cancel_walk = cancel_flag.clone();
    let file_matchers_ref = &file_matchers;
    let settings_ref = settings;

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let cancel_inner = cancel_walk.clone();
        let mut batch = WalkBatch {
            dirs: Vec::with_capacity(512),
            occupied_parents: FxHashSet::default(),
            errors: Vec::new(),
        };

        Box::new(move |result| {
            if cancel_inner.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }

            match result {
                Ok(entry) => {
                    let p = Arc::<Path>::from(entry.path());
                    let depth = entry.depth() as i32;
                    let file_type = entry.file_type();
                    let file_name = entry.file_name().to_string_lossy();

                    let is_dir = file_type.as_ref().map(is_entry_dir).unwrap_or(false);
                    let is_symlink = file_type
                        .as_ref()
                        .map(|ft| ft.is_symlink())
                        .unwrap_or(false);
                    let meta = entry.metadata().ok();

                    if is_dir {
                        let is_hidden = meta
                            .as_ref()
                            .map(|m| is_hidden_metadata(m, &file_name))
                            .unwrap_or_else(|| file_name.starts_with('.'));
                        let is_young = meta
                            .as_ref()
                            .map(|m| is_metadata_too_young(m, settings_ref.min_age_hours))
                            .unwrap_or(false);

                        batch.dirs.push(DiscoveredDir {
                            path: p,
                            depth,
                            is_young,
                            is_hidden,
                            is_symlink,
                        });
                    } else {
                        let is_ignored = file_matchers_ref.iter().any(|m| m.matches(&file_name));
                        let is_empty_file = settings_ref.consider_empty_files_empty
                            && meta.as_ref().map(|m| m.len() == 0).unwrap_or(false);

                        if !is_ignored
                            && !is_empty_file
                            && let Some(parent) = p.parent()
                        {
                            batch.occupied_parents.insert(Arc::<Path>::from(parent));
                        }
                    }
                }
                Err(err) => {
                    batch.errors.push(err.to_string());
                }
            }

            if batch.dirs.len() >= 512 || batch.occupied_parents.len() >= 512 {
                let send_batch = std::mem::replace(
                    &mut batch,
                    WalkBatch {
                        dirs: Vec::with_capacity(512),
                        occupied_parents: FxHashSet::default(),
                        errors: Vec::new(),
                    },
                );
                let _ = tx.send(send_batch);
            }

            ignore::WalkState::Continue
        })
    });
    drop(tx);

    let mut dir_states: Vec<DiscoveredDir> = Vec::new();
    let mut occupied_parents: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut walk_errors: Vec<String> = Vec::new();

    for mut batch in rx {
        dir_states.append(&mut batch.dirs);
        occupied_parents.extend(batch.occupied_parents);
        walk_errors.append(&mut batch.errors);
    }

    if cancel_flag.load(Ordering::Relaxed) {
        return Err("Operation cancelled by user".to_string());
    }

    if !walk_errors.is_empty() {
        if settings.hide_search_errors {
            log(&format!(
                "[!] {} item(s) skipped due to access errors.",
                walk_errors.len()
            ));
        } else {
            for e in &walk_errors {
                log(&format!("[!] Access error: {}", e));
            }
        }
    }

    dir_states.sort_by_key(|d| std::cmp::Reverse(d.depth));

    let mut dir_status: FxHashMap<Arc<Path>, bool> = FxHashMap::default();
    let mut included_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut empty_dirs_found: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut protected_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut hidden_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut symlink_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();

    for parent in occupied_parents {
        dir_status.insert(parent, false);
    }

    for dir in dir_states {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }

        if settings.max_depth >= 0 && dir.depth > settings.max_depth {
            if let Some(parent) = dir.path.parent() {
                dir_status.insert(Arc::from(parent), false);
            }
            continue;
        }

        let mut is_empty = *dir_status.get(&dir.path).unwrap_or(&true);
        let mut is_protected = false;

        if dir.is_hidden {
            hidden_dirs.insert(dir.path.clone());
        }
        if dir.is_symlink {
            symlink_dirs.insert(dir.path.clone());
        }

        let is_system = fs::metadata(&dir.path)
            .as_ref()
            .map(is_system_metadata)
            .unwrap_or(false);

        if is_empty
            && is_directory_protected(
                &dir.path,
                dir.is_hidden,
                dir.is_young,
                is_system,
                settings,
                &dir_filter,
            )
        {
            is_empty = false;
            is_protected = true;
        }

        dir_status.insert(dir.path.clone(), is_empty);

        if dir.path.as_ref() != root {
            if is_empty {
                empty_dirs_found.insert(dir.path.clone());
                included_dirs.insert(dir.path.clone());
                add_ancestors(&mut included_dirs, &dir.path, root);
            } else {
                if let Some(parent) = dir.path.parent() {
                    dir_status.insert(Arc::from(parent), false);
                }

                if is_protected {
                    protected_dirs.insert(dir.path.clone());
                    included_dirs.insert(dir.path.clone());
                    add_ancestors(&mut included_dirs, &dir.path, root);
                }
            }
        }
    }

    let mut sorted_paths: Vec<Arc<Path>> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::with_capacity(sorted_paths.len());
    for p in sorted_paths {
        let is_empty = empty_dirs_found.contains(&p);
        let is_protected = protected_dirs.contains(&p);
        let depth = (p.components().count() as i32) - root_depth;
        let name = if p.as_ref() == root {
            p.to_string_lossy().into_owned()
        } else {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };

        result.push(DirectoryNode {
            path: p.clone(),
            name,
            path_str: p.to_string_lossy().into_owned(),
            depth,
            status: if is_empty {
                1
            } else if is_protected {
                3
            } else {
                0
            },
            has_children: false,
            is_expanded: true,
            is_last_sibling: false,
            is_hidden: hidden_dirs.contains(&p),
            is_symlink: symlink_dirs.contains(&p),
        });
    }

    compute_tree_relationships(&mut result);
    Ok(result)
}

#[cfg(target_os = "windows")]
pub fn scan_empty_dirs_mft(
    root: &Path,
    settings: &ScanSettings,
    log: &dyn Fn(&str),
    cancel_flag: &Arc<AtomicBool>,
) -> Result<Vec<DirectoryNode>, String> {
    use ntfs_reader::file_info::FileInfo;
    use ntfs_reader::mft::Mft;
    use ntfs_reader::volume::Volume;
    use std::path::Component;

    log("[*] Initializing Direct MFT Scan...");

    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();

    let dir_filter = DirFilter::new(&settings.ignore_dirs);
    let canonical_path = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    let mut drive_letter_opt = None;
    if let Some(Component::Prefix(prefix_component)) = canonical_path.components().next() {
        use std::path::Prefix;
        match prefix_component.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                drive_letter_opt = Some((drive as char).to_string());
            }
            Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => {
                return Err("Direct MFT Scan is not supported on network UNC shares.".to_string());
            }
            Prefix::DeviceNS(_) | Prefix::Verbatim(_) => {
                return Err("Direct MFT Scan is not supported on device volumes.".to_string());
            }
        }
    }

    let drive_letter = drive_letter_opt
        .or_else(|| {
            root.components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .map(|s| s.trim_end_matches('\\').trim_end_matches(':').to_string())
        })
        .ok_or_else(|| "Failed to parse volume drive letter from path".to_string())?;

    if drive_letter.len() != 1 || !drive_letter.chars().next().unwrap().is_ascii_alphabetic() {
        return Err(format!(
            "Invalid drive letter: '{}'. Direct MFT Scan requires a local drive (e.g., C:).",
            drive_letter
        ));
    }

    let volume_path = format!("\\\\.\\{}:", drive_letter);
    let volume = Volume::new(&volume_path).map_err(|e| {
        format!(
            "Failed to open NTFS volume (Requires Administrator privileges): {}",
            e
        )
    })?;

    let mft = Mft::new(volume)
        .map_err(|e| format!("Failed to initialize Master File Table parser: {}", e))?;

    log("[*] Reading Master File Table records...");

    let lowercase_path = |path: &Path| -> String {
        let s = path.to_string_lossy().to_lowercase();
        if s.ends_with('\\') && !s.ends_with(":\\") {
            s.trim_end_matches('\\').to_string()
        } else {
            s
        }
    };

    let root_lower_str = lowercase_path(root);
    let mut all_dirs: FxHashMap<String, PathBuf> = FxHashMap::default();
    let mut occupied_dirs: FxHashSet<String> = FxHashSet::default();

    // Fast boundary check ensuring paths belong strictly within root scope
    let is_within_root = |p: &str| -> bool {
        if p == root_lower_str {
            true
        } else if root_lower_str.ends_with('\\') {
            p.starts_with(&root_lower_str)
        } else {
            p.len() > root_lower_str.len()
                && p.as_bytes()[root_lower_str.len()] == b'\\'
                && p.starts_with(&root_lower_str)
        }
    };

    for file in mft.files() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }

        let info = FileInfo::new(&mft, &file);
        let raw_path = info.path;

        let path_with_drive = if raw_path.starts_with(format!("{}:\\", drive_letter)) {
            raw_path
        } else {
            let clean = raw_path.strip_prefix("\\").unwrap_or(&raw_path);
            PathBuf::from(format!("{}:\\", drive_letter)).join(clean)
        };

        let p_lower = lowercase_path(&path_with_drive);

        // Skip any records lying completely outside the requested root
        if !is_within_root(&p_lower) {
            continue;
        }

        if info.is_directory {
            all_dirs.insert(p_lower, path_with_drive);
        } else {
            let child_name = path_with_drive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let is_ignored = file_matchers.iter().any(|m| m.matches(&child_name));

            if !is_ignored {
                let mut current_path: &str = &p_lower;
                while let Some(idx) = current_path.rfind('\\') {
                    let mut parent_path = &current_path[..idx];
                    if parent_path.ends_with(':') {
                        parent_path = &current_path[..idx + 1];
                    }

                    if parent_path.is_empty() || occupied_dirs.contains(parent_path) {
                        break;
                    }
                    occupied_dirs.insert(parent_path.to_string());

                    if parent_path == root_lower_str || parent_path.ends_with(":\\") {
                        break;
                    }
                    current_path = parent_path;
                }
            }
        }
    }

    let mut empty_dirs_found: FxHashSet<String> = FxHashSet::default();
    let mut included_dirs: FxHashSet<PathBuf> = FxHashSet::default();
    let root_depth = root.components().count() as i32;

    included_dirs.insert(root.to_path_buf());
    all_dirs
        .entry(root_lower_str.clone())
        .or_insert_with(|| root.to_path_buf());

    for (p_lower, p) in &all_dirs {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }

        if !occupied_dirs.contains(p_lower)
            && p_lower.starts_with(&root_lower_str)
            && p_lower != &root_lower_str
        {
            empty_dirs_found.insert(p_lower.clone());
            included_dirs.insert(p.clone());

            let mut current_path: &str = p_lower;
            while let Some(idx) = current_path.rfind('\\') {
                let mut parent_path = &current_path[..idx];
                if parent_path.ends_with(':') {
                    parent_path = &current_path[..idx + 1];
                }

                if parent_path == root_lower_str || !parent_path.starts_with(&root_lower_str) {
                    break;
                }

                if let Some(exact_parent) = all_dirs.get(parent_path) {
                    if !included_dirs.insert(exact_parent.clone()) {
                        break;
                    }
                } else if !included_dirs.insert(PathBuf::from(parent_path)) {
                    break;
                }

                if parent_path.ends_with(":\\") {
                    break;
                }
                current_path = parent_path;
            }
        }
    }

    log("[*] Verifying MFT directory states...");

    let mut true_empty: FxHashSet<String> = FxHashSet::default();
    let mut empty_vec: Vec<String> = empty_dirs_found.into_iter().collect();
    empty_vec.sort_by_key(|p| std::cmp::Reverse(p.matches('\\').count()));

    for p_lower in empty_vec {
        if let Some(exact_p) = all_dirs.get(&p_lower) {
            let mut is_truly_empty = true;
            if let Ok(entries) = fs::read_dir(exact_p) {
                for entry in entries.flatten() {
                    let child_path = entry.path();
                    let child_name = entry.file_name().to_string_lossy().into_owned();

                    if child_path.is_dir() {
                        let child_lower = lowercase_path(&child_path);
                        if !true_empty.contains(&child_lower) {
                            is_truly_empty = false;
                            break;
                        }
                    } else {
                        let is_ignored = file_matchers.iter().any(|m| m.matches(&child_name));
                        let is_empty_file = settings.consider_empty_files_empty
                            && entry.metadata().map(|m| m.len() == 0).unwrap_or(false);

                        if !is_ignored && !is_empty_file {
                            is_truly_empty = false;
                            break;
                        }
                    }
                }
            } else {
                is_truly_empty = false;
            }

            if is_truly_empty {
                true_empty.insert(p_lower);
            }
        }
    }

    let mut sorted_paths: Vec<PathBuf> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::with_capacity(sorted_paths.len());
    for p in sorted_paths {
        let p_lower = lowercase_path(&p);
        let mut is_empty = true_empty.contains(&p_lower);
        let depth = (p.components().count() as i32) - root_depth;
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let meta = fs::metadata(&p).ok();
        let is_hidden = meta
            .as_ref()
            .map(|m| is_hidden_metadata(m, &name))
            .unwrap_or(false);
        let is_system = meta.as_ref().map(is_system_metadata).unwrap_or(false);
        let is_young = meta
            .as_ref()
            .map(|m| is_metadata_too_young(m, settings.min_age_hours))
            .unwrap_or(false);
        let is_symlink = fs::symlink_metadata(&p)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        let mut is_protected = false;
        if is_empty
            && is_directory_protected(&p, is_hidden, is_young, is_system, settings, &dir_filter)
        {
            is_empty = false;
            is_protected = true;
        }

        result.push(DirectoryNode {
            path: Arc::from(p.clone()),
            name: if p.as_path() == root {
                root.to_string_lossy().into_owned()
            } else {
                name
            },
            path_str: p.to_string_lossy().into_owned(),
            depth,
            status: if is_empty {
                1
            } else if is_protected {
                3
            } else {
                0
            },
            has_children: false,
            is_expanded: true,
            is_last_sibling: false,
            is_hidden,
            is_symlink,
        });
    }

    compute_tree_relationships(&mut result);
    log(&format!(
        "[+] Direct MFT Scan complete. Found {} empty directories.",
        true_empty.len()
    ));

    Ok(result)
}

#[derive(Clone, Debug)]
pub struct DeleteSettings {
    pub move_to_trash: bool,
    pub ignore_errors: bool,
    pub pause_ms: u32,
    pub ignore_files: Vec<String>,
    pub consider_empty_files_empty: bool,
    pub dry_run: bool,
}

fn verify_subtree_empty(
    dir: &Path,
    settings: &DeleteSettings,
    file_matchers: &[WildMatch],
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;

    for entry in entries.flatten() {
        let child_path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;

        let is_dir = is_entry_dir(&file_type);

        if is_dir {
            if !file_type.is_symlink() {
                verify_subtree_empty(&child_path, settings, file_matchers)?;
            }
        } else {
            let child_name = child_path.file_name().unwrap_or_default().to_string_lossy();
            let is_ignored = file_matchers.iter().any(|m| m.matches(&child_name));
            let is_empty_file = settings.consider_empty_files_empty
                && entry.metadata().map(|m| m.len() == 0).unwrap_or(false);

            if !is_ignored && !is_empty_file {
                return Err(format!(
                    "Subtree contains non-empty file: {}",
                    child_path.display()
                ));
            }
        }
    }
    Ok(())
}

fn clean_leaf_dir_files(
    dir: &Path,
    settings: &DeleteSettings,
    file_matchers: &[WildMatch],
) -> Result<(), String> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let cp = entry.path();
            let ft = entry.file_type().map_err(|e| e.to_string())?;

            let is_dir = is_entry_dir(&ft);

            if !is_dir {
                let name = cp.file_name().unwrap_or_default().to_string_lossy();
                let is_ignored = file_matchers.iter().any(|m| m.matches(&name));
                let is_empty = settings.consider_empty_files_empty
                    && entry.metadata().map(|m| m.len() == 0).unwrap_or(false);

                if (is_ignored || is_empty) && !settings.dry_run {
                    #[cfg(windows)]
                    {
                        if let Ok(meta) = fs::metadata(&cp) {
                            let mut perms = meta.permissions();
                            if perms.readonly() {
                                #[allow(clippy::permissions_set_readonly_false)]
                                perms.set_readonly(false);
                                let _ = fs::set_permissions(&cp, perms);
                            }
                        }
                    }
                    let _ = fs::remove_file(&cp);
                }
            }
        }
    }
    Ok(())
}

fn perform_directory_delete(
    dir: &Path,
    settings: &DeleteSettings,
    file_matchers: &[WildMatch],
) -> Result<(), String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    let is_symlink = meta.is_symlink();

    if is_symlink {
        return if settings.dry_run {
            Ok(())
        } else {
            fs::remove_dir(dir)
                .or_else(|_| fs::remove_file(dir))
                .map_err(|e| e.to_string())
        };
    }

    clean_leaf_dir_files(dir, settings, file_matchers)?;

    if settings.dry_run {
        return Ok(());
    }

    if settings.move_to_trash {
        trash::delete(dir).map_err(|e| e.to_string())
    } else {
        fs::remove_dir(dir).map_err(|e| e.to_string())
    }
}

pub fn delete_empty_dirs<F, P>(
    nodes: &mut [DirectoryNode],
    settings: &DeleteSettings,
    log: &F,
    progress_cb: &P,
    cancel_flag: &Arc<AtomicBool>,
) -> (usize, usize)
where
    F: Fn(&str, usize, i32),
    P: Fn(f32),
{
    let mut deleted = 0;
    let mut failed = 0;

    let mut empty_indices: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.status == 1)
        .map(|(i, _)| i)
        .collect();

    if empty_indices.is_empty() {
        return (0, 0);
    }

    empty_indices.sort_by_key(|&i| nodes[i].depth);

    let mut root_delete_targets: Vec<usize> = Vec::new();
    let mut covered_paths_set: FxHashSet<PathBuf> = FxHashSet::default();
    let mut covered_paths: Vec<PathBuf> = Vec::new();

    for &i in &empty_indices {
        let path = &nodes[i].path;
        let mut is_covered = false;
        let mut ancestor = path.parent();

        while let Some(anc) = ancestor {
            if covered_paths_set.contains(anc) {
                is_covered = true;
                break;
            }
            ancestor = anc.parent();
        }

        if !is_covered {
            root_delete_targets.push(i);
            covered_paths_set.insert(path.to_path_buf());
            covered_paths.push(path.to_path_buf());
        }
    }

    let file_matchers: Vec<WildMatch> = settings
        .ignore_files
        .iter()
        .map(|s| WildMatch::new(s))
        .collect();

    let mut batch_success = false;

    if settings.move_to_trash && !settings.dry_run && settings.pause_ms == 0 {
        log(
            "[*] Verifying subtree integrity for batch Recycle Bin deletion...",
            0,
            0,
        );

        let mut subtree_valid = true;
        for &i in &root_delete_targets {
            if let Err(e) = verify_subtree_empty(&nodes[i].path, settings, &file_matchers) {
                log(
                    &format!(
                        "[!] Cannot batch recycle {}: {}",
                        nodes[i].path.display(),
                        e
                    ),
                    i,
                    4,
                );
                subtree_valid = false;
                break;
            }
        }

        if subtree_valid {
            match trash::delete_all(&covered_paths) {
                Ok(_) => {
                    for (progress_idx, &i) in empty_indices.iter().enumerate() {
                        nodes[i].status = 2;
                        log(
                            &format!("Deleted (Trash): {}", nodes[i].path.display()),
                            i,
                            2,
                        );
                        progress_cb((progress_idx + 1) as f32 / empty_indices.len() as f32);
                    }
                    deleted = empty_indices.len();
                    batch_success = true;
                }
                Err(err) => {
                    log(
                        &format!(
                            "[!] Batch recycling failed: {}. Falling back to bottom-up deletion...",
                            err
                        ),
                        0,
                        0,
                    );
                }
            }
        }
    }

    if !batch_success {
        let mut processed_items = 0;
        let mut depths: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
        let total_items = empty_indices.len();

        for &i in &empty_indices {
            depths.entry(nodes[i].depth).or_default().push(i);
        }

        for (_depth, indices) in depths.into_iter().rev() {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }

            if !settings.move_to_trash && settings.pause_ms == 0 && indices.len() > 1 {
                let results: Vec<_> = {
                    let nodes_ref: &[DirectoryNode] = nodes;
                    let cancel_inner = cancel_flag.clone();
                    indices
                        .par_iter()
                        .map(|&i| {
                            if cancel_inner.load(Ordering::Relaxed) {
                                return (i, 4, "Cancelled".to_string());
                            }
                            let dir = &nodes_ref[i].path;
                            if settings.dry_run {
                                return (
                                    i,
                                    2,
                                    format!("[Dry-Run] Would delete: {}", dir.display()),
                                );
                            }

                            match perform_directory_delete(dir, settings, &file_matchers) {
                                Ok(_) => (i, 2, format!("Deleted: {}", dir.display())),
                                Err(e) => {
                                    (i, 4, format!("Failed to delete {}: {}", dir.display(), e))
                                }
                            }
                        })
                        .collect()
                };

                let mut abort = false;
                for (i, status, msg) in results {
                    if cancel_flag.load(Ordering::Relaxed) {
                        abort = true;
                        break;
                    }
                    if msg == "Cancelled" {
                        continue;
                    }

                    processed_items += 1;
                    progress_cb(processed_items as f32 / total_items as f32);

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
                let mut abort = false;
                for &i in &indices {
                    if cancel_flag.load(Ordering::Relaxed) {
                        abort = true;
                        break;
                    }

                    let dir = nodes[i].path.clone();

                    if settings.dry_run {
                        log(&format!("[Dry-Run] Would delete: {}", dir.display()), i, 2);
                        nodes[i].status = 2;
                        deleted += 1;
                        processed_items += 1;
                        progress_cb(processed_items as f32 / total_items as f32);
                        continue;
                    }

                    match perform_directory_delete(&dir, settings, &file_matchers) {
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

                    processed_items += 1;
                    progress_cb(processed_items as f32 / total_items as f32);

                    if settings.pause_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(
                            settings.pause_ms as u64,
                        ));
                    }
                }
                if abort {
                    break;
                }
            }
        }
    }

    (deleted, failed)
}
