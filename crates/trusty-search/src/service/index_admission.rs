//! Shared initial-walk and live-event admission (#7379).
use crate::core::registry::IndexHandle;
use crate::service::walker::{self, walk_source_files_with_options, WalkOptions};
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
/// Why: watcher saves must honor the same current policy as reindex.
/// What: check configured subtrees and filters, then the walker's ignore engine along only this path.
/// Test: `live_admission_observes_registry_replacement`.
pub(crate) fn admits(handle: &IndexHandle, path: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    if !configured_file(handle, &path) {
        return false;
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
    for root in roots {
        let Ok(root) = root.canonicalize() else {
            continue;
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
            return true;
        }
    }
    false
}

/// Read live policy for each delivered modification, including updates after watcher startup.
pub(crate) async fn apply_modified(
    registry: &crate::core::registry::IndexRegistry,
    index_id: &crate::core::registry::IndexId,
    path: &Path,
    canonical_root: &Path,
    raw_root: &Path,
    indexer: &std::sync::Arc<tokio::sync::RwLock<crate::core::CodeIndexer>>,
    indexed_files: &crate::service::IndexedFiles,
) {
    let Some(handle) = registry.get(index_id) else {
        return;
    };
    if admits(&handle, path) {
        crate::service::watch_loop::handle_modified(
            path,
            index_id,
            canonical_root,
            raw_root,
            indexer,
            indexed_files,
        )
        .await;
    } else {
        crate::service::watch_loop::handle_removed(
            path,
            index_id,
            canonical_root,
            raw_root,
            indexer,
            indexed_files,
        )
        .await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        registry::{IndexId, IndexRegistry},
        CodeIndexer,
    };
    use std::sync::Arc;
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
                &root,
                &root,
                &indexer,
                &files,
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
            &root,
            &root,
            &indexer,
            &files,
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
            &root,
            &root,
            &indexer,
            &files,
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
}
