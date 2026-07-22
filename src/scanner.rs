use ignore::WalkBuilder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use slint::SharedString;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wildmatch::WildMatch;

/// Holds the configuration parameters for the directory scanner pipeline
#[derive(Clone, Debug)]
pub struct ScanSettings {
    pub ignore_files: Vec<String>,
    pub ignore_dirs: Vec<String>,
    pub ignore_hidden: bool,
    pub keep_system: bool,
    pub min_age_hours: u32,
    pub max_depth: i32,
    pub consider_empty_files_empty: bool,
    /// If true, permission-denied and filesystem errors are collapsed into a single summary
    pub hide_search_errors: bool,
}

/// Represents a single filesystem node in the tree hierarchy
#[derive(Clone, Debug)]
pub struct DirectoryNode {
    pub path: Arc<Path>,
    pub name: SharedString,
    pub path_str: SharedString,
    pub depth: i32,
    pub status: i32, // 0: Normal, 1: Empty, 2: Deleted, 3: Protected, 4: Failed
    pub has_children: bool,
    pub is_expanded: bool,
    pub is_last_sibling: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
}

/// Messages emitted during the parallel file walk
enum WalkMsg {
    Entry(ignore::DirEntry),
    Error(String),
}

/// Recursively climbs up the tree to mark all parent directories as included in the visible model
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

/// Evaluates node relationships to set parent connection rendering flags
fn compute_tree_relationships(nodes: &mut [DirectoryNode]) {
    for i in 0..nodes.len() {
        if i + 1 < nodes.len() && nodes[i + 1].depth > nodes[i].depth {
            nodes[i].has_children = true;
        }

        let mut last = true;
        for j in (i + 1)..nodes.len() {
            if nodes[j].depth < nodes[i].depth {
                break;
            }
            if nodes[j].depth == nodes[i].depth {
                last = false;
                break;
            }
        }
        nodes[i].is_last_sibling = last;
    }
}

