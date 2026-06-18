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

/// Whether the caller intends to write to the palace redb file.
///
/// Why (issue #1152, Tier 3): `try_open_or_snapshot` previously always
/// fell back to a read-only snapshot when the file was already locked by
/// another process, regardless of whether the caller was a read-only stdio
/// client or the HTTP daemon itself (a writer). A daemon that hits
/// `DatabaseAlreadyOpen` and silently opens a snapshot would service all
/// its writes against a throw-away copy — a correctness disaster. Passing
/// `OpenIntent` lets the function fail loud (Err) for writers while
/// preserving the snapshot fallback for genuine read-only clients.
/// What: Two-variant enum. `Writer` → error on lock contention;
/// `ReadOnlyClient` → snapshot fallback (legacy behaviour, kept for
/// future read-only client paths).
/// Test: `writer_intent_fails_on_locked_file`,
/// `snapshot_fallback_when_locked` (ReadOnlyClient path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenIntent {
    /// The caller needs read-write access. When the file is already locked
    /// by another process, return `Err` rather than opening a snapshot.
    Writer,
    /// The caller only needs to read. When the file is already locked, open
    /// a process-local snapshot copy (existing behaviour, issue #59).
    ReadOnlyClient,
}

/// Number of `Database::create` attempts the `Writer` path makes before it
/// gives up and fails loud on a persistent `DatabaseAlreadyOpen`.
///
/// Why (issue #1487): a graceful launchd handoff
/// (`bootout` old → `bootstrap` new) briefly overlaps two daemons — the old
/// one may still hold the redb `flock(LOCK_EX)` for a few hundred
/// milliseconds while the new one starts. A naive fail-loud on the very
/// first `DatabaseAlreadyOpen` would make the fresh daemon exit non-zero,
/// triggering launchd's `KeepAlive { SuccessfulExit: false }` respawn — i.e.
/// restart flapping. Retrying for a short, bounded window absorbs the
/// handoff without masking a *persistent* conflict (a second live daemon).
/// What: 5 attempts. Combined with [`WRITER_RETRY_SLEEP_MS`] the total wait
/// is bounded at ~1.55 s — long enough to outlast a normal flock handoff,
/// short enough that a genuine conflict still fails loud quickly.
/// Test: `writer_intent_fails_on_locked_file` (persistent conflict still
/// errors) and `writer_intent_retries_then_succeeds_when_lock_released`
/// (transient conflict succeeds within the window).
pub(crate) const WRITER_RETRY_ATTEMPTS: u8 = 5;

