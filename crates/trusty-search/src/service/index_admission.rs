//! Shared initial-walk and live-event admission (#7379).
use crate::core::registry::IndexHandle;
use crate::service::walker::{self, walk_source_files_with_options, WalkOptions};
use crate::service::watch_rescan::RescanGate;
use std::path::{Path, PathBuf};

pub(crate) fn walk(handle: &IndexHandle) -> walker::WalkResult {
    let include_paths: Vec<PathBuf> = if handle.include_paths.is_empty() {
        vec![handle.root_path.clone()]
    } else {
        handle.include_paths.clone()
    };
    let mut walked_files: Vec<PathBuf> = Vec::new();
    let mut total_skipped_dirs: usize = 0;
    // Issue #1372: resolve the per-index hygiene knobs onto the walk options.
    // `data_file_max_bytes` is an `Option<u64>` on the handle's config source;
    // it was already resolved to a concrete `u64` field on the handle, so the
    // walker always receives a concrete cap.
    let walk_opts = WalkOptions {
        include_docs: handle.include_docs,
        respect_gitignore: handle.respect_gitignore,
        follow_links: handle.follow_links,
        extra_skip_dirs: handle.extra_skip_dirs.clone(),
        data_file_max_bytes: handle.data_file_max_bytes,
    };
    for subtree in &include_paths {
        let w = walk_source_files_with_options(subtree, &walk_opts);
        walked_files.extend(w.files);
        total_skipped_dirs = total_skipped_dirs.saturating_add(w.skipped_dirs);
    }

    walked_files.retain(|path| configured_file(handle, path));

    // De-duplicate when multiple `include_paths` overlap.
    walked_files.sort();
    walked_files.dedup();

    crate::service::walker::WalkResult {
        files: walked_files,
        skipped_dirs: total_skipped_dirs,
    }
}
fn configured_file(handle: &IndexHandle, path: &Path) -> bool {
    !tombstone_file(path)
        && !crate::core::repo_config::path_matches_any_glob(path, &handle.exclude_globs)
        && (handle.extensions.is_empty()
            || path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    handle
                        .extensions
                        .iter()
                        .any(|e| e.eq_ignore_ascii_case(ext))
                }))
        && (handle.path_filter.is_empty()
            || crate::core::registry::path_matches_filter(
                path,
                &handle
                    .root_path
                    .canonicalize()
                    .unwrap_or_else(|_| handle.root_path.clone()),
                &handle.path_filter,
            ))
}
fn tombstone_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return false;
    }
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut prefix = Vec::new();
    file.take(65 * 1024).read_to_end(&mut prefix).is_ok()
        && trusty_common::knowledge_document::is_tombstone(&String::from_utf8_lossy(&prefix))
}
/// Whether the live policy indexes a path — or whether that could not be decided.
///
/// Why (#7396): [`apply_modified`] routes a negative answer into the REMOVAL
/// path, so "the policy excludes this path" and "the filesystem could not
/// answer right now" must not share one answer. A transient `EACCES`, a
/// network-mount hiccup, or the window inside an atomic rename would otherwise
/// delete every chunk the file owns.
/// What: three states. Only [`Admission::Excluded`] may reach a removal.
/// Test: `a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The current policy indexes this path.
    Included,
    /// The current policy excludes this path, or the path is definitively gone.
    Excluded,
    /// The filesystem could not answer. Nothing may be removed on this.
    Undetermined,
}

/// Classify a `canonicalize` failure as definitive absence or a transient miss.
///
/// Why (#7396): the removal branch is destructive, so it may only be taken on
/// an error that actually means "this path is gone".
/// What: a `NotFound` is re-checked with `symlink_metadata`, which neither
/// follows links nor needs the whole prefix resolved; a second `NotFound` is
/// the definitive answer. Every other error kind is undetermined.
/// Test: `a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan`.
fn resolve_failure(path: &Path, err: &std::io::Error) -> Admission {
    let gone = err.kind() == std::io::ErrorKind::NotFound
        && path
            .symlink_metadata()
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if gone {
        Admission::Excluded
    } else {
        Admission::Undetermined
    }
}