/// Detects the Windows-specific "System" file attribute
#[cfg(windows)]
fn is_system_dir(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    fs::metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_SYSTEM != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_system_dir(_path: &Path) -> bool {
    false
}

/// Identifies hidden directories natively on Windows or via dotfile naming on Unix
#[cfg(windows)]
fn is_hidden_dir(path: &Path, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    fs::metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_hidden_dir(_path: &Path, name: &str) -> bool {
    name.starts_with('.')
}

/// Determines if a directory is too young to be processed based on age threshold
fn is_dir_too_young(p: &Path, min_age_hours: u32) -> bool {
    if min_age_hours == 0 {
        return false;
    }
    fs::metadata(p)
        .ok()
        .and_then(|m| m.created().or_else(|_| m.modified()).ok())
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() < (min_age_hours as u64 * 3600))
        .unwrap_or(false)
}

/// Assesses whether a directory qualifies for preservation under configured protection policies
fn is_directory_protected(
    p: &Path,
    is_hidden: bool,
    is_young_dir: bool,
    settings: &ScanSettings,
    dir_matchers: &[WildMatch],
) -> bool {
    let matches_ignore_dir = if dir_matchers.is_empty() {
        false
    } else {
        let full_path_lower = p.to_string_lossy().replace('\\', "/").to_lowercase();
        dir_matchers.iter().any(|m| m.matches(&full_path_lower))
    };
    let matches_hidden = settings.ignore_hidden && is_hidden;
    let matches_system = settings.keep_system && is_system_dir(p);

    matches_ignore_dir || matches_hidden || matches_system || is_young_dir
}

/// Standard file walk scanner powered by WalkBuilder and parallel Rayon processing
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

    let dir_matchers: Vec<WildMatch> = settings
        .ignore_dirs
        .iter()
        .map(|s| {
            let s_normalized = s.replace('\\', "/").to_lowercase();
            let pattern = if s_normalized.contains('*') || s_normalized.contains('?') {
                s_normalized
            } else {
                format!("*{}*", s_normalized)
            };
            WildMatch::new(&pattern)
        })
        .collect();

    let root_depth = root.components().count() as i32;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false);

    let (tx, rx) = std::sync::mpsc::channel::<WalkMsg>();
    let cancel_walk = cancel_flag.clone();
    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let cancel_inner = cancel_walk.clone();
        Box::new(move |result| {
            if cancel_inner.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            match result {
                Ok(entry) => {
                    let _ = tx.send(WalkMsg::Entry(entry));
                }
                Err(err) => {
                    let _ = tx.send(WalkMsg::Error(err.to_string()));
                }
            }
            ignore::WalkState::Continue
        })
    });
    drop(tx);

    let mut entries: Vec<ignore::DirEntry> = Vec::new();
    let mut walk_errors: Vec<String> = Vec::new();
    for msg in rx {
        match msg {
            WalkMsg::Entry(e) => entries.push(e),
            WalkMsg::Error(e) => walk_errors.push(e),
        }
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

    let file_matchers_ref = &file_matchers;
    let settings_ref = settings;

    // Split processing of dirs vs files to drastically reduce memory usage
    let (mut dir_states, occupied_parents) = entries
        .into_par_iter()
        .fold(
            || (Vec::new(), FxHashSet::default()),
            |(mut dirs, mut occupied), entry| {
                let p = Arc::<Path>::from(entry.path());
                let depth = entry.depth();
                let file_type = entry.file_type();
                let file_name = entry.file_name().to_string_lossy().into_owned();

                let is_dir = file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false);
                let is_file = file_type.as_ref().map(|ft| ft.is_file()).unwrap_or(false);

                let is_hidden = is_hidden_dir(&p, &file_name);
                let is_symlink =
                    file_type
                        .as_ref()
                        .map(|ft| ft.is_symlink())
                        .unwrap_or_else(|| {
                            fs::symlink_metadata(&p)
                                .map(|m| m.file_type().is_symlink())
                                .unwrap_or(false)
                        });

                if is_dir {
                    let is_young_dir = if settings_ref.min_age_hours > 0 {
                        is_dir_too_young(&p, settings_ref.min_age_hours)
                    } else {
                        false
                    };
                    dirs.push((p, depth, file_name, is_young_dir, is_hidden, is_symlink));
                } else {
                    let is_ignored = file_matchers_ref.iter().any(|m| m.matches(&file_name));

                    // Use cached metadata from WalkBuilder (DirEntry) instead of fs::metadata
                    let is_empty_file = if is_file && settings_ref.consider_empty_files_empty {
                        entry.metadata().map(|m| m.len() == 0).unwrap_or(false)
                    } else {
                        false
                    };

                    if !is_ignored
                        && !is_empty_file
                        && let Some(parent) = p.parent()
                    {
                        occupied.insert(Arc::<Path>::from(parent));
                    }
                }

                (dirs, occupied)
            },
        )
        .reduce(
            || (Vec::new(), FxHashSet::default()),
            |(mut d1, mut o1), (mut d2, o2)| {
                d1.append(&mut d2);
                o1.extend(o2);
                (d1, o1)
            },
        );

    // Sort bottom-up so lower child nodes are analyzed first
    dir_states.sort_by_key(|(_, d, _, _, _, _)| std::cmp::Reverse(*d));

    let mut dir_status: FxHashMap<Arc<Path>, bool> = FxHashMap::default();
    let mut included_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut empty_dirs_found: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut protected_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut hidden_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut symlink_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();

    // Mark directory branches containing active files as non-empty from the start
    for parent in occupied_parents {
        dir_status.insert(parent, false);
    }

    for (p, depth, _child_name, is_young_dir, is_hidden, is_symlink) in dir_states {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }

        if settings.max_depth >= 0 && (depth as i32) > settings.max_depth {
            if let Some(parent) = p.parent() {
                dir_status.insert(Arc::from(parent), false);
            }
            continue;
        }

        let mut is_empty = *dir_status.get(&p).unwrap_or(&true);
        let mut is_protected = false;

        if is_hidden {
            hidden_dirs.insert(p.clone());
        }
        if is_symlink {
            symlink_dirs.insert(p.clone());
        }

        if is_empty && is_directory_protected(&p, is_hidden, is_young_dir, settings, &dir_matchers)
        {
            is_empty = false;
            is_protected = true;
        }

        dir_status.insert(p.clone(), is_empty);

        if p.as_ref() != root {
            if is_empty {
                empty_dirs_found.insert(p.clone());
                included_dirs.insert(p.clone());
                add_ancestors(&mut included_dirs, &p, root);
            } else {
                if let Some(parent) = p.parent() {
                    dir_status.insert(Arc::from(parent), false);
                }

                if is_protected {
                    protected_dirs.insert(p.clone());
                    included_dirs.insert(p.clone());
                    add_ancestors(&mut included_dirs, &p, root);
                }
            }
        }
    }

    let mut sorted_paths: Vec<Arc<Path>> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::new();
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
            name: SharedString::from(name),
            path_str: SharedString::from(p.to_string_lossy().into_owned()),
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

