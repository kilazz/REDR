use super::*;
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

    if is_protected_path(dir, &settings.protected_paths) {
        return Err("path is protected by the whitelist".to_string());
    }

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

pub fn delete_empty_dirs<P, S>(
    nodes: &mut [DirectoryNode],
    settings: &DeleteSettings,
    status_cb: &S,
    progress_cb: &P,
    cancel_flag: &Arc<AtomicBool>,
) -> (usize, usize)
where
    P: Fn(f32),
    S: Fn(usize, i32),
{
    let mut deleted = 0;
    let mut failed = 0;

    let mut empty_indices: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.status == NodeStatus::Empty || node.status == NodeStatus::Orphaned)
        // Whitelisted paths are never removed, whatever the scan said.
        .filter(|(_, node)| !is_protected_path(&node.path, &settings.protected_paths))
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
        tracing::info!("Verifying subtree integrity for batch Recycle Bin deletion...");

        let mut subtree_valid = true;
        for &i in &root_delete_targets {
            if let Err(e) = verify_subtree_empty(&nodes[i].path, settings, &file_matchers) {
                tracing::warn!("Cannot batch recycle {}: {}", nodes[i].path.display(), e);
                status_cb(i, 4);
                subtree_valid = false;
                break;
            }
        }

        if subtree_valid {
            match trash::delete_all(&covered_paths) {
                Ok(_) => {
                    for (progress_idx, &i) in empty_indices.iter().enumerate() {
                        nodes[i].status = NodeStatus::Deleted;
                        tracing::info!("Deleted (Trash): {}", nodes[i].path.display());
                        status_cb(i, 2);
                        progress_cb((progress_idx + 1) as f32 / empty_indices.len() as f32);
                    }
                    deleted = empty_indices.len();
                    batch_success = true;
                }
                Err(err) => {
                    tracing::warn!(
                        "Batch recycling failed: {}. Falling back to bottom-up deletion...",
                        err
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
                                return (i, NodeStatus::Failed, "Cancelled".to_string());
                            }
                            let dir = &nodes_ref[i].path;
                            if settings.dry_run {
                                return (
                                    i,
                                    NodeStatus::Deleted,
                                    format!("[Dry-Run] Would delete: {}", dir.display()),
                                );
                            }

                            match perform_directory_delete(dir, settings, &file_matchers) {
                                Ok(_) => (
                                    i,
                                    NodeStatus::Deleted,
                                    format!("Deleted: {}", dir.display()),
                                ),
                                Err(e) => (
                                    i,
                                    NodeStatus::Failed,
                                    format!("Failed to delete {}: {}", dir.display(), e),
                                ),
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

                    tracing::info!("{}", msg);
                    status_cb(i, status as i32);
                    nodes[i].status = status;

                    if status == NodeStatus::Deleted {
                        deleted += 1;
                    } else {
                        failed += 1;
                        if !settings.ignore_errors {
                            tracing::warn!("Aborting deletion due to error.");
                            status_cb(i, 4);
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
                        tracing::info!("[Dry-Run] Would delete: {}", dir.display());
                        status_cb(i, 2);
                        nodes[i].status = NodeStatus::Deleted;
                        deleted += 1;
                        processed_items += 1;
                        progress_cb(processed_items as f32 / total_items as f32);
                        continue;
                    }

                    match perform_directory_delete(&dir, settings, &file_matchers) {
                        Ok(_) => {
                            tracing::info!("Deleted: {}", dir.display());
                            status_cb(i, 2);
                            nodes[i].status = NodeStatus::Deleted;
                            deleted += 1;
                        }
                        Err(e) => {
                            tracing::warn!("Failed to delete {}: {}", dir.display(), e);
                            status_cb(i, 4);
                            nodes[i].status = NodeStatus::Failed;
                            failed += 1;
                            if !settings.ignore_errors {
                                tracing::warn!("Aborting deletion due to error.");
                                status_cb(i, 4);
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
