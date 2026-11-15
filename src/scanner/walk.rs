use super::*;
pub fn scan_empty_dirs(
    root: &Path,
    settings: &ScanSettings,
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
        let mut batch = BatchSender::new(tx.clone());

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
                    let meta = entry.metadata().ok();
                    let is_symlink = meta
                        .as_ref()
                        .map(|m| {
                            file_type
                                .as_ref()
                                .map(|ft| ft.is_symlink())
                                .unwrap_or(false)
                                || is_reparse_point(m)
                        })
                        .unwrap_or(false);

                    if is_dir {
                        let facts = dir_facts_from_meta(
                            &file_name,
                            meta.as_ref(),
                            settings_ref.min_age_hours,
                        );

                        batch.batch.dirs.push(DiscoveredDir {
                            path: p,
                            depth,
                            is_young: facts.is_young,
                            is_hidden: facts.is_hidden,
                            is_symlink,
                        });
                    } else {
                        let facts = file_facts_from_meta(&file_name, meta.as_ref());
                        let kind = classify_file(&facts, settings_ref, file_matchers_ref);

                        if let Some(parent) = p.parent() {
                            let parent = Arc::<Path>::from(parent);
                            match kind {
                                FileKind::Content => {
                                    batch.batch.occupied_parents.insert(parent);
                                }
                                FileKind::Minor => {
                                    batch.batch.minor_occupied_parents.insert(parent);
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    batch.batch.errors.push(err.to_string());
                }
            }

            batch.flush_if_needed();

            ignore::WalkState::Continue
        })
    });
    drop(tx);

    let mut dir_states: Vec<DiscoveredDir> = Vec::new();
    let mut occupied_parents: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut minor_occupied_parents: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut walk_errors: Vec<String> = Vec::new();

    for mut batch in rx {
        dir_states.append(&mut batch.dirs);
        occupied_parents.extend(batch.occupied_parents);
        minor_occupied_parents.extend(batch.minor_occupied_parents);
        walk_errors.append(&mut batch.errors);
    }

    if cancel_flag.load(Ordering::Relaxed) {
        return Err("Operation cancelled by user".to_string());
    }

    if !walk_errors.is_empty() {
        if settings.hide_search_errors {
            tracing::warn!(
                "{} item(s) skipped due to access errors.",
                walk_errors.len()
            );
        } else {
            for e in &walk_errors {
                tracing::warn!("Access error: {}", e);
            }
        }
    }

    dir_states.sort_by_key(|d| std::cmp::Reverse(d.depth));

    let mut dir_status: FxHashMap<Arc<Path>, bool> = FxHashMap::default();
    let mut included_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut empty_dirs_found: FxHashSet<Arc<Path>> = FxHashSet::default();
    let mut orphaned_dirs: FxHashSet<Arc<Path>> = FxHashSet::default();
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

        let facts = DirFacts {
            is_hidden: dir.is_hidden,
            is_young: dir.is_young,
            is_system: fs::metadata(&dir.path)
                .as_ref()
                .map(is_system_metadata)
                .unwrap_or(false),
            is_symlink: dir.is_symlink,
        };

        if is_empty && is_protected_dir(&dir.path, &facts, settings, &dir_filter) {
            is_empty = false;
            is_protected = true;
        }

        dir_status.insert(dir.path.clone(), is_empty);

        if dir.path.as_ref() != root {
            if is_empty {
                let is_orphaned = minor_occupied_parents.contains(&dir.path);
                if is_orphaned {
                    orphaned_dirs.insert(dir.path.clone());
                }
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
            if settings.max_results > 0 && included_dirs.len() >= settings.max_results {
                break;
            }
        }
    }

    let mut sorted_paths: Vec<Arc<Path>> = included_dirs.into_iter().collect();
    sorted_paths.sort();

    let mut result = Vec::with_capacity(sorted_paths.len());
    for p in sorted_paths {
        // The emptiness was already decided while walking; here it only needs
        // to be turned into a status.
        let status = status_of(
            empty_dirs_found.contains(&p),
            orphaned_dirs.contains(&p),
            protected_dirs.contains(&p),
        );
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
            status,
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
