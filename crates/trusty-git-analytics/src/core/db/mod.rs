//! SQLite database access layer.
//!
//! All databases opened by this crate are configured with the following
//! pragmas on every connection open (see `Database::apply_pragmas`):
//!
//! - `journal_mode = WAL` — concurrent reads during write-heavy collection
//! - `busy_timeout = 5000` — wait up to 5 s for a lock before erroring
//! - `synchronous = NORMAL` — durability with reasonable performance
//! - `foreign_keys = ON` — enforce FK constraints
//! - `cache_size = -65536` — 64 MB page cache (negative = KB)
//! - `temp_store = MEMORY` — temporary tables / indexes held in RAM
//! - `mmap_size = 268435456` — 256 MB memory-mapped I/O window
//!
//! WAL mode is **mandatory** per project conventions and is set on every
//! [`Database::open`] call.
//!
//! ## Connection pooling
//!
//! `tga` is a single-process CLI, not a server. A single
//! [`rusqlite::Connection`] guarded by `&mut` borrow is sufficient: the
//! collection / classification / report stages each run sequentially and
//! never share the connection across threads. We deliberately do **not**
//! pull in `r2d2-sqlite` — a pool adds locking and threading overhead with
//! zero benefit at our concurrency level (1). If a future stage needs
//! parallel SQLite access, prefer `rusqlite`'s built-in
//! `Connection::open_with_flags` per worker over a pool.
//!
//! See `docs/trusty-git-analytics/decisions/0001-sqlite-tuning.md` for the rationale behind each
//! pragma value.

use std::path::Path;

use rusqlite::Connection;
use tracing::{debug, info};

use crate::core::config::expand_path;
use crate::core::errors::{Result, TgaError};

pub mod azdo_iterations;
pub mod collection_runs;
pub mod migrations;
pub mod work_items;

pub use azdo_iterations::{list_iterations, upsert_iteration};
pub use collection_runs::{is_week_collected, record_collection_run, repo_count_for_week};
pub use work_items::{
    get_work_item, get_work_items_for_commit, link_commit_work_item, list_work_items,
    upsert_work_item, WorkItemRow,
};

