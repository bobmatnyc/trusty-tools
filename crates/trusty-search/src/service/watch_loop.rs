//! Glue between [`crate::service::watcher::FileWatcher`] and `CodeIndexer`.
//!
//! Why: The watcher emits raw filesystem events; the indexer wants
//! `index_file` / `remove_chunk` calls. This module bridges them and
//! maintains an [`IndexedFiles`] side-map so that file deletions can locate
//! the chunk IDs that need to come out of the HNSW + corpus.
//!
//! What: [`spawn_watch_loop`] starts the [`FileWatcher`] and a long-running
//! tokio task that consumes events. Returns a `WatcherTask` handle that owns
//! both the `FileWatcher` (so dropping it stops the OS watcher) and the
//! `JoinHandle` of the consumer task.
//!
//! Test: integration test below boots the loop on a temp dir, writes a file,
//! and asserts the indexer's `chunk_count()` increases.

use std::path::Path;
use std::sync::Arc;

use crate::core::chunker::chunk_ast;
use crate::core::CodeIndexer;
use anyhow::Result;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::service::indexed_files::IndexedFiles;
use crate::service::walker::{path_in_skipped_dir, should_skip_path};
use crate::service::watcher::{FileWatcher, WatchEvent};

/// Handle for a running watch loop. Drop it to stop watching and join the
/// consumer task on the next `await` boundary.
pub struct WatcherTask {
    _watcher: FileWatcher,
    _join: JoinHandle<()>,
}

/// Start watching `root_path` and forward changes into `indexer`.
///
/// `indexed_files` is shared with anyone else who needs to know which chunks
/// belong to which path (e.g. an explicit `remove_file` HTTP handler).
pub fn spawn_watch_loop(
    root_path: &Path,
    indexer: Arc<RwLock<CodeIndexer>>,
    indexed_files: IndexedFiles,
) -> Result<WatcherTask> {
    let (tx, mut rx) = mpsc::unbounded_channel::<WatchEvent>();
    let watcher = FileWatcher::start(root_path.to_path_buf(), tx)?;

    // Canonicalize the root exactly as the reindex walker does (issue #402).
    // `std::fs::canonicalize` resolves symlinks so that the macOS `/var` →
    // `/private/var` alias (and similar) never cause a prefix-mismatch when
    // the notify event path and the stored root differ only by symlink target.
    // Fall back to the raw path when canonicalization fails (mount unmounted,
    // permission error) — matching the reindex fallback in `validate.rs`.
    let canonical_root =
        std::fs::canonicalize(root_path).unwrap_or_else(|_| root_path.to_path_buf());

    let join = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                WatchEvent::Modified(path) => {
                    handle_modified(&path, &canonical_root, &indexer, &indexed_files).await;
                }
                WatchEvent::Removed(path) => {
                    handle_removed(&path, &indexer, &indexed_files).await;
                }
            }
        }
    });

    Ok(WatcherTask {
        _watcher: watcher,
        _join: join,
    })
}

/// Normalize an absolute watcher event path to a repo-root-relative string,
/// matching the path convention used by the reindex pipeline (issue #402).
///
/// Why: `notify` delivers absolute filesystem paths (e.g.
/// `/Volumes/SSD1/proj/src/lib.rs`). The reindex walker stores paths
/// *relative* to the canonical index root (e.g. `src/lib.rs`) via
/// `strip_prefix`. When the watcher stored absolute paths instead, branch
/// boosting (`set.contains("src/lib.rs")`) silently failed and the index
/// became non-portable across worktrees or CI machines.
///
/// What: canonicalizes `event_path` (resolving symlinks, e.g. macOS
/// `/var` → `/private/var`) then strips `canonical_root` as a prefix. If
/// stripping succeeds, returns the relative path string. If the event path
/// is genuinely outside the root (a symlink target outside the tree, a
/// cross-device notification glitch), falls back to the raw
/// `event_path.to_string_lossy()` — matching the `unwrap_or` in the reindex
/// `strip_prefix` call.
///
/// Test: unit tests at the bottom of this module cover the normal case,
/// nested subdirs, files outside root, and an optional symlink-root test.
pub fn watcher_relative_path(canonical_root: &Path, event_path: &Path) -> String {
    // Canonicalize the event path to resolve any symlink components.  On
    // macOS `/var/folders/...` is a symlink to `/private/var/...`; without
    // this canonicalization the `strip_prefix` below would fail because
    // `canonical_root` was already resolved to `/private/...`.
    //
    // `canonicalize` can fail if the file was deleted between the event and
    // this call (race window); in that case we fall back to the raw path.
    // `strip_prefix` will still succeed as long as the raw path and root
    // share the same byte prefix — safe on most real filesystems where
    // the only divergence is a macOS-style symlink alias at the root.
    let canonical_event =
        std::fs::canonicalize(event_path).unwrap_or_else(|_| event_path.to_path_buf());

    canonical_event
        .strip_prefix(canonical_root)
        .unwrap_or(&canonical_event)
        .to_string_lossy()
        .into_owned()
}

