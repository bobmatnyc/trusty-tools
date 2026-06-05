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
    //
    // We keep the raw root too so the deleted-file fallback in
    // `watcher_relative_path` can strip against both canonical and raw forms
    // (the file is gone, so canonicalize of the event path fails and we must
    // try both root variants to avoid an absolute-path mismatch).
    let raw_root = root_path.to_path_buf();
    let canonical_root =
        std::fs::canonicalize(root_path).unwrap_or_else(|_| root_path.to_path_buf());

    let join = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                WatchEvent::Modified(path) => {
                    handle_modified(&path, &canonical_root, &raw_root, &indexer, &indexed_files)
                        .await;
                }
                WatchEvent::Removed(path) => {
                    handle_removed(&path, &canonical_root, &raw_root, &indexer, &indexed_files)
                        .await;
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
/// For `WatchEvent::Removed` the file is already gone, so
/// `std::fs::canonicalize(event_path)` fails and we fall back to the raw
/// event path. On macOS, `notify` may deliver the path under the raw root
/// (e.g. `/var/folders/…`) while `canonical_root` was resolved to
/// `/private/var/folders/…` (or vice versa). The dual-root fallback
/// (`canonical_root` then `raw_root`) ensures at least one strip succeeds so
/// we always return a relative key rather than an absolute path.
///
/// What: (1) canonicalize `event_path` and strip `canonical_root`. (2) If
/// canonicalization fails (file deleted), attempt `strip_prefix` against
/// both `canonical_root` and `raw_root` on the raw event path. (3) If the
/// file is genuinely outside both roots (symlink target outside the tree,
/// cross-device glitch), fall back to `event_path.to_string_lossy()`.
///
/// Test: unit tests at the bottom of this module cover the normal case,
/// nested subdirs, files outside root, the symlink-root case, deleted-file
/// dual-root fallback, and consistency between Modified and Removed arms.
pub fn watcher_relative_path(canonical_root: &Path, raw_root: &Path, event_path: &Path) -> String {
    // Fast path: file still exists → canonicalize resolves symlinks.
    // On macOS `/var/folders/…` → `/private/var/folders/…`; canonical root
    // was already resolved, so strip_prefix reliably hits.
    if let Ok(canonical_event) = std::fs::canonicalize(event_path) {
        if let Ok(rel) = canonical_event.strip_prefix(canonical_root) {
            return rel.to_string_lossy().into_owned();
        }
        // Canonical event exists but is outside canonical_root (genuinely
        // out-of-root file). Fall through to the string fallback below.
        return canonical_event.to_string_lossy().into_owned();
    }

    // Slow path: canonicalize failed — file was deleted before this call.
    // Try stripping both the canonical root and the raw root against the
    // raw event path so a macOS /var↔/private/var mismatch doesn't leave
    // an absolute key when one form matches and the other does not.
    if let Ok(rel) = event_path.strip_prefix(canonical_root) {
        return rel.to_string_lossy().into_owned();
    }
    if let Ok(rel) = event_path.strip_prefix(raw_root) {
        return rel.to_string_lossy().into_owned();
    }

    // Out-of-root or unknown path — preserve the raw string as-is,
    // matching the reindex `unwrap_or(&path)` convention.
    event_path.to_string_lossy().into_owned()
}

/// Re-chunk the file and merge it into the indexer. Stale chunks from a
/// previous version of the same file are removed first so we don't accumulate
/// dead entries on edit.
async fn handle_modified(
    path: &Path,
    canonical_root: &Path,
    raw_root: &Path,
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

    // Issue #402 — normalize to repo-root-relative path before indexing.
    // The reindex pipeline strips `root_path` from every walked file so the
    // corpus stores portable relative paths (e.g. `src/lib.rs`).  The
    // watcher previously forwarded the absolute event path (e.g.
    // `/Volumes/SSD1/.../src/lib.rs`), diverging from that convention and
    // silently breaking branch-boost (`set.contains("src/lib.rs")`) as well
    // as making the index non-portable across worktrees and CI machines.
    //
    // Compute the relative key first so the stale-chunk removal and the
    // subsequent record both use the same key (the relative path string).
    // This also ensures a subsequent Removed event — which computes the same
    // relative key — finds the entry even when `notify` delivers a different
    // symlink form for the delete event.
    let path_str = watcher_relative_path(canonical_root, raw_root, path);

    // Drop any prior chunks for this file before re-indexing.
    if let Some(stale_ids) = indexed_files
        .take(&std::path::PathBuf::from(&path_str))
        .await
    {
        let idx = indexer.read().await;
        for id in stale_ids {
            if let Err(err) = idx.remove_chunk(&id).await {
                tracing::warn!(?err, %id, "remove_chunk failed");
            }
        }
    }
    let (chunks, _entities) = chunk_ast(&path_str, &content);
    let new_ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();

    let idx = indexer.read().await;
    if let Err(err) = idx.index_file(&path_str, &content).await {
        tracing::warn!(?err, ?path, "index_file failed");
        return;
    }
    drop(idx);

    // Key by the relative path string so Removed events (which compute the
    // same relative key) find the entry even when `notify` delivers a
    // different symlink form for the delete event.
    indexed_files
        .record(std::path::PathBuf::from(&path_str), new_ids)
        .await;
}

/// Drop every chunk we previously recorded for `path` from the indexer.
///
/// Why: `WatchEvent::Removed` fires after a file is deleted. We must look up
/// the chunk IDs that were recorded when the file was indexed and evict them
/// from the HNSW + BM25 corpus so deleted files do not silently linger.
///
/// What: normalizes the event path to the same repo-root-relative key used by
/// `handle_modified` when it recorded the chunks, then calls `remove_chunk`
/// for every chunk ID in the index. Uses `watcher_relative_path` with both
/// the canonical and raw roots so that even when `notify` delivers the path
/// in a different symlink form (e.g. `/var/…` vs `/private/var/…` on macOS)
/// the lookup still hits the entry stored by `handle_modified`.
///
/// Test: `removed_event_produces_same_relative_key_as_modified` and
/// `removed_deleted_file_dual_root_fallback` unit tests below.
async fn handle_removed(
    path: &Path,
    canonical_root: &Path,
    raw_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
) {
    // Compute the same relative key that handle_modified stored so the
    // lookup succeeds even when notify delivers a different symlink form
    // for the delete event (e.g. /var vs /private/var on macOS).
    let rel_key = watcher_relative_path(canonical_root, raw_root, path);
    let Some(ids) = indexed_files
        .take(&std::path::PathBuf::from(&rel_key))
        .await
    else {
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
        std::fs::write(&file, "").expect("create file");
        let rel = watcher_relative_path(&root, &root, &file);
        assert_eq!(rel, "lib.rs", "expected bare filename, got {rel:?}");
        assert!(!rel.starts_with('/'), "must not start with '/'");
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
        let rel = watcher_relative_path(&root, &root, &file);
        assert_eq!(
            rel,
            PathBuf::from("src")
                .join("auth")
                .join("mod.rs")
                .display()
                .to_string(),
            "expected src/auth/mod.rs"
        );
        assert!(!rel.starts_with('/'), "must not start with '/'");
    }

    /// Why: a notify event for a file outside the index root (symlink target
    /// outside tree, cross-device glitch) must fall back to the raw/canonical
    /// path rather than panicking or returning an empty string.
    /// Test: this test.
    #[test]
    fn watcher_relative_path_falls_back_for_file_outside_root() {
        let root_dir = tempfile::tempdir().expect("tempdir root");
        let other_dir = tempfile::tempdir().expect("tempdir other");
        let root = std::fs::canonicalize(root_dir.path()).expect("canonicalize root");
        let outside = other_dir.path().join("x.rs");
        std::fs::write(&outside, "").expect("create outside file");
        let result = watcher_relative_path(&root, &root, &outside);
        assert!(
            !result.starts_with(root.to_str().unwrap_or("")),
            "result must not start with root: {result:?}"
        );
        assert!(!result.is_empty(), "result must not be empty");
    }

    /// Why: on macOS `/var` is a symlink to `/private/var`; canonicalization
    /// must resolve both forms so strip_prefix succeeds.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn watcher_relative_path_resolves_symlinked_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("create real");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let file = real.join("foo.rs");
        std::fs::write(&file, "").expect("create file");
        let canonical_root = std::fs::canonicalize(&link).expect("canonicalize link");
        let rel = watcher_relative_path(&canonical_root, &link, &file);
        assert_eq!(rel, "foo.rs", "expected bare filename, got {rel:?}");
        assert!(!rel.starts_with('/'), "must not start with '/'");
    }

    /// Why: a Removed-style event with an absolute in-root path must normalize
    /// to the relative key, matching what Modified stored. This is the primary
    /// guard against deleted files silently lingering in the index (issue #804).
    /// Test: this test.
    #[test]
    fn removed_event_produces_same_relative_key_as_modified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw_root = dir.path().to_path_buf();
        let canonical_root = std::fs::canonicalize(&raw_root).expect("canonicalize");
        // Simulate file existing (Modified arm) then deleted (Removed arm).
        let abs_path = canonical_root.join("src").join("lib.rs");
        std::fs::create_dir_all(abs_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&abs_path, "").expect("write");
        let modified_key = watcher_relative_path(&canonical_root, &raw_root, &abs_path);
        // Now delete the file and compute the Removed-arm key.
        std::fs::remove_file(&abs_path).expect("remove");
        let removed_key = watcher_relative_path(&canonical_root, &raw_root, &abs_path);
        assert_eq!(
            modified_key, removed_key,
            "Removed arm key {removed_key:?} must equal Modified arm key {modified_key:?}"
        );
        assert!(
            !removed_key.starts_with('/'),
            "must be relative: {removed_key:?}"
        );
    }

    /// Why: when the deleted file's path uses the raw root form (e.g. `/var/…`)
    /// while `canonical_root` is `/private/var/…`, the dual-root strip must
    /// still yield a relative key rather than an absolute path.
    /// Test: this test exercises the raw-root fallback branch deterministically
    /// by constructing a scenario where the event path is under `raw_root` but
    /// `canonical_root` is a different resolved path (symlink target).
    #[cfg(unix)]
    #[test]
    fn removed_deleted_file_dual_root_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("proj");
        std::fs::create_dir(&real).expect("create proj");
        let link = dir.path().join("proj-link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        // canonical_root resolves through the symlink; raw_root is the link.
        let canonical_root = std::fs::canonicalize(&link).expect("canonicalize");
        let raw_root = link.clone();
        // Event path is under the raw root (link form), file is now deleted.
        let event_path = raw_root.join("src").join("main.rs");
        // Do NOT create event_path — it must not exist, simulating a deletion.
        // canonicalize(event_path) will fail; fallback must try raw_root.
        let result = watcher_relative_path(&canonical_root, &raw_root, &event_path);
        assert_eq!(
            result,
            PathBuf::from("src").join("main.rs").display().to_string(),
            "dual-root fallback must yield relative key, got {result:?}"
        );
        assert!(!result.starts_with('/'), "must be relative: {result:?}");
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