/// Why: watcher saves must honor the same current policy as reindex.
/// What: check configured subtrees and filters, then the walker's ignore engine along only this path.
/// Test: `live_admission_observes_registry_replacement`.
pub(crate) fn admits(handle: &IndexHandle, path: &Path) -> Admission {
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(err) => return resolve_failure(path, &err),
    };
    if !configured_file(handle, &path) {
        return Admission::Excluded;
    }
    let roots = if handle.include_paths.is_empty() {
        vec![handle.root_path.clone()]
    } else {
        handle.include_paths.clone()
    };
    let opts = WalkOptions {
        include_docs: handle.include_docs,
        respect_gitignore: handle.respect_gitignore,
        follow_links: handle.follow_links,
        extra_skip_dirs: handle.extra_skip_dirs.clone(),
        data_file_max_bytes: handle.data_file_max_bytes,
    };
    // #7396: a root we could not resolve is not evidence that the file left the
    // index, so it downgrades the fall-through answer rather than being skipped.
    let mut unresolved_root = false;
    for root in roots {
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(err) => {
                unresolved_root |= resolve_failure(&root, &err) == Admission::Undetermined;
                continue;
            }
        };
        if !path.starts_with(&root) || !walker::path_admitted(&root, &path, &opts) {
            continue;
        }
        let target = path.clone();
        let mut builder = walker::configured_builder(&root, &opts);
        builder.filter_entry(move |entry| target.starts_with(entry.path()));
        if builder
            .build()
            .filter_map(Result::ok)
            .any(|entry| entry.path() == path && entry.file_type().is_some_and(|t| t.is_file()))
        {
            return Admission::Included;
        }
    }
    if unresolved_root {
        Admission::Undetermined
    } else {
        Admission::Excluded
    }
}

/// Leave the index untouched and ask a rescan to settle this path.
///
/// Why (#7396): the alternative at an undecidable admission is to guess, and
/// the wrong guess deletes data. A rescan re-derives the path's state from
/// disk, which is the recovery the dropped-event path already uses.
/// What: warns with the reason, then asks [`RescanGate::request`] for a pass.
/// The gate, not this function, decides whether a timer is armed: one cause —
/// a mount that went away, a directory the daemon lost access to — answers
/// every event in a batch undecidably, and one full-tree reconcile settles all
/// of them, so a request made while one is already outstanding is dropped
/// (#7396). Consecutive-failure backoff belongs to the rescan arm, which owns
/// that counter; a deferred save is a fresh request.
/// Test: `a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan`,
/// `three_undecidable_events_schedule_exactly_one_rescan`.
fn defer_to_rescan(
    index_id: &crate::core::registry::IndexId,
    path: &Path,
    reason: &str,
    rescan: Option<&RescanGate>,
) {
    let armed = rescan.is_some_and(RescanGate::request);
    tracing::warn!(
        index_id = %index_id,
        path = %path.display(),
        reason,
        armed,
        "live admission could not be decided — index left untouched, deferring to a rescan",
    );
}

/// The watched root in the two forms the relative-path fallback needs.
///
/// Why: `canonical` is what the reindex walker keys on; `raw` is the root as
/// configured, which a deleted file's path must also be stripped against
/// because canonicalizing a gone path fails (see `watch_loop`). They always
/// travel together, so they travel as one argument.
#[derive(Clone, Copy)]
pub(crate) struct WatchRoots<'a> {
    pub(crate) canonical: &'a Path,
    pub(crate) raw: &'a Path,
}

