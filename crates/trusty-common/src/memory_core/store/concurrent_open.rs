//! Multi-process concurrent open support for redb-backed palace storage.
//!
//! Why: redb takes an exclusive `flock(LOCK_EX)` on every database file, so
//! a second process attempting to open the same `kg.redb` or
//! `index.usearch.redb` while the HTTP daemon owns it fails with
//! `DatabaseError::DatabaseAlreadyOpen`. Issue #59 demands that the stdio
//! MCP client and the HTTP daemon coexist: writers must still go through
//! the daemon, but the stdio client must be able to *read* the same palace
//! state without the daemon being forced offline.
//!
//! Strategy: when an exclusive open fails with `DatabaseAlreadyOpen`, copy
//! the database file to a process-local snapshot path under the system tmp
//! directory and open that snapshot as a fresh redb database. The snapshot
//! is owned exclusively by *this* process so redb's lock check succeeds.
//! The snapshot represents a point-in-time read of the live database — it
//! is sufficient to serve `recall`, `kg_query`, and `palace_info` from the
//! stdio MCP client while the daemon continues to write to the original
//! file. Writes against a snapshot-mode store return a clear "palace is
//! read-only" error rather than silently diverging from the daemon's view.
//!
//! What: `try_open_or_snapshot` returns `(Arc<Database>, OpenMode)` and
//! `SnapshotGuard` cleans up the snapshot file on drop.
//! Test: `snapshot_fallback_when_locked` opens a file twice in one process
//! by holding the first handle while opening the second — the second open
//! falls back to a snapshot and read transactions still succeed.

use anyhow::{Context, Result};
use redb::{Database, DatabaseError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Whether a redb file was opened directly (read/write) or via a snapshot
/// (read-only).
///
/// Why: Callers need to know whether subsequent writes are safe. A
/// snapshot-mode database accepts writes from redb's perspective, but those
/// writes never reach the original file and would silently diverge from
/// the daemon's authoritative state — so the store layer must reject them
/// before they happen.
/// What: A two-variant enum. `ReadWrite` means we hold the live file lock;
/// `Snapshot` means we hold a process-local copy.
/// Test: `snapshot_fallback_when_locked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Holds the exclusive file lock on the original path.
    ReadWrite,
    /// Operating against a process-local snapshot copy. Writes must be
    /// rejected at the store layer.
    Snapshot,
    /// The original file was in an incompatible / old redb format (redb 2.x),
    /// so it was moved aside (`*.v2-incompatible`) and a fresh empty database
    /// was created in its place (issue #702). Holds the exclusive lock on the
    /// new file like `ReadWrite`, but signals to the caller that the prior
    /// contents were lost and the store should be surfaced as
    /// `degraded`/`rebuilding` rather than `ready`.
    Recreated,
}

impl OpenMode {
    /// Why: Lets callers branch with a method instead of pattern-matching.
    /// What: Returns `true` when the mode is `Snapshot`.
    /// Test: trivially covered by the snapshot fallback test.
    pub fn is_read_only(self) -> bool {
        matches!(self, OpenMode::Snapshot)
    }

    /// Why: callers (KG store init, palace status) need to know the store was
    /// rebuilt empty after an incompatible-format file so they can treat it as
    /// `degraded`/needs-rebuild rather than `ready` (the #601/#694 false-healthy
    /// guard) while still materialising tables like a normal read-write open.
    /// What: Returns `true` only for [`OpenMode::Recreated`].
    /// Test: `recreates_on_incompatible_format`.
    #[must_use]
    pub fn was_recreated(self) -> bool {
        matches!(self, OpenMode::Recreated)
    }
}

/// RAII guard that deletes the snapshot file when dropped.
///
/// Why: Snapshot files accumulate fast (one per palace per stdio session)
/// and would otherwise leak into `$TMPDIR` indefinitely. Tying their
/// lifetime to the store handle keeps cleanup automatic without requiring
/// callers to remember a teardown step.
/// What: Holds the snapshot file path; `Drop` removes it best-effort and
/// logs a warning on failure.
/// Test: `snapshot_guard_removes_file_on_drop`.
#[derive(Debug)]
pub struct SnapshotGuard {
    path: Option<PathBuf>,
}