/// Wrapper around a [`rusqlite::Connection`] with project-standard pragmas
/// applied and migrations run.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create a SQLite database at `path`, apply pragmas, and run
    /// any pending migrations.
    ///
    /// Tilde-expansion is applied to `path`.
    ///
    /// # Errors
    ///
    /// - [`TgaError::DbError`] for SQLite-level failures.
    /// - [`TgaError::MigrationError`] if a migration fails.
    pub fn open(path: &Path) -> Result<Database> {
        let resolved = expand_path(path);
        debug!(path = %resolved.display(), "opening database");
        let conn = Connection::open(&resolved)?;
        Self::apply_pragmas(&conn)?;
        let mut db = Database { conn };
        migrations::run(&mut db.conn)?;
        info!(path = %resolved.display(), "database ready");
        Ok(db)
    }

    /// Open an in-memory database. Primarily intended for tests.
    ///
    /// # Errors
    ///
    /// See [`Database::open`].
    pub fn open_in_memory() -> Result<Database> {
        let conn = Connection::open_in_memory()?;
        Self::apply_pragmas(&conn)?;
        let mut db = Database { conn };
        migrations::run(&mut db.conn)?;
        Ok(db)
    }

    /// Apply the canonical pragma set on a fresh connection.
    ///
    /// Pragmas applied (see module-level docs for rationale):
    /// - `journal_mode = WAL`
    /// - `busy_timeout = 5000`
    /// - `synchronous = NORMAL`
    /// - `foreign_keys = ON`
    /// - `cache_size = -65536` (64 MB)
    /// - `temp_store = MEMORY`
    /// - `mmap_size = 268435456` (256 MB)
    ///
    /// Why the `busy_timeout`: in WAL mode a brief exclusive lock is taken
    /// during checkpointing and at the tail of a write transaction. If
    /// `tga classify` opens the DB while a just-finished `tga collect` is
    /// still flushing/checkpointing its WAL, SQLite returns `SQLITE_BUSY`
    /// ("database is locked") *immediately* with no retry — surfacing as a
    /// hard failure to the operator (issue #397, bug 3). Setting
    /// `busy_timeout` makes SQLite block and retry for up to 5 s, which is
    /// far longer than any transient checkpoint lock, so the second command
    /// waits the lock out instead of erroring.
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        // `journal_mode` is a query-style pragma; use query_row to honor it.
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(TgaError::from)?;
        debug!(journal_mode = %mode, "applied WAL pragma");
        // Bundle the remaining pragmas in a single batch — none of them
        // return rows so `execute_batch` is appropriate. `busy_timeout` is
        // first so a transient lock from a concurrent writer is waited out
        // rather than failing the rest of the batch.
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000; \
             PRAGMA synchronous = NORMAL; \
             PRAGMA foreign_keys = ON; \
             PRAGMA cache_size = -65536; \
             PRAGMA temp_store = MEMORY; \
             PRAGMA mmap_size = 268435456;",
        )?;
        debug!(
            "applied SQLite tuning pragmas (busy_timeout=5s, cache=64MB, mmap=256MB, temp=memory)"
        );
        Ok(())
    }

    /// Borrow the underlying connection (read-only).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Borrow the underlying connection mutably.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Return the active journal mode (e.g. `"wal"` or `"memory"`).
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] if the pragma query fails.
    pub fn journal_mode(&self) -> Result<String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(TgaError::from)?;
        Ok(mode)
    }

    /// Return the highest applied migration version.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] if the query fails.
    pub fn schema_version(&self) -> Result<i64> {
        let v: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(TgaError::from)?;
        Ok(v)
    }

    /// Checkpoint the WAL into the main database file.
    ///
    /// Why: SQLite WAL mode defers writes to a separate `-wal` file; if the
    /// process exits (or is killed) before a checkpoint, the main database
    /// file can lag behind the WAL. On clean exit callers should call
    /// `wal_checkpoint(CheckpointMode::Truncate)` to flush and zero the WAL,
    /// guaranteeing durability. During long-running writes, periodic
    /// `wal_checkpoint(CheckpointMode::Passive)` calls limit the data-loss
    /// window on crash. See bug #298.
    ///
    /// What: executes `PRAGMA wal_checkpoint(<mode>)` and logs the result.
    /// Returns `Ok(())` on success; returns an error if the checkpoint pragma
    /// fails (e.g. `SQLITE_CORRUPT`).
    ///
    /// # Checkpoint modes
    ///
    /// - [`CheckpointMode::Passive`] — copies frames from the WAL without
    ///   blocking writers. Safe to call mid-run; used for periodic crash-
    ///   resilience checkpoints.
    /// - [`CheckpointMode::Truncate`] — flushes all WAL frames to the main
    ///   database and truncates the WAL file to zero. Call on clean exit to
    ///   guarantee durability and cap the WAL file size.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] if the pragma query fails.
    ///
    /// # Test
    ///
    /// See `tests::wal_checkpoint_truncate_zeroes_wal_on_file_db`.
    pub fn wal_checkpoint(&self, mode: CheckpointMode) -> Result<()> {
        let mode_str = match mode {
            CheckpointMode::Passive => "PASSIVE",
            CheckpointMode::Truncate => "TRUNCATE",
        };
        // `wal_checkpoint()` returns three columns (busy, log, checkpointed).
        // We query them for logging purposes but do not fail on non-zero
        // "busy" — a passive checkpoint may leave some WAL frames uncopied
        // when another reader/writer holds a lock.
        let (busy, log, checkpointed): (i64, i64, i64) = self
            .conn
            .query_row(&format!("PRAGMA wal_checkpoint({mode_str})"), [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(TgaError::from)?;
        info!(
            mode = mode_str,
            busy, log, checkpointed, "WAL checkpoint complete"
        );
        Ok(())
    }
}

/// WAL checkpoint mode.
///
/// Why: the two modes have different safety vs. throughput trade-offs; having
/// a typed enum prevents mixing up the string literals at call sites.
/// What: `Passive` for mid-run crash-resilience calls; `Truncate` for the
/// clean-exit flush.
/// Test: used by `Database::wal_checkpoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointMode {
    /// Copy frames without blocking; some frames may remain if a reader
    /// holds a lock.
    Passive,
    /// Flush all frames and truncate the WAL file to zero. Requires
    /// exclusive access; retry if blocked.
    Truncate,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: regression guard for issue #298. `wal_checkpoint(Passive)` must
    /// succeed on an in-memory database (WAL is a no-op for `:memory:`, but
    /// the pragma must not return an error). Verifies the PRAGMA syntax is
    /// correct and the return columns are mapped properly.
    /// What: open an in-memory DB, call both checkpoint modes, assert no error.
    /// Test: pure DB exercise, no I/O besides SQLite.
    #[test]
    fn wal_checkpoint_succeeds_on_in_memory_db() {
        let db = Database::open_in_memory().expect("open");
        // In-memory DBs use `memory` journal mode, not WAL. SQLite still
        // accepts the checkpoint pragma but returns (0, 0, 0) — no error.
        db.wal_checkpoint(CheckpointMode::Passive)
            .expect("passive checkpoint must not fail");
        db.wal_checkpoint(CheckpointMode::Truncate)
            .expect("truncate checkpoint must not fail");
    }

    /// Why: regression guard for issue #397 bug 3. Every connection must set a
    /// non-zero `busy_timeout` so that a `classify` opened immediately after a
    /// `collect` waits out the brief WAL checkpoint lock instead of failing
    /// with "database is locked". Without the pragma, `busy_timeout` is 0 and
    /// any contended lock errors instantly.
    /// What: open a DB and read back `PRAGMA busy_timeout`; assert it is 5000ms.
    /// Test: pure pragma read; works on in-memory and file DBs identically.
    #[test]
    fn busy_timeout_is_set_to_5000ms() {
        let db = Database::open_in_memory().expect("open");
        let timeout: i64 = db
            .connection()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("query busy_timeout");
        assert_eq!(
            timeout, 5000,
            "busy_timeout must be 5000ms so contended opens wait rather than erroring"
        );
    }

    /// Why: a second connection opened against the same file-backed WAL
    /// database (the `collect` → `classify` sequence) must succeed even while
    /// the first connection holds an open write transaction for a moment;
    /// `busy_timeout` makes the second connection wait rather than fail with
    /// "database is locked" (issue #397 bug 3). This asserts the second open
    /// applies its pragmas (including `busy_timeout`) without erroring while
    /// the first connection is live.
    /// What: open a file DB, keep it open, open a *second* `Database` on the
    /// same path, and assert both report a 5000ms `busy_timeout`.
    /// Test: uses a real temp-file DB so WAL locking semantics apply.
    #[test]
    fn second_open_on_same_file_succeeds_with_busy_timeout() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let db_path = tmp.path().to_path_buf();
        tmp.keep().expect("keep tempfile");

        let db1 = Database::open(&db_path).expect("first open");
        assert_eq!(db1.journal_mode().expect("journal_mode"), "wal");

        // A second connection (mimicking `tga classify` after `tga collect`)
        // must open cleanly and also carry the busy_timeout pragma.
        let db2 = Database::open(&db_path).expect("second open must not fail");
        let timeout: i64 = db2
            .connection()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("query busy_timeout");
        assert_eq!(timeout, 5000);

        drop(db1);
        drop(db2);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Why: the checkpoint must succeed and the WAL file must be small (or
    /// absent) after a TRUNCATE on a real file-backed database with WAL mode.
    /// What: create a temp file DB, write 100 rows, call TRUNCATE, assert the
    /// WAL file is 0 bytes or absent.
    /// Test: uses `tempfile::NamedTempFile` for a real filesystem DB.
    #[test]
    fn wal_checkpoint_truncate_zeroes_wal_on_file_db() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let db_path = tmp.path().to_path_buf();
        // Keep the file alive through the test by converting to a persist path.
        tmp.keep().expect("keep tempfile");

        {
            let db = Database::open(&db_path).expect("open");
            // Verify WAL mode is active.
            assert_eq!(db.journal_mode().expect("journal_mode"), "wal");

            // Write rows to ensure the WAL has content.
            for i in 0..100 {
                db.connection()
                    .execute(
                        "INSERT INTO commits \
                         (sha, author_name, author_email, timestamp, message, repository) \
                         VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', 'msg', 'repo')",
                        rusqlite::params![format!("sha-{i}")],
                    )
                    .expect("insert");
            }

            // TRUNCATE checkpoint: must flush and zero the WAL.
            db.wal_checkpoint(CheckpointMode::Truncate)
                .expect("truncate checkpoint");
        }

        // After the connection drops, check the WAL file.
        let wal_path = db_path.with_extension("db-wal");
        if wal_path.exists() {
            let wal_size = std::fs::metadata(&wal_path).expect("wal metadata").len();
            assert_eq!(
                wal_size, 0,
                "WAL file must be zero bytes after TRUNCATE checkpoint, got {wal_size} bytes"
            );
        }
        // If the WAL file doesn't exist, the checkpoint succeeded completely.

        // Verify all rows are in the main DB by opening it fresh.
        let db2 = Database::open(&db_path).expect("reopen");
        let count: i64 = db2
            .connection()
            .query_row("SELECT COUNT(*) FROM commits", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 100, "all 100 rows must be durable after checkpoint");

        // Cleanup.
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&wal_path);
        let shm_path = db_path.with_extension("db-shm");
        let _ = std::fs::remove_file(&shm_path);
    }
}