// ==========================================
// EXPERIMENTAL WINDOWS NTFS DIRECT MFT SCAN
// ==========================================
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

    let dir_matchers: Vec<WildMatch> = settings
        .ignore_dirs
        .iter()
        .map(|s| {
            let s_normalized = s.replace('\\', "/").to_lowercase();
            let pattern = if s_normalized.contains('*') || s_normalized.contains('?') {
                s_normalized
            } else {
                format!("*{}*", s_normalized)
            };
            WildMatch::new(&pattern)
        })
        .collect();

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
                return Err(
                    "Direct MFT Scan is not supported on this type of device volume.".to_string(),
                );
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
            "Invalid drive letter extracted: '{}'. Direct MFT Scan requires a local disk drive (e.g., C:).",
            drive_letter
        ));
    }

    let volume_path = format!("\\\\.\\{}:", drive_letter);

    let volume = Volume::new(&volume_path).map_err(|e| {
        format!(
            "Failed to open physical NTFS volume (Requires Administrator privileges): {}",
            e
        )
    })?;

    let mft = Mft::new(volume)
        .map_err(|e| format!("Failed to initialize Master File Table parser: {}", e))?;

    log("[*] Reading Master File Table directly into system memory...");

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

    for file in mft.files() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }

        let info = FileInfo::new(&mft, &file);
        let raw_path = info.path.clone();
        let raw_str = raw_path.to_string_lossy();

        let path_with_drive = if raw_str.starts_with('\\') && !raw_str.starts_with("\\\\.\\") {
            let clean_path = raw_path.strip_prefix("\\").unwrap_or(&raw_path);
            PathBuf::from(format!("{}:\\", drive_letter)).join(clean_path)
        } else if !raw_str.contains(':') {
            PathBuf::from(format!("{}:\\", drive_letter)).join(&raw_path)
        } else {
            raw_path.clone()
        };

        let path_str = path_with_drive.to_string_lossy();
        let target_prefix = format!("{}:\\", drive_letter);
        let target_lower = target_prefix.to_lowercase();
        let path_lower = path_str.to_lowercase();

        let p = if let Some(pos) = path_lower.find(&target_lower) {
            PathBuf::from(&path_str[pos..])
        } else {
            path_with_drive
        };

        let p_lower = lowercase_path(&p);

        if info.is_directory {
            all_dirs.insert(p_lower, p.clone());
        } else {
            let child_name = p.file_name().unwrap_or_default().to_string_lossy();
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

                    if parent_path.ends_with(":\\") {
                        break;
                    }
                    current_path = parent_path;
                }
            }
        }
    }

    log("[*] Reconstructing hierarchical tree paths and filtering occupied branches...");

    let mut empty_dirs_found: FxHashSet<String> = FxHashSet::default();
    let mut included_dirs: FxHashSet<PathBuf> = FxHashSet::default();
    let root_depth = root.components().count() as i32;

    included_dirs.insert(root.to_path_buf());

    if !all_dirs.contains_key(&root_lower_str) {
        all_dirs.insert(root_lower_str.clone(), root.to_path_buf());
    }

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
                } else {
                    if !included_dirs.insert(PathBuf::from(parent_path)) {
                        break;
                    }
                }

                if parent_path.ends_with(":\\") {
                    break;
                }
                current_path = parent_path;
            }
        }
    }

    log("[*] Performing hybrid accuracy check to eliminate MFT false positives...");

    let mut true_empty: FxHashSet<String> = FxHashSet::default();
    let mut false_positives: FxHashSet<String> = FxHashSet::default();

    let mut empty_vec: Vec<String> = empty_dirs_found.into_iter().collect();
    empty_vec.sort_by_key(|p| std::cmp::Reverse(p.matches('\\').count()));

    for p_lower in empty_vec {
        if let Some(exact_p) = all_dirs.get(&p_lower) {
            if false_positives.contains(&p_lower) {
                if let Some(idx) = p_lower.rfind('\\') {
                    let mut parent = &p_lower[..idx];
                    if parent.ends_with(':') {
                        parent = &p_lower[..idx + 1];
                    }
                    false_positives.insert(parent.to_string());
                }
                continue;
            }

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
                        let is_empty_file = if settings.consider_empty_files_empty {
                            entry.metadata().map(|m| m.len() == 0).unwrap_or(false)
                        } else {
                            false
                        };

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
                true_empty.insert(p_lower.clone());
            } else {
                false_positives.insert(p_lower.clone());
                if let Some(idx) = p_lower.rfind('\\') {
                    let mut parent = &p_lower[..idx];
                    if parent.ends_with(':') {
                        parent = &p_lower[..idx + 1];
                    }
                    false_positives.insert(parent.to_string());
                }
            }
        }
    }

    let mut sorted_paths: Vec<PathBuf> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::new();

    for p in sorted_paths {
        let p_lower = lowercase_path(&p);
        let mut is_empty = true_empty.contains(&p_lower);

        let depth = (p.components().count() as i32) - root_depth;
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let is_hidden = is_hidden_dir(&p, &name);
        let is_symlink = fs::symlink_metadata(&p)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        let mut is_protected = false;

        if is_empty
            && is_directory_protected(
                &p,
                is_hidden,
                is_dir_too_young(&p, settings.min_age_hours),
                settings,
                &dir_matchers,
            )
        {
            is_empty = false;
            is_protected = true;
        }

        result.push(DirectoryNode {
            path: Arc::from(p.clone()),
            name: SharedString::from(if p.as_path() == root {
                root.to_string_lossy().into_owned()
            } else {
                name
            }),
            path_str: SharedString::from(p.to_string_lossy().into_owned()),
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
        "[+] Direct MFT Scan complete. Found {} truly empty directories.",
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

fn clean_and_verify_empty(
    dir: &Path,
    settings: &DeleteSettings,
    file_matchers: &[WildMatch],
) -> Result<bool, String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if meta.is_symlink() {
        return Ok(true);
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for child in entries.flatten() {
            let cp = child.path();
            let meta = fs::symlink_metadata(&cp);
            let is_symlink = meta.as_ref().map(|m| m.is_symlink()).unwrap_or(false);

            if is_symlink || cp.is_file() {
                let child_name = cp.file_name().unwrap_or_default().to_string_lossy();
                let is_ignored = file_matchers.iter().any(|m| m.matches(&child_name));
                let is_empty_file = !is_symlink
                    && settings.consider_empty_files_empty
                    && fs::metadata(&cp).map(|m| m.len() == 0).unwrap_or(false);

                if (is_ignored || is_empty_file) && !settings.dry_run {
                    let _ = fs::remove_file(&cp).or_else(|_| fs::remove_dir(&cp));
                }
            }
        }
    }

    match fs::read_dir(dir) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                Ok(true)
            } else {
                Err("Directory is not empty (contains non-ignored files)".to_string())
            }
        }
        Err(e) => Err(format!("Failed to verify directory: {}", e)),
    }
}