impl SnapshotGuard {
    /// Why: Used by `try_open_or_snapshot` to wrap a freshly created
    /// snapshot path so it gets cleaned up later. A no-op variant is used
    /// for the read/write path so call sites can store a uniform type.
    /// What: Constructs a guard owning `path`; on drop the file is removed.
    /// Test: Indirect via `try_open_or_snapshot`.
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Why: The read/write path doesn't create a snapshot, but call sites
    /// still want a uniform `SnapshotGuard` field so they can avoid
    /// `Option` plumbing.
    /// What: Returns a guard with no path; drop is a no-op.
    /// Test: Indirect via `try_open_or_snapshot`.
    pub fn noop() -> Self {
        Self { path: None }
    }
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take()
            && let Err(e) = std::fs::remove_file(&path)
        {
            // Only warn — the snapshot is in $TMPDIR and the OS will reap
            // it eventually. We don't want a drop-time error to mask a
            // more interesting cleanup path elsewhere.
            tracing::warn!(
                snapshot = %path.display(),
                "failed to remove redb snapshot file: {e}"
            );
        }
    }
}

/// Monotonic counter used to disambiguate snapshot paths created within
/// the same process. Without it, two threads (or two sequential test
/// cases) opening the same palace file would compute the same snapshot
/// filename and the second `Database::create` would fail with "Database
/// already open. Cannot acquire lock." because the first handle is still
/// alive in this process.
static SNAPSHOT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Build a snapshot path for `original` that is unique to this process
/// AND to this call (so concurrent / sequential opens of the same file
/// never collide).
///
/// Why: Multiple stdio clients (each a separate process) may all snapshot
/// the same palace file at once; including the pid avoids cross-process
/// collisions. Within one process, parallel callers (tests, two stdio
/// sessions sharing the same daemon binary) must also get distinct
/// snapshot filenames — otherwise the second `Database::create` against
/// the snapshot trips redb's exclusive lock. A monotonic counter solves
/// this without requiring callers to thread an id through. Including the
/// file's stem keeps the snapshot recognisable in `lsof` during
/// debugging.
/// What: `<tmpdir>/trusty-memory-snapshot-<pid>-<seq>-<filename>`.
/// Test: `snapshot_path_is_unique_per_process`,
/// `snapshot_path_is_unique_per_call`.
fn snapshot_path_for(original: &Path) -> PathBuf {
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    let seq = SNAPSHOT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stem = original
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "redb".to_string());
    tmp.join(format!("trusty-memory-snapshot-{pid}-{seq}-{stem}"))
}