/// Re-chunk the file and merge it into the indexer. Stale chunks from a
/// previous version of the same file are removed first so we don't accumulate
/// dead entries on edit.
async fn handle_modified(
    path: &Path,
    canonical_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
) {
    // Skip directories — the watcher fires on parent mtime updates too.
    if path.is_dir() {
        return;
    }

    // Apply the same exclusions as the recursive walker: a file modified
    // inside an excluded subtree (e.g. `cdk.out/`, `node_modules/`) or a
    // minified/binary/large file must not enter the index incrementally.
    //
    // Issue #118: the v0.8.2 watcher additionally filtered `.md` /
    // CHANGELOG / LICENSE edits via `is_default_doc_excluded` to mirror
    // the reindex-time exclusion. With the reindex default flipped to
    // `include_docs: true`, the watcher must follow so live doc edits
    // don't go stale. The per-mode `is_allowed_for_mode` filter still
    // gates the docs out of `mode=code` results, so this only widens
    // text-mode coverage.
    if path_in_skipped_dir(path) || should_skip_path(path) {
        tracing::debug!(?path, "skip excluded file");
        return;
    }

    let content = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(err) => {
            tracing::debug!(?err, ?path, "skip unreadable file");
            return;
        }
    };

    // Drop any prior chunks for this file before re-indexing.
    if let Some(stale_ids) = indexed_files.take(path).await {
        let idx = indexer.read().await;
        for id in stale_ids {
            if let Err(err) = idx.remove_chunk(&id).await {
                tracing::warn!(?err, %id, "remove_chunk failed");
            }
        }
    }

    // Issue #402 — normalize to repo-root-relative path before indexing.
    // The reindex pipeline strips `root_path` from every walked file so the
    // corpus stores portable relative paths (e.g. `src/lib.rs`).  The
    // watcher previously forwarded the absolute event path (e.g.
    // `/Volumes/SSD1/.../src/lib.rs`), diverging from that convention and
    // silently breaking branch-boost (`set.contains("src/lib.rs")`) as well
    // as making the index non-portable across worktrees and CI machines.
    let path_str = watcher_relative_path(canonical_root, path);
    let (chunks, _entities) = chunk_ast(&path_str, &content);
    let new_ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();

    let idx = indexer.read().await;
    if let Err(err) = idx.index_file(&path_str, &content).await {
        tracing::warn!(?err, ?path, "index_file failed");
        return;
    }
    drop(idx);

    indexed_files.record(path.to_path_buf(), new_ids).await;
}