/// Per-attempt backoff (milliseconds) for the `Writer` retry loop.
///
/// Why (issue #1487): exponential backoff spreads the 5 attempts across a
/// ~1.55 s window (0 + 50 + 100 + 400 + 1000) so the common case (handoff
/// finishes in <500 ms) succeeds on attempt 2 or 3 without waiting the full
/// budget, while a still-held lock is re-probed a few more times before we
/// declare a hard conflict. The first attempt has no preceding sleep, so the
/// uncontended open returns immediately.
/// What: One sleep value per attempt *after* the first
/// (`WRITER_RETRY_ATTEMPTS - 1 == 4` entries). The sleep at index `i` is
/// applied before attempt `i + 1`.
/// Test: `writer_retry_sleep_table_matches_attempt_count` asserts the table
/// length stays in lock-step with [`WRITER_RETRY_ATTEMPTS`].
pub(crate) const WRITER_RETRY_SLEEP_MS: [u64; 4] = [50, 100, 400, 1000];

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
/// snapshot copy when the file is already locked by another process AND
/// the caller is a read-only client.
///
/// Why: The HTTP daemon holds an exclusive flock on every palace's
/// `kg.redb` and `index.usearch.redb`. Without this helper a second
/// process (e.g. the stdio MCP server invoked by Claude Code) cannot open
/// the same palace at all — every recall fails with "open palace …".
/// With this helper, a read-only client detects lock contention and
/// transparently switches to a snapshot copy so reads can proceed.
/// Issue #1152 (Tier 3) / #1487: a daemon/writer that gets
/// `DatabaseAlreadyOpen` must NOT silently open a snapshot — it would service
/// all its writes against a throw-away copy while the real daemon continues
/// writing to the live file. Writer contexts receive a loud `Err` instead,
/// but only after a short bounded retry window (issue #1487) that absorbs the
/// brief flock overlap of a graceful launchd handoff without flapping.
/// What: Attempts `Database::create(path)`. On
/// `DatabaseError::DatabaseAlreadyOpen` the behaviour depends on `intent`.
/// ReadOnlyClient: copies `path` to a per-process snapshot and opens that
/// (existing read-only fallback for issue #59). Writer: retries up to
/// [`WRITER_RETRY_ATTEMPTS`] times with exponential backoff
/// ([`WRITER_RETRY_SLEEP_MS`]); if the lock is still held after the window,
/// returns `Err` with a clear, actionable message and an ERROR log — never a
/// snapshot. Returns the open database, a `SnapshotGuard` that removes the
/// snapshot file when dropped, and the `OpenMode` so the caller can reject
/// writes when running on a snapshot.
/// Test: `snapshot_fallback_when_locked` (ReadOnlyClient path),
/// `writer_intent_fails_on_locked_file` (Writer persistent-conflict path),
/// and `writer_intent_retries_then_succeeds_when_lock_released` (Writer
/// transient-conflict path, issue #1487).
pub fn try_open_or_snapshot(
    path: &Path,
    intent: OpenIntent,
) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
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
        Err(DatabaseError::DatabaseAlreadyOpen) => match intent {
            // Issue #1152, Tier 3 + #1487: a writer that hits the lock must
            // fail loud (never a snapshot), but only after a short bounded
            // retry to absorb a graceful launchd handoff's flock overlap.
            OpenIntent::Writer => open_writer_with_handoff_retry(path),
            // Original fallback for read-only clients (issue #59).
            OpenIntent::ReadOnlyClient => open_read_only_snapshot(path),
        },
        Err(e) => Err(anyhow::anyhow!("open redb at {}: {e}", path.display())),
    }
}

/// Open `path` exclusively for a writer, retrying through a brief
/// `DatabaseAlreadyOpen` window before failing loud.
///
/// Why (issue #1487): a graceful daemon restart (`launchctl bootout` the old
/// instance, then `bootstrap` the new one) briefly overlaps two processes;
/// the old daemon can still hold the redb `flock` for a few hundred
/// milliseconds. Failing on the first `DatabaseAlreadyOpen` would make the
/// new daemon exit non-zero and trigger launchd's respawn-on-failure, i.e.
/// flapping. Retrying for a bounded window
/// ([`WRITER_RETRY_ATTEMPTS`] × [`WRITER_RETRY_SLEEP_MS`]) lets the handoff
/// complete. A *persistent* lock (a second live daemon) still fails loud at
/// the end of the window — it MUST NOT degrade to a snapshot.
/// What: Re-attempts `Database::create(path)` with exponential backoff. On
/// success returns `OpenMode::ReadWrite`; if every attempt sees
/// `DatabaseAlreadyOpen`, logs an ERROR and returns a clear `Err` naming the
/// conflict and the path. Any *other* error (incompatible format, I/O) is
/// surfaced immediately without retry.
/// Test: `writer_intent_fails_on_locked_file`,
/// `writer_intent_retries_then_succeeds_when_lock_released`.
fn open_writer_with_handoff_retry(path: &Path) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
    // Attempt 0 already failed with DatabaseAlreadyOpen at the call site, so
    // re-probe through the remaining window. We re-run the *first* create here
    // too (after the initial backoff) to keep the loop self-contained and the
    // attempt accounting honest.
    for attempt in 0..WRITER_RETRY_ATTEMPTS {
        if attempt > 0 {
            // Sleep before every attempt after the first; index is attempt-1
            // because the first retry uses the smallest backoff.
            let sleep_ms = WRITER_RETRY_SLEEP_MS[(attempt - 1) as usize];
            backoff_sleep_ms(sleep_ms);
        }
        match Database::create(path) {
            Ok(db) => {
                if attempt > 0 {
                    tracing::info!(
                        path = %path.display(),
                        attempt = attempt + 1,
                        "acquired redb write lock after a transient conflict \
                         (graceful daemon handoff absorbed; issue #1487)"
                    );
                }
                return Ok((Arc::new(db), SnapshotGuard::noop(), OpenMode::ReadWrite));
            }
            Err(DatabaseError::DatabaseAlreadyOpen) => {
                // Still held — keep probing until the window is exhausted.
                continue;
            }
            // A non-lock error (I/O, incompatible format that slipped past the
            // caller's branch) is not a handoff race; fail immediately.
            Err(e) => return Err(anyhow::anyhow!("open redb at {}: {e}", path.display())),
        }
    }

    tracing::error!(
        path = %path.display(),
        attempts = WRITER_RETRY_ATTEMPTS,
        "redb file is still locked by another process after the write-lock handoff \
         retry window but this process requires write access (OpenIntent::Writer). \
         Refusing to open a read-only snapshot — another live trusty-memory instance \
         holds the write lock. Stop the other daemon first or ensure only one writer \
         is active at a time."
    );
    Err(anyhow::anyhow!(
        "palace redb file {} is still locked by another live trusty-memory instance \
         after {} write-lock acquisition attempts; this daemon requires write access \
         and refuses to degrade to read-only/snapshot mode. Stop the other daemon or \
         run `trusty-memory stop` first.",
        path.display(),
        WRITER_RETRY_ATTEMPTS,
    ))
}