fn perform_directory_delete(
    dir: &Path,
    settings: &DeleteSettings,
    file_matchers: &[WildMatch],
) -> Result<(), String> {
    match clean_and_verify_empty(dir, settings, file_matchers) {
        Ok(true) => {
            let meta = fs::symlink_metadata(dir);
            let is_symlink = meta.map(|m| m.is_symlink()).unwrap_or(false);

            if settings.move_to_trash && !is_symlink {
                trash::delete(dir).map_err(|e| e.to_string())
            } else if is_symlink {
                fs::remove_dir(dir)
                    .or_else(|_| fs::remove_file(dir))
                    .map_err(|e| e.to_string())
            } else {
                fs::remove_dir(dir).map_err(|e| e.to_string())
            }
        }
        Ok(false) => Err("Directory is not empty (contains non-ignored files)".to_string()),
        Err(e) => Err(e),
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

    // O(N * D) instead of O(N^2) using FxHashSet for ancestor lookup
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
            "[*] Attempting batch deletion to Recycle Bin (Root-Chop)...",
            0,
            0,
        );

        let mut verification_failed = false;
        for &i in &root_delete_targets {
            if let Err(e) = clean_and_verify_empty(&nodes[i].path, settings, &file_matchers) {
                log(
                    &format!(
                        "[!] Verification failed for {}: {}",
                        nodes[i].path.display(),
                        e
                    ),
                    i,
                    4,
                );
                verification_failed = true;
                break;
            }
        }

        if !verification_failed {
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
                            "[!] Batch Recycle Bin deletion failed: {}. Falling back to safe bottom-up...",
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
        let mut total_items = 0;

        for (i, node) in nodes.iter().enumerate() {
            if node.status == 1 {
                depths.entry(node.depth).or_default().push(i);
                total_items += 1;
            }
        }

        for (_depth, indices) in depths.into_iter().rev() {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }

            if settings.pause_ms == 0 && indices.len() > 1 {
                let results: Vec<_> = {
                    let nodes_ref: &[DirectoryNode] = nodes;
                    let cancel_inner = cancel_flag.clone();
                    indices
                        .par_iter()
                        .map(|&i| {
                            if cancel_inner.load(Ordering::Relaxed) {
                                return (i, 4, "Cancelled".to_string(), None);
                            }
                            let dir = &nodes_ref[i].path;

                            if settings.dry_run {
                                return (
                                    i,
                                    2,
                                    format!("[Dry-Run] Would delete: {}", dir.display()),
                                    None,
                                );
                            }

                            match perform_directory_delete(dir, settings, &file_matchers) {
                                Ok(_) => (i, 2, format!("Deleted: {}", dir.display()), None),
                                Err(e) => (
                                    i,
                                    4,
                                    format!("Failed to delete {}: {}", dir.display(), e),
                                    Some(e),
                                ),
                            }
                        })
                        .collect()
                };

                let mut abort = false;
                for (i, status, msg, _err) in results {
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