/// Drop every chunk we previously recorded for `path` from the indexer.
async fn handle_removed(
    path: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
) {
    let Some(ids) = indexed_files.take(path).await else {
        return;
    };
    let idx = indexer.read().await;
    for id in ids {
        if let Err(err) = idx.remove_chunk(&id).await {
            tracing::warn!(?err, %id, "remove_chunk failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::sync::RwLock;

    // ── Pure unit tests for `watcher_relative_path` ──────────────────────────

    /// Why: the primary fix for issue #402 — a file directly inside the root
    /// must be stored as a bare relative name, not the absolute path.
    /// Test: this test.
    #[test]
    fn watcher_relative_path_strips_root_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
        let file = root.join("lib.rs");
        // Create the file so canonicalize works on it.
        std::fs::write(&file, "").expect("create file");
        let rel = watcher_relative_path(&root, &file);
        assert_eq!(rel, "lib.rs", "expected bare filename, got {rel:?}");
        assert!(
            !rel.starts_with('/'),
            "relative path must not start with '/', got {rel:?}"
        );
    }

    /// Why: files nested under subdirectories must produce multi-component
    /// relative paths (e.g. `src/auth/mod.rs`), not just the basename.
    /// Test: this test.
    #[test]
    fn watcher_relative_path_preserves_subdirectory_structure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
        let subdir = root.join("src").join("auth");
        std::fs::create_dir_all(&subdir).expect("create subdir");
        let file = subdir.join("mod.rs");
        std::fs::write(&file, "").expect("create file");
        let rel = watcher_relative_path(&root, &file);
        assert_eq!(
            rel,
            PathBuf::from("src")
                .join("auth")
                .join("mod.rs")
                .display()
                .to_string(),
            "expected src/auth/mod.rs, got {rel:?}"
        );
        assert!(
            !rel.starts_with('/'),
            "relative path must not start with '/', got {rel:?}"
        );
    }

    /// Why: a notify event for a file outside the index root (symlink target
    /// outside tree, cross-device glitch) must fall back to the raw/canonical
    /// path rather than panicking or returning an empty string. This matches
    /// the reindex `unwrap_or(&path)` convention.
    /// Test: this test.
    #[test]
    fn watcher_relative_path_falls_back_for_file_outside_root() {
        let root_dir = tempfile::tempdir().expect("tempdir root");
        let other_dir = tempfile::tempdir().expect("tempdir other");
        let root = std::fs::canonicalize(root_dir.path()).expect("canonicalize root");
        let outside = other_dir.path().join("x.rs");
        std::fs::write(&outside, "").expect("create outside file");
        let result = watcher_relative_path(&root, &outside);
        // Must not start with the root prefix.
        assert!(
            !result.starts_with(root.to_str().unwrap_or("")),
            "result must not start with root when file is outside: {result:?}"
        );
        // Must be non-empty — some path representation must survive.
        assert!(!result.is_empty(), "result must not be empty");
    }

    /// Why: on macOS, `/var` is a symlink to `/private/var`. If `notify` delivers
    /// a path under the real target (`/private/var/...`) while the root was
    /// registered under the symlink (`/var/...`) — or vice versa — the prefix
    /// stripping must still succeed. This test verifies the canonicalization
    /// step handles that case.
    #[cfg(unix)]
    #[test]
    fn watcher_relative_path_resolves_symlinked_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("create real");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        // File is under the real directory.
        let file = real.join("foo.rs");
        std::fs::write(&file, "").expect("create file");

        // Root is the symlink alias — simulates how an operator might register
        // the index under a symlinked path.
        let canonical_root = std::fs::canonicalize(&link).expect("canonicalize link");
        let rel = watcher_relative_path(&canonical_root, &file);
        assert_eq!(
            rel, "foo.rs",
            "expected bare filename through symlink, got {rel:?}"
        );
        assert!(
            !rel.starts_with('/'),
            "relative path must not start with '/', got {rel:?}"
        );
    }

    /// End-to-end: writing a `.rs` file inside a watched directory causes the
    /// indexer's chunk count to grow within ~2s.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn modified_file_triggers_indexing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new("test", dir.path())));
        let tracker = IndexedFiles::new();

        let _task = spawn_watch_loop(dir.path(), Arc::clone(&indexer), tracker.clone())
            .expect("watch loop starts");

        // Allow the OS watcher to install.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let file = dir.path().join("lib.rs");
        tokio::fs::write(&file, "fn alpha() {}\nfn beta() {}\n")
            .await
            .expect("write file");

        // Poll up to 2s for the indexer to pick the change up.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let count = indexer.read().await.chunk_count();
            if count > 0 {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("chunk_count never grew above 0");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert!(
            tracker.len().await >= 1,
            "expected at least one tracked file"
        );
    }

    /// Issue #129: a file created inside `cdk.out/` must NOT be indexed by the
    /// watcher — the build-artefact subtree exclusion applies incrementally.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cdk_out_file_is_not_indexed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let indexer = Arc::new(RwLock::new(CodeIndexer::new("test", dir.path())));
        let tracker = IndexedFiles::new();

        let _task = spawn_watch_loop(dir.path(), Arc::clone(&indexer), tracker.clone())
            .expect("watch loop starts");

        tokio::time::sleep(Duration::from_millis(150)).await;

        // Write a real source file and a build-artefact file.
        let cdk_dir = dir.path().join("cdk.out/asset.abc/python");
        tokio::fs::create_dir_all(&cdk_dir).await.expect("mkdir");
        tokio::fs::write(cdk_dir.join("vendored.py"), "import boto3\n")
            .await
            .expect("write vendored");
        tokio::fs::write(dir.path().join("handler.py"), "def handler(): pass\n")
            .await
            .expect("write handler");

        // Poll for the real file to be picked up.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if indexer.read().await.chunk_count() > 0 {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("real source was never indexed");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Give the watcher a moment to (not) process the cdk.out file.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Only handler.py should be tracked; vendored.py must be excluded.
        let tracked = tracker.len().await;
        assert_eq!(
            tracked, 1,
            "exactly one file (handler.py) should be tracked, got {tracked}"
        );
    }
}