/// Open a process-local read-only snapshot copy of a locked redb file.
///
/// Why (issue #59): a read-only stdio MCP client must still serve `recall`,
/// `kg_query`, and `palace_info` while the HTTP daemon holds the exclusive
/// `flock`. Copying the file to a per-process snapshot lets redb's lock check
/// succeed against the copy.
/// What: Copies `path` to a per-call-unique snapshot path, opens it as a
/// fresh redb database, and returns it in `OpenMode::Snapshot` with a
/// `SnapshotGuard` that deletes the copy on drop.
/// Test: `snapshot_fallback_when_locked`, `snapshot_guard_removes_file_on_drop`.
fn open_read_only_snapshot(path: &Path) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
    let snap = snapshot_path_for(path);
    // Snapshot paths are per-call unique (pid + monotonic counter), so no
    // stale-file cleanup is needed here.
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

/// Sleep for `ms` milliseconds in a way that is safe from both sync and async
/// Tokio contexts.
///
/// Why: `KgStoreRedb::open` and `open_or_get_cached_db` are sync fns but are
/// called from async HTTP handlers (via `PalaceHandle::open`). A bare
/// `std::thread::sleep` blocks the Tokio worker thread, starving other tasks.
/// `tokio::task::block_in_place` signals the multi-thread scheduler to
/// move pending tasks to other workers while this thread sleeps. On a
/// `current_thread` scheduler (used in some unit tests) `block_in_place` panics,
/// so we fall back to plain `std::thread::sleep` in that case. No-op when
/// called outside any Tokio runtime.
/// What: Detects the active runtime flavor; uses `block_in_place` on
/// `MultiThread`, falls back to `std::thread::sleep` elsewhere.
/// Test: Indirectly exercised by every retry-backoff test in `kg_redb` and
/// `vector` (both sync and async test configurations).
pub(crate) fn backoff_sleep_ms(ms: u64) {
    let dur = std::time::Duration::from_millis(ms);
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| std::thread::sleep(dur));
        }
        _ => std::thread::sleep(dur),
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
    /// that is already locked falls back to a snapshot and succeeds for a
    /// read-only client.
    /// What: Opens `db.redb` in this process (acquiring the lock), then
    /// calls `try_open_or_snapshot` with `ReadOnlyClient`. The second
    /// call must succeed in `Snapshot` mode.
    /// Test: this test.
    #[test]
    fn snapshot_fallback_when_locked() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");

        // First open holds the flock.
        let live = Database::create(&path).expect("first open");

        // Second open with ReadOnlyClient intent must succeed via snapshot.
        let (snap_db, guard, mode) = try_open_or_snapshot(&path, OpenIntent::ReadOnlyClient)
            .expect("snapshot fallback should succeed for read-only client");
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

    /// Why (issue #1152, Tier 3 + #1487): a daemon / writer that hits a
    /// *persistent* `DatabaseAlreadyOpen` (a second live daemon, not a
    /// transient handoff) must return `Err` rather than silently opening a
    /// snapshot — writing to a snapshot copy would diverge from the live file
    /// and corrupt the user's palace data. The error must arrive only after
    /// the bounded retry window is exhausted (issue #1487), never degrading
    /// to snapshot mode.
    /// What: Opens `db.redb` in this process (acquiring the lock) and HOLDS it
    /// for the whole test, then calls `try_open_or_snapshot` with `Writer`.
    /// The call must return `Err` (after retrying the full window) with a
    /// message naming the lock conflict.
    /// Test: this test.
    #[test]
    fn writer_intent_fails_on_locked_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");

        // First open holds the flock for the duration of the call below.
        let _live = Database::create(&path).expect("first open");

        // Writer intent must not fall back to a snapshot — must be Err after
        // the retry window since the lock is never released.
        let result = try_open_or_snapshot(&path, OpenIntent::Writer);
        assert!(
            result.is_err(),
            "Writer intent must return Err on persistent lock contention, not silently snapshot"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("still locked") || msg.contains("write access"),
            "error message must explain the lock conflict; got: {msg}"
        );
    }

    /// Why (issue #1487): the `Writer` retry window must absorb a transient
    /// lock — when the holding handle is released *within* the backoff window,
    /// the writer open must eventually SUCCEED in `ReadWrite` mode (never a
    /// snapshot). This guards the restart-handoff behaviour: a graceful
    /// `bootout`→`bootstrap` overlap resolves into a clean writer open.
    /// What: Holds the flock on `db.redb`, spawns a thread that drops the lock
    /// after ~120 ms (inside the ~1.55 s retry window), then calls
    /// `try_open_or_snapshot` with `Writer`. The open must return `ReadWrite`.
    /// Test: this test.
    #[test]
    fn writer_intent_retries_then_succeeds_when_lock_released() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");

        // Hold the flock, then release it from a background thread after a
        // short delay that lands well inside the writer retry window.
        let live = Database::create(&path).expect("first open");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            drop(live); // releases the OS flock
        });

        // Writer intent should retry past the initial DatabaseAlreadyOpen and
        // succeed once the background thread drops the lock.
        let (_db, _guard, mode) = try_open_or_snapshot(&path, OpenIntent::Writer)
            .expect("Writer open should succeed after the lock is released within the window");
        assert_eq!(
            mode,
            OpenMode::ReadWrite,
            "Writer must acquire the live lock (ReadWrite), never degrade to Snapshot"
        );
        assert!(!mode.is_read_only());

        releaser.join().expect("releaser thread");
    }

    /// Why (issue #1487): the per-attempt backoff table must stay in lock-step
    /// with the attempt count — one sleep precedes every attempt after the
    /// first. A drift between the two would either skip a backoff or index
    /// out of bounds.
    /// What: Asserts `WRITER_RETRY_SLEEP_MS.len() == WRITER_RETRY_ATTEMPTS - 1`.
    /// Test: this test.
    #[test]
    fn writer_retry_sleep_table_matches_attempt_count() {
        assert_eq!(
            WRITER_RETRY_SLEEP_MS.len(),
            (WRITER_RETRY_ATTEMPTS - 1) as usize,
            "one backoff value must precede every attempt after the first"
        );
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
        let (_db, _guard, mode) =
            try_open_or_snapshot(&path, OpenIntent::Writer).expect("direct open");
        assert_eq!(mode, OpenMode::ReadWrite);
        assert!(!mode.is_read_only());
    }

    /// Why: Snapshot files must be removed on guard drop so $TMPDIR does
    /// not accumulate stale copies after a stdio session ends.
    /// What: Force-creates a snapshot via lock contention (ReadOnlyClient),
    /// captures the snapshot path from the guard via Debug, drops the guard,
    /// and asserts the file is gone.
    /// Test: this test.
    #[test]
    fn snapshot_guard_removes_file_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.redb");
        let _live = Database::create(&path).unwrap();

        let (_snap_db, guard, _mode) =
            try_open_or_snapshot(&path, OpenIntent::ReadOnlyClient).expect("fallback");
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

        let (db, _guard, mode) = try_open_or_snapshot(&path, OpenIntent::Writer)
            .expect("incompatible file must recover, not error");
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
