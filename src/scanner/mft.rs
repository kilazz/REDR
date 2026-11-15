use super::rules::{DirFacts, FileFacts, FileKind, classify_file, is_protected_dir, status_of};
use super::*;
#[cfg(target_os = "windows")]
#[cfg(target_os = "windows")]
fn mark_occupied_ancestors(file_lower: &str, root_lower: &str, occupied: &mut FxHashSet<String>) {
    let mut current = file_lower;
    while let Some(idx) = current.rfind('\\') {
        let mut parent = &current[..idx];
        if parent.ends_with(':') {
            parent = &current[..idx + 1];
        }

        if parent.is_empty() || occupied.contains(parent) {
            break;
        }
        occupied.insert(parent.to_string());

        if parent == root_lower || parent.ends_with(":\\") {
            break;
        }
        current = parent;
    }
}

#[cfg(target_os = "windows")]
fn include_ancestor_dirs(
    dir_lower: &str,
    root_lower: &str,
    all_dirs: &FxHashMap<String, PathBuf>,
    included: &mut FxHashSet<PathBuf>,
) {
    let mut current = dir_lower;
    while let Some(idx) = current.rfind('\\') {
        let mut parent = &current[..idx];
        if parent.ends_with(':') {
            parent = &current[..idx + 1];
        }

        if parent == root_lower || !parent.starts_with(root_lower) {
            break;
        }

        let inserted = if let Some(exact_parent) = all_dirs.get(parent) {
            included.insert(exact_parent.clone())
        } else {
            included.insert(PathBuf::from(parent))
        };
        if !inserted {
            break;
        }

        if parent.ends_with(":\\") {
            break;
        }
        current = parent;
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn ntfs_path_to_win_path(
    raw_path: &Path,
    ntfs_volume_path: &str,
    drive_letter: &str,
) -> PathBuf {
    let volume_path_buf = PathBuf::from(ntfs_volume_path);
    let win_root = PathBuf::from(format!("{}:\\", drive_letter));
    if let Ok(rel) = raw_path.strip_prefix(&volume_path_buf) {
        let trimmed = rel.to_string_lossy().trim_start_matches('\\').to_string();
        if trimmed.is_empty() {
            win_root
        } else {
            win_root.join(trimmed)
        }
    } else if raw_path.starts_with(&win_root) {
        raw_path.to_path_buf()
    } else {
        let clean = raw_path.strip_prefix("\\").unwrap_or(raw_path);
        win_root.join(clean)
    }
}

#[derive(Default)]
struct MftCounters {
    total: usize,
    used: usize,
    outside_root: usize,
    unresolved: usize,
    deleted_skipped: usize,
}

pub fn scan_empty_dirs_mft(
    root: &Path,
    settings: &ScanSettings,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<Vec<DirectoryNode>, String> {
    use ntfs_reader::DefaultPathCache;
    use ntfs_reader::FileInfo;
    use ntfs_reader::Mft;
    use ntfs_reader::Volume;
    use std::path::Component;

    tracing::info!("Initializing Direct MFT Scan...");

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

    let ntfs_volume_path = format!("\\\\.\\{}:", drive_letter);
    let volume = Volume::new(&ntfs_volume_path).map_err(|e| {
        format!(
            "Failed to open NTFS volume (Requires Administrator privileges): {}",
            e
        )
    })?;

    let mft = Mft::new(volume)
        .map_err(|e| format!("Failed to initialize Master File Table parser: {}", e))?;

    tracing::info!("Reading Master File Table records...");

    let root_lower_str = lowercase_path(root);
    let mut all_dirs: FxHashMap<String, PathBuf> = FxHashMap::default();
    let mut occupied_dirs: FxHashSet<String> = FxHashSet::default();
    let mut minor_occupied_dirs: FxHashSet<String> = FxHashSet::default();
    let mut reparse_dirs: FxHashSet<String> = FxHashSet::default();
    let mut child_dirs: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut deleted_dirs: FxHashSet<String> = FxHashSet::default();

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

    let mut path_cache = DefaultPathCache::new();
    let mut deleted_cache = ntfs_reader::DeletedPathCache::new();

    let mut counters = MftCounters::default();
    let mut process_file = |file: ntfs_reader::NtfsFile<'_>,
                            use_deleted_cache: bool,
                            all_dirs: &mut FxHashMap<String, PathBuf>,
                            occupied_dirs: &mut FxHashSet<String>,
                            counters: &mut MftCounters|
     -> Result<(), String> {
        let info = if use_deleted_cache {
            FileInfo::with_caches(&file, &mut path_cache, &mut deleted_cache)
        } else {
            FileInfo::with_cache(&file, &mut path_cache)
        };

        let raw_path = match info.path {
            Some(p) => p,
            None => {
                counters.unresolved += 1;
                return Ok(());
            }
        };

        let path_with_drive = ntfs_path_to_win_path(&raw_path, &ntfs_volume_path, &drive_letter);

        let p_lower = lowercase_path(&path_with_drive);

        if !is_within_root(&p_lower) {
            counters.outside_root += 1;
            return Ok(());
        }
        counters.used += 1;

        if info.is_directory {
            if info.file_attributes & 0x400 != 0 {
                reparse_dirs.insert(p_lower.clone());
            }
            if use_deleted_cache {
                // A deleted directory no longer exists on disk: show it, but never treat it
                // as an empty candidate and never let it affect its parent's emptiness.
                deleted_dirs.insert(p_lower.clone());
            } else if let Some(parent) = std::path::Path::new(&p_lower).parent() {
                child_dirs
                    .entry(parent.to_string_lossy().to_string())
                    .or_default()
                    .push(p_lower.clone());
            }
            all_dirs.insert(p_lower, path_with_drive);
        } else {
            let child_name = path_with_drive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let facts = FileFacts {
                name: child_name.to_string(),
                size: Some(info.size),
                modified: info.modified.map(|t| t.unix_timestamp()),
                is_offline: info.file_attributes & (0x4000 | 0x40000) != 0,
            };
            let kind = classify_file(&facts, settings, &file_matchers);

            if use_deleted_cache {
                // Deleted content only marks the directory as "orphaned": it is still a
                // removal candidate, but the user gets to know it used to hold files.
                if let Some(parent) = std::path::Path::new(&p_lower).parent() {
                    minor_occupied_dirs.insert(parent.to_string_lossy().to_string());
                }
            } else {
                match kind {
                    FileKind::Content => {
                        mark_occupied_ancestors(&p_lower, &root_lower_str, occupied_dirs);
                    }
                    FileKind::Minor => {
                        if let Some(parent) = std::path::Path::new(&p_lower).parent() {
                            minor_occupied_dirs.insert(parent.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    };

    for file in mft.files() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("Operation cancelled by user".to_string());
        }
        counters.total += 1;
        process_file(
            file,
            false,
            &mut all_dirs,
            &mut occupied_dirs,
            &mut counters,
        )?;
    }

    if settings.include_deleted {
        for file in mft.deleted_files() {
            if cancel_flag.load(Ordering::Relaxed) {
                return Err("Operation cancelled by user".to_string());
            }
            counters.total += 1;
            process_file(file, true, &mut all_dirs, &mut occupied_dirs, &mut counters)?;
        }
    } else {
        counters.deleted_skipped = mft.deleted_files().count();
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
            && !deleted_dirs.contains(p_lower)
            && p_lower.starts_with(&root_lower_str)
            && p_lower != &root_lower_str
        {
            empty_dirs_found.insert(p_lower.clone());
            included_dirs.insert(p.clone());

            include_ancestor_dirs(p_lower, &root_lower_str, &all_dirs, &mut included_dirs);
        }
        if settings.max_results > 0 && included_dirs.len() >= settings.max_results {
            break;
        }
    }

    tracing::info!("Verifying MFT directory states...");

    let mut true_empty: FxHashSet<String> = FxHashSet::default();
    let mut empty_vec: Vec<String> = empty_dirs_found.into_iter().collect();
    empty_vec.sort_by_key(|p| std::cmp::Reverse(p.matches('\\').count()));

    // Verify each depth level in parallel; deeper levels are processed first so
    // a directory's children are already decided.
    let mut i = 0;
    while i < empty_vec.len() {
        let depth = empty_vec[i].matches('\\').count();
        let mut j = i;
        while j < empty_vec.len() && empty_vec[j].matches('\\').count() == depth {
            j += 1;
        }
        let group: Vec<String> = empty_vec[i..j].to_vec();
        let snapshot = true_empty.clone();
        let child_dirs_snapshot = child_dirs.clone();
        let results: Vec<(String, bool)> = group
            .par_iter()
            .map(|p_lower| {
                let is_truly_empty = child_dirs_snapshot
                    .get(p_lower)
                    .map(|children| children.iter().all(|c| snapshot.contains(c)))
                    .unwrap_or(true);
                (p_lower.clone(), is_truly_empty)
            })
            .collect();
        for (p, ok) in results {
            if ok {
                true_empty.insert(p);
            }
        }
        i = j;
    }

    let mut sorted_paths: Vec<PathBuf> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::with_capacity(sorted_paths.len());
    for p in sorted_paths {
        let p_lower = lowercase_path(&p);
        let is_empty = true_empty.contains(&p_lower);
        let depth = (p.components().count() as i32) - root_depth;
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let meta = fs::metadata(&p).ok();
        let facts = DirFacts {
            is_hidden: meta
                .as_ref()
                .map(|m| is_hidden_metadata(m, &name))
                .unwrap_or(false),
            is_system: meta.as_ref().map(is_system_metadata).unwrap_or(false),
            is_young: meta
                .as_ref()
                .map(|m| is_metadata_too_young(m, settings.min_age_hours))
                .unwrap_or(false),
            is_symlink: reparse_dirs.contains(&p_lower)
                || fs::symlink_metadata(&p)
                    .map(|m| m.file_type().is_symlink() || is_reparse_point(&m))
                    .unwrap_or(false),
        };

        let is_protected = is_protected_dir(&p, &facts, settings, &dir_filter);
        let status = status_of(
            is_empty,
            minor_occupied_dirs.contains(&p_lower),
            is_protected,
        );

        result.push(DirectoryNode {
            path: Arc::from(p.clone()),
            name: if p.as_path() == root {
                root.to_string_lossy().into_owned()
            } else {
                name
            },
            path_str: p.to_string_lossy().into_owned(),
            depth,
            status,
            has_children: false,
            is_expanded: true,
            is_last_sibling: false,
            is_hidden: facts.is_hidden,
            is_symlink: facts.is_symlink,
        });
    }

    compute_tree_relationships(&mut result);
    tracing::info!(
        "MFT coverage: {} records read, {} used, {} outside root, {} unresolved paths, {} deleted records skipped.",
        counters.total,
        counters.used,
        counters.outside_root,
        counters.unresolved,
        counters.deleted_skipped
    );
    tracing::info!(
        "Direct MFT Scan complete. Found {} empty directories.",
        true_empty.len()
    );

    Ok(result)
}

#[cfg(test)]
#[cfg(target_os = "windows")]
mod tests {
    use super::include_ancestor_dirs;
    use super::mark_occupied_ancestors;
    use super::ntfs_path_to_win_path;
    use rustc_hash::{FxHashMap, FxHashSet};
    use std::path::PathBuf;

    #[test]
    fn converts_volume_path() {
        assert_eq!(
            ntfs_path_to_win_path(&PathBuf::from(r"\\.\C:\dir\file"), r"\\.\C:", "C"),
            PathBuf::from(r"C:\dir\file")
        );
        assert_eq!(
            ntfs_path_to_win_path(&PathBuf::from(r"\\.\C:"), r"\\.\C:", "C"),
            PathBuf::from(r"C:\")
        );
    }

    #[test]
    fn marks_every_ancestor_as_occupied() {
        let mut occupied = FxHashSet::default();
        mark_occupied_ancestors(r"c:\a\b\c\file.txt", r"c:\a", &mut occupied);
        assert!(occupied.contains(r"c:\a\b\c"));
        assert!(occupied.contains(r"c:\a\b"));
        assert!(occupied.contains(r"c:\a"));
    }

    #[test]
    fn includes_ancestors_up_to_root() {
        let mut all_dirs: FxHashMap<String, PathBuf> = FxHashMap::default();
        all_dirs.insert(r"c:\a".to_string(), PathBuf::from(r"c:\a"));
        all_dirs.insert(r"c:\a\b".to_string(), PathBuf::from(r"c:\a\b"));

        let mut included = FxHashSet::default();
        include_ancestor_dirs(r"c:\a\b\c", r"c:\", &all_dirs, &mut included);

        assert!(included.contains(&PathBuf::from(r"c:\a\b")));
        assert!(included.contains(&PathBuf::from(r"c:\a")));
    }

    /// Builds a chain of nested directories, e.g. `c:\a`, `c:\a\b`, `c:\a\b\c`.
    fn chain(root: &str, depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut current = root.trim_end_matches('\\').to_string();
        for _ in 0..depth {
            current.push_str("\\x");
            out.push(current.clone());
        }
        out
    }

    proptest::proptest! {
        #[test]
        fn occupied_contains_every_ancestor(
            depth in 1usize..8,
            root in r"[a-z]:\\",
        ) {
            let dirs = chain(&root, depth);
            let file = format!("{}\\{}.txt", dirs.last().unwrap(), "file");
            let mut occupied = FxHashSet::default();
            mark_occupied_ancestors(&file, &root, &mut occupied);

            for dir in &dirs {
                proptest::prop_assert!(occupied.contains(dir.as_str()), "missing {}", dir);
            }
        }

        #[test]
        fn occupied_stops_at_the_root(
            depth in 1usize..6,
            root in r"[a-z]:\\",
        ) {
            let dirs = chain(&root, depth);
            let file = format!("{}\\{}.txt", dirs.last().unwrap(), "file");
            let mut occupied = FxHashSet::default();
            mark_occupied_ancestors(&file, &root, &mut occupied);

            // Nothing above the root may be marked.
            proptest::prop_assert!(!occupied.iter().any(|p| p.len() < root.len()));
        }

        #[test]
        fn volume_path_conversion_keeps_the_tail(
            depth in 1usize..6,
            drive in "[a-z]",
        ) {
            let drive_upper = drive.to_uppercase();
            let volume = format!(r"\\.\{}:", drive_upper);

            // Build a relative tail like `x\x\x`.
            let mut relative = String::new();
            for _ in 0..depth {
                relative.push_str("\\x");
            }

            let converted = ntfs_path_to_win_path(
                &PathBuf::from(format!("{}{}", volume, relative)),
                &volume,
                &drive_upper,
            );

            proptest::prop_assert_eq!(
                converted.to_string_lossy().to_lowercase(),
                format!("{}:{}", drive_upper.to_lowercase(), relative.to_lowercase())
            );
        }
    }
}