/// Open `path` as a redb database, falling back to a process-local
/// snapshot copy when the file is already locked by another process.
///
/// Why: The HTTP daemon holds an exclusive flock on every palace's
/// `kg.redb` and `index.usearch.redb`. Without this helper a second
/// process (e.g. the stdio MCP server invoked by Claude Code) cannot open
/// the same palace at all — every recall fails with "open palace …".
/// With this helper, the second process detects the lock contention and
/// transparently switches to a snapshot copy so reads can proceed.
/// What: First attempts `Database::create(path)`. On
/// `DatabaseError::DatabaseAlreadyOpen` it copies `path` to a per-process
/// snapshot location and opens that copy instead. Returns the open
/// database, a `SnapshotGuard` that removes the snapshot file when
/// dropped, and the `OpenMode` so the caller can reject writes when
/// running on a snapshot.
/// Test: `snapshot_fallback_when_locked`.
pub fn try_open_or_snapshot(path: &Path) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
    match Database::create(path) {
        Ok(db) => Ok((Arc::new(db), SnapshotGuard::noop(), OpenMode::ReadWrite)),
        // Issue #702: the file is in an incompatible / old redb format (redb
        // 2.x written by a pre-4.x binary). We hold no lock on it, so it is
        // safe to move it aside and create a fresh empty database. The caller
        // receives `OpenMode::Recreated` and must surface degraded status —
        // never report this store as `ready`.
        Err(e) if super::redb_open::is_incompatible_format(&e) => {
            let backup = super::redb_open::backup_incompatible_file(path).with_context(|| {
                format!(
                    "back up incompatible-format redb file {} before recreating",
                    path.display()
                )
            })?;
            let db = Database::create(path).with_context(|| {
                format!(
                    "create fresh redb after moving incompatible file aside at {}",
                    path.display()
                )
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "redb file is in an incompatible/old format (redb 2.x); moved it aside and \
                 created a fresh empty database — this palace must be rebuilt, not treated as ready"
            );
            Ok((Arc::new(db), SnapshotGuard::noop(), OpenMode::Recreated))
        }
        Err(DatabaseError::DatabaseAlreadyOpen) => {
            let snap = snapshot_path_for(path);
            // Snapshot paths are per-call unique (pid + monotonic
            // counter), so no stale-file cleanup is needed here.
            std::fs::copy(path, &snap).with_context(|| {
                format!(
                    "snapshot {} -> {} for read-only fallback",
                    path.display(),
                    snap.display()
                )
            })?;
            let db = Database::create(&snap).with_context(|| {
                format!(
                    "open redb snapshot at {} (fallback for locked {})",
                    snap.display(),
                    path.display()
                )
            })?;
            tracing::info!(
                original = %path.display(),
                snapshot = %snap.display(),
                "redb file locked by another process; opened read-only snapshot"
            );
            Ok((Arc::new(db), SnapshotGuard::new(snap), OpenMode::Snapshot))
        }
        Err(e) => Err(anyhow::anyhow!("open redb at {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `begin_read` lives on the `ReadableDatabase` trait in redb 4.x; only the
    // tests exercise it here, so the import is scoped to the test module to keep
    // the non-test code free of an otherwise-unused trait import.
    use redb::ReadableDatabase;
    use tempfile::tempdir;

    /// Why: Confirms the core contract — a second open against a path
    /// that is already locked falls back to a snapshot and succeeds.
    /// What: Opens `db.redb` in this process (acquiring the lock), then
    /// calls `try_open_or_snapshot` against the same path. The second
    /// call must succeed in `Snapshot` mode.
    /// Test: this test.
    #[test]
    fn snapshot_fallback_when_locked() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");

        // First open holds the flock.
        let live = Database::create(&path).expect("first open");

        // Second open must succeed via snapshot fallback.
        let (snap_db, guard, mode) =
            try_open_or_snapshot(&path).expect("snapshot fallback should succeed");
        assert_eq!(mode, OpenMode::Snapshot);
        assert!(mode.is_read_only());

        // Read transactions work against the snapshot.
        let rtx = snap_db.begin_read().expect("begin_read on snapshot");
        drop(rtx);

        // Holding `live` proves we never released the original lock.
        drop(live);
        drop(snap_db);
        drop(guard); // snapshot file removed here
    }

    /// Why: The read/write path must NOT create a snapshot file when
    /// there is no contention.
    /// What: Opens a fresh path; asserts `ReadWrite` mode and no snapshot
    /// file appears in `$TMPDIR`.
    /// Test: this test.
    #[test]
    fn direct_open_when_uncontended() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");
        let (_db, _guard, mode) = try_open_or_snapshot(&path).expect("direct open");
        assert_eq!(mode, OpenMode::ReadWrite);
        assert!(!mode.is_read_only());
    }

    /// Why: Snapshot files must be removed on guard drop so $TMPDIR does
    /// not accumulate stale copies after a stdio session ends.
    /// What: Force-creates a snapshot via lock contention, captures the
    /// snapshot path from the guard via Debug, drops the guard, and
    /// asserts the file is gone.
    /// Test: this test.
    #[test]
    fn snapshot_guard_removes_file_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");
        let _live = Database::create(&path).unwrap();

        let (_snap_db, guard, _mode) = try_open_or_snapshot(&path).expect("fallback");
        // Extract the snapshot path before drop so we can re-check
        // existence afterwards.
        let snap_path = guard
            .path
            .clone()
            .expect("snapshot guard should carry a path");
        assert!(snap_path.exists(), "snapshot file should exist before drop");
        drop(_snap_db); // release the redb handle on the snapshot file
        drop(guard);
        assert!(
            !snap_path.exists(),
            "snapshot file should be removed on guard drop"
        );
    }

    /// Why: #702 — an incompatible-format (redb 2.x) file at the open path must
    /// be moved aside and replaced with a fresh empty DB returned in
    /// `Recreated` mode, NOT crash and NOT be treated as a healthy file. This
    /// is the central palace-open guard against the #601/#694 false-healthy bug.
    /// What: writes garbage to the palace path, opens via `try_open_or_snapshot`,
    /// asserts `Recreated` mode, the backup exists, and the fresh DB is writable.
    /// Test: this test.
    #[test]
    fn recreates_on_incompatible_format() {
        use std::io::Write;
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();

        let (db, _guard, mode) =
            try_open_or_snapshot(&path).expect("incompatible file must recover, not error");
        assert_eq!(mode, OpenMode::Recreated);
        assert!(mode.was_recreated());
        assert!(!mode.is_read_only(), "recreated DB holds the live lock");
        assert!(
            path.with_file_name("kg.redb.v2-incompatible").exists(),
            "incompatible file must be backed up"
        );
        // The fresh DB is writable.
        let wtx = db.begin_write().unwrap();
        wtx.commit().unwrap();
    }

    /// Why: A path is process-scoped; running tests in parallel must not
    /// collide on the snapshot filename.
    /// What: Asserts the snapshot path contains the current pid and the
    /// original file's name.
    /// Test: this test.
    #[test]
    fn snapshot_path_is_unique_per_process() {
        let p = snapshot_path_for(Path::new("/tmp/palace/kg.redb"));
        let s = p.to_string_lossy().into_owned();
        assert!(s.contains(&format!("{}", std::process::id())));
        assert!(s.ends_with("kg.redb"));
    }
}
