//! Staging corpora left behind by earlier reindex runs (#8889).
//!
//! Why: every run now stages into its own `index.redb.run-*.tmp` (see
//! `claim::ReindexClaim::staging_file_name`), so a crashed run's file is no
//! longer overwritten by the next run's `open_fresh`. Something has to adopt it
//! (the #3979 resume) or delete it, and neither may touch the running run's own
//! file. A leftover the daemon cannot remove — a directory, a permission error —
//! is logged and skipped: it no longer shares a path with any new run, so it
//! cannot break one.
//! What: [`is_staging_leftover`] names the pattern (the per-run form plus the
//! pre-#8889 `index.redb.tmp`); [`adopt_newest`] moves the newest leftover file
//! onto a run's own staging path for resume; [`sweep`] deletes the rest.
//! Test: `resume_tests::a_leftover_staging_directory_does_not_break_the_next_run`
//! and `resume_tests` (a planted legacy `index.redb.tmp` is adopted).

use std::path::{Path, PathBuf};

use crate::core::registry::IndexId;

/// Prefix of a per-run staging corpus file name.
pub(crate) const RUN_STAGING_PREFIX: &str = "index.redb.run-";
/// Suffix of a per-run staging corpus file name.
pub(crate) const RUN_STAGING_SUFFIX: &str = ".tmp";

/// True for a staging corpus name some run may have left behind.
pub(crate) fn is_staging_leftover(name: &str) -> bool {
    name == crate::service::storage_layout::REDB_TMP_FILE
        || (name.starts_with(RUN_STAGING_PREFIX) && name.ends_with(RUN_STAGING_SUFFIX))
}

/// Leftover staging entries beside `own`, newest first, excluding `own` itself.
///
/// What: lists `own`'s parent directory; an unreadable directory yields none.
/// Entries whose mtime cannot be read sort last.
fn leftovers(own: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let Some(dir) = own.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, std::fs::Metadata, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(is_staging_leftover))
        .filter(|e| e.path() != own)
        .filter_map(|e| {
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((e.path(), meta, mtime))
        })
        .collect();
    found.sort_by_key(|entry| std::cmp::Reverse(entry.2));
    found.into_iter().map(|(p, m, _)| (p, m)).collect()
}

/// Remove one leftover; a failure is logged and ignored.
fn remove(path: &Path, meta: &std::fs::Metadata, index_id: &IndexId) {
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => tracing::info!(
            "reindex[{}]: removed leftover staging corpus {} (#8889)",
            index_id.0,
            path.display()
        ),
        Err(e) => tracing::warn!(
            "reindex[{}]: could not remove leftover staging corpus {} ({e}); it shares no \
             path with this run, so the run proceeds (#8889)",
            index_id.0,
            path.display()
        ),
    }
}

/// Delete every leftover staging entry beside `own` (#8889).
///
/// Why: with per-run names a crashed run's file is never overwritten, so it must
/// be deleted explicitly or it leaks disk.
/// What: best-effort removal on a blocking worker. The caller must hold the
/// index's reindex claim, which is what guarantees no live run owns any of them.
/// Test: `resume_tests::a_leftover_staging_directory_does_not_break_the_next_run`.
pub(crate) async fn sweep(own: &Path, index_id: &IndexId) {
    let own = own.to_path_buf();
    let id = index_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for (path, meta) in leftovers(&own) {
            remove(&path, &meta, &id);
        }
    })
    .await;
}

/// Move the newest leftover staging FILE onto `own`, deleting the others (#8889).
///
/// Why: #3979 resumes an interrupted run from its staging corpus. The resumed
/// run takes the file over under its own name, so from here on only this run
/// writes to it.
/// What: returns `true` when `own` exists afterwards (already there, or a
/// leftover was renamed onto it). Directories and symlinks are never adopted.
/// A failed rename leaves `own` absent, so the caller rebuilds from scratch.
/// Test: `resume_tests::interrupted_reindex_resumes_to_identical_index`.
pub(crate) async fn adopt_newest(own: &Path, index_id: &IndexId) -> bool {
    let own = own.to_path_buf();
    let id = index_id.clone();
    tokio::task::spawn_blocking(move || {
        let mut adopted = own.exists();
        for (path, meta) in leftovers(&own) {
            if !adopted && meta.is_file() {
                match std::fs::rename(&path, &own) {
                    Ok(()) => {
                        tracing::info!(
                            "reindex[{}]: took over leftover staging corpus {} as {} (#8889)",
                            id.0,
                            path.display(),
                            own.display()
                        );
                        adopted = true;
                        continue;
                    }
                    Err(e) => tracing::warn!(
                        "reindex[{}]: could not take over leftover staging corpus {} ({e})",
                        id.0,
                        path.display()
                    ),
                }
            }
            remove(&path, &meta, &id);
        }
        adopted
    })
    .await
    .unwrap_or(false)
}