/// Read live policy for each delivered modification, including updates after watcher startup.
///
/// Why (#7396): an undecidable admission must not reach `handle_removed`.
/// What: the three [`Admission`] states map to index, remove, and defer; only
/// the third leaves the index untouched and re-arms a rescan.
/// Test: `a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan`,
/// `live_admission_observes_registry_replacement`,
/// `three_undecidable_events_schedule_exactly_one_rescan`.
pub(crate) async fn apply_modified(
    registry: &crate::core::registry::IndexRegistry,
    index_id: &crate::core::registry::IndexId,
    path: &Path,
    roots: WatchRoots<'_>,
    indexer: &std::sync::Arc<tokio::sync::RwLock<crate::core::CodeIndexer>>,
    indexed_files: &crate::service::IndexedFiles,
    rescan: Option<&RescanGate>,
) {
    let Some(handle) = registry.get(index_id) else {
        // #7396: an absent handle is a failed pass, not a reason to drop the
        // event — the ruling `watch_rescan::reconcile_registered` already makes.
        defer_to_rescan(index_id, path, "index is not registered", rescan);
        return;
    };
    match admits(&handle, path) {
        Admission::Included => {
            crate::service::watch_loop::handle_modified(
                path,
                index_id,
                roots.canonical,
                roots.raw,
                indexer,
                indexed_files,
            )
            .await;
        }
        Admission::Excluded => {
            crate::service::watch_loop::handle_removed(
                path,
                index_id,
                roots.canonical,
                roots.raw,
                indexer,
                indexed_files,
            )
            .await;
        }
        Admission::Undetermined => {
            defer_to_rescan(index_id, path, "path could not be resolved", rescan);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        registry::{IndexId, IndexRegistry},
        CodeIndexer,
    };
    use crate::service::watcher::WatchEvent;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;
    #[tokio::test]
    async fn live_admission_observes_registry_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("notes")).unwrap();
        for (path, content) in [
            ("notes/maya.md", "# Maya\nMaya leads Atlas."),
            ("registry.toml", "name='synthetic'"),
            ("notes/private.md", "# Private\nSynthetic excluded content"),
            ("control.md", "# Control\nOutside selected subtree"),
        ] {
            std::fs::write(root.join(path), content).unwrap();
        }
        let id = IndexId::new("synthetic");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new("synthetic", &root)));
        let registry = IndexRegistry::new();
        let make = |extensions: Vec<String>, excludes: Vec<String>| {
            let mut handle = IndexHandle::bare(id.clone(), indexer.clone(), root.clone());
            handle.include_paths = vec![root.join("notes")];
            handle.extensions = extensions;
            handle.exclude_globs = excludes;
            handle
        };
        let handle = registry.register(make(vec!["md".into()], vec!["**/private.md".into()]));
        let expected = vec![root.join("notes/maya.md")];
        assert_eq!(walk(&handle).files, expected);
        let files = crate::service::IndexedFiles::new();
        let roots = WatchRoots {
            canonical: &root,
            raw: &root,
        };
        for path in [
            "notes/maya.md",
            "notes/private.md",
            "registry.toml",
            "control.md",
        ] {
            apply_modified(
                &registry,
                &id,
                &root.join(path),
                roots,
                &indexer,
                &files,
                None,
            )
            .await;
        }
        assert!(!indexer
            .read()
            .await
            .chunk_ids_for_file("notes/maya.md")
            .await
            .is_empty());
        for path in ["notes/private.md", "registry.toml", "control.md"] {
            assert!(indexer
                .read()
                .await
                .chunk_ids_for_file(path)
                .await
                .is_empty());
        }
        registry.register(make(vec!["rs".into()], vec![]));
        apply_modified(
            &registry,
            &id,
            &root.join("notes/maya.md"),
            roots,
            &indexer,
            &files,
            None,
        )
        .await;
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("notes/maya.md")
            .await
            .is_empty());
        let handle = registry.register(make(vec!["md".into()], vec![]));
        crate::service::watch_rescan::reconcile_with_policy(
            &id,
            &root,
            &root,
            &indexer,
            &files,
            Some(&handle),
        )
        .await
        .unwrap();
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("registry.toml")
            .await
            .is_empty());
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("control.md")
            .await
            .is_empty());
        assert!(!indexer
            .read()
            .await
            .chunk_ids_for_file("notes/private.md")
            .await
            .is_empty());
        let deleted = "---\nsource_id: synthetic\nsource_status: deleted\n---\nMaya leads Atlas.";
        std::fs::write(root.join("notes/maya.md"), deleted).unwrap();
        apply_modified(
            &registry,
            &id,
            &root.join("notes/maya.md"),
            roots,
            &indexer,
            &files,
            None,
        )
        .await;
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("notes/maya.md")
            .await
            .is_empty());
        assert!(!walk(&handle).files.contains(&root.join("notes/maya.md")));
        indexer
            .read()
            .await
            .index_file("notes/maya.md", deleted)
            .await
            .unwrap();
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("notes/maya.md")
            .await
            .is_empty());
        crate::service::watch_rescan::reconcile_with_policy(
            &id,
            &root,
            &root,
            &indexer,
            &files,
            Some(&handle),
        )
        .await
        .unwrap();
        assert!(indexer
            .read()
            .await
            .chunk_ids_for_file("notes/maya.md")
            .await
            .is_empty());
    }

    /// Why (#7396): `apply_modified` used to route EVERY negative answer from
    /// `admits` into `handle_removed`, and `admits` answered negative when
    /// `canonicalize` merely failed. One `EACCES`, one network-mount hiccup, or
    /// one save landing inside an atomic-rename window therefore purged the
    /// file from the index — a destructive fail-open on an uncertain answer.
    ///
    /// The fixture is a self-referential symlink at the indexed path: the path
    /// still exists (`symlink_metadata` succeeds) but `canonicalize` fails with
    /// `ELOOP`. That is deterministic on every platform and, unlike a
    /// `chmod 000` fixture, does not depend on the test user not being root —
    /// the same reasoning as `watch_rescan_tests::write_unreadable_source`.
    /// The permission and rename-window error kinds are then classified
    /// directly, since they cannot be provoked portably.
    ///
    /// Against `557ab9c5c` the chunk assertion below fails: the chunks are gone.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_transient_canonicalize_failure_keeps_chunks_and_schedules_a_rescan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("notes")).unwrap();
        let target = root.join("notes/maya.md");
        std::fs::write(&target, "# Maya\nMaya leads Atlas.").unwrap();

        let id = IndexId::new("synthetic");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new("synthetic", &root)));
        let registry = IndexRegistry::new();
        let mut handle = IndexHandle::bare(id.clone(), indexer.clone(), root.clone());
        handle.include_paths = vec![root.join("notes")];
        handle.extensions = vec!["md".into()];
        registry.register(handle);
        let files = crate::service::IndexedFiles::new();
        let roots = WatchRoots {
            canonical: &root,
            raw: &root,
        };

        apply_modified(&registry, &id, &target, roots, &indexer, &files, None).await;
        assert!(
            !indexer
                .read()
                .await
                .chunk_ids_for_file("notes/maya.md")
                .await
                .is_empty(),
            "the file must be indexed before the failure is introduced"
        );

        // The path exists, but resolving it fails — the shape of a save caught
        // mid-rename, or of a mount that answered with an error this instant.
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink("maya.md", &target).unwrap();
        assert!(
            target.canonicalize().is_err(),
            "the fixture must actually break canonicalize"
        );
        assert!(
            target.symlink_metadata().is_ok(),
            "the path itself is still there — this is not a deletion"
        );
        assert_eq!(
            admits(&registry.get(&id).unwrap(), &target),
            Admission::Undetermined,
            "an unresolvable path is not an exclusion"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WatchEvent>();
        let gate = RescanGate::new(tx);
        apply_modified(
            &registry,
            &id,
            &target,
            roots,
            &indexer,
            &files,
            Some(&gate),
        )
        .await;
        assert!(
            !indexer
                .read()
                .await
                .chunk_ids_for_file("notes/maya.md")
                .await
                .is_empty(),
            "a canonicalize failure must never delete the file's chunks"
        );
        assert_eq!(
            rx.recv().await,
            Some(WatchEvent::Rescan),
            "the undecided path must be re-armed for a rescan, not dropped"
        );

        // Error kinds that cannot be provoked portably, classified directly.
        use std::io::{Error, ErrorKind};
        assert_eq!(
            resolve_failure(&target, &Error::from(ErrorKind::PermissionDenied)),
            Admission::Undetermined,
            "a permission error is not evidence the file left the index"
        );
        assert_eq!(
            resolve_failure(&target, &Error::from(ErrorKind::NotFound)),
            Admission::Undetermined,
            "a NotFound whose path is still on disk is the rename window"
        );
        assert_eq!(
            resolve_failure(
                &root.join("notes/gone.md"),
                &Error::from(ErrorKind::NotFound)
            ),
            Admission::Excluded,
            "a path that is definitively absent still takes the removal branch"
        );
    }

    /// Why (#7396): `defer_to_rescan` armed a timer once per undecidable event,
    /// outside the `RescanFollowUp` decision that owns scheduling. The causes
    /// are not per-file — a mount that answered with an error, a directory the
    /// daemon lost access to — so one batch answers undecidably for every file
    /// under it, and the watch task paid N detached timers and N full-tree
    /// reconciles for one cause that a single pass settles.
    ///
    /// The fixture is three self-referential symlinks, the same deterministic
    /// `ELOOP` shape `a_transient_canonicalize_failure_keeps_chunks_and_
    /// schedules_a_rescan` uses, standing in for three files under one broken
    /// subtree.
    ///
    /// Against `3c7dbeaec` this fails at the count: three `Rescan` events are
    /// waiting instead of one.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn three_undecidable_events_schedule_exactly_one_rescan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("notes")).unwrap();

        let id = IndexId::new("synthetic");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new("synthetic", &root)));
        let registry = IndexRegistry::new();
        let mut handle = IndexHandle::bare(id.clone(), indexer.clone(), root.clone());
        handle.include_paths = vec![root.join("notes")];
        handle.extensions = vec!["md".into()];
        registry.register(handle);
        let files = crate::service::IndexedFiles::new();
        let roots = WatchRoots {
            canonical: &root,
            raw: &root,
        };

        let targets: Vec<PathBuf> = ["maya.md", "atlas.md", "orion.md"]
            .iter()
            .map(|name| {
                let target = root.join("notes").join(name);
                std::os::unix::fs::symlink(name, &target).unwrap();
                assert_eq!(
                    admits(&registry.get(&id).unwrap(), &target),
                    Admission::Undetermined,
                    "the fixture must actually produce an undecidable admission"
                );
                target
            })
            .collect();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WatchEvent>();
        let gate = RescanGate::new(tx);
        for target in &targets {
            apply_modified(&registry, &id, target, roots, &indexer, &files, Some(&gate)).await;
        }

        // Sleeping past the base backoff on a paused clock auto-advances to each
        // armed timer in turn and lets its detached task send, so draining after
        // it sees every `Rescan` that was armed — not just the first, which is
        // what makes the count able to fail.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut rescans = 0usize;
        while let Ok(event) = rx.try_recv() {
            assert_eq!(event, WatchEvent::Rescan, "the gate arms nothing else");
            rescans += 1;
        }
        assert_eq!(
            rescans, 1,
            "three undecidable events must coalesce into one full-tree reconcile"
        );

        // The pass that discharges the request re-opens the gate: a later
        // undecidable event is a new cause, not a duplicate of the settled one.
        gate.disarm();
        apply_modified(
            &registry,
            &id,
            &targets[0],
            roots,
            &indexer,
            &files,
            Some(&gate),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(
            rx.try_recv().ok(),
            Some(WatchEvent::Rescan),
            "a defer after the pass must arm again"
        );
    }
}
