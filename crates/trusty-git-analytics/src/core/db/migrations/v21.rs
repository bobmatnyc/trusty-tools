//! Migration v21 (`agentic_mode`) — column guard for `commits.agentic_mode`
//! and the `fact_weekly_engineer` table.
//!
//! Extracted to a dedicated module (like `v17.rs`) because the `agentic_mode`
//! ADD COLUMN may already exist in a pre-release build that applied the column
//! directly. SQLite has no `ADD COLUMN IF NOT EXISTS`, so we query
//! `PRAGMA table_info` before issuing the ALTER.

use rusqlite::Connection;
use tracing::debug;

use crate::core::errors::{Result, TgaError};

use super::column_names;

/// Apply migration 21 (`agentic_mode`) with a guard for `commits.agentic_mode`.
///
/// Why: the `agentic_mode` column (issue #1113) may already be present in a
/// pre-release build that patched migration v17 directly. SQLite has no
/// `ALTER TABLE … ADD COLUMN IF NOT EXISTS`, so we check before altering.
///
/// What: three parts:
/// 1. `commits.agentic_mode` ADD COLUMN (guarded) + index.
/// 2. `fact_weekly_engineer` CREATE TABLE IF NOT EXISTS + indexes.
///
/// All steps are non-destructive — no existing rows or columns are changed.
///
/// Test: `tests::migration_v21_*` below and in `migrations::tests`.
pub(super) fn apply(conn: &Connection) -> Result<()> {
    // Part 1: add `agentic_mode` to `commits` (guarded).
    let commits_cols = column_names(conn, "commits")?;
    if !commits_cols.iter().any(|c| c == "agentic_mode") {
        conn.execute_batch(
            "ALTER TABLE commits ADD COLUMN agentic_mode TEXT NOT NULL DEFAULT 'none';",
        )
        .map_err(|e| {
            TgaError::MigrationError(format!(
                "migration 21 (agentic_mode) commits ALTER failed: {e}"
            ))
        })?;
    } else {
        debug!(
            "migration v21: agentic_mode already present in commits \
             (pre-release build detected), skipping ADD COLUMN"
        );
    }

    // Index on agentic_mode (always CREATE IF NOT EXISTS — idempotent).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_commits_agentic_mode ON commits(agentic_mode);",
    )
    .map_err(|e| {
        TgaError::MigrationError(format!(
            "migration 21 (agentic_mode) commits index failed: {e}"
        ))
    })?;

    // Part 2: `fact_weekly_engineer` (CREATE TABLE IF NOT EXISTS — idempotent).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS fact_weekly_engineer ( \
            author_email        TEXT    NOT NULL, \
            iso_year            INTEGER NOT NULL, \
            iso_week            INTEGER NOT NULL, \
            repository          TEXT    NOT NULL, \
            net_commits         INTEGER NOT NULL DEFAULT 0, \
            agentic_count       INTEGER NOT NULL DEFAULT 0, \
            ide_assisted_count  INTEGER NOT NULL DEFAULT 0, \
            agentic_pct         REAL    NOT NULL DEFAULT 0.0, \
            formula_version     TEXT    NOT NULL DEFAULT 'v1', \
            computed_at         INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY (author_email, iso_year, iso_week, repository) \
        ); \
        CREATE INDEX IF NOT EXISTS idx_fwe_week   ON fact_weekly_engineer (iso_year, iso_week); \
        CREATE INDEX IF NOT EXISTS idx_fwe_author ON fact_weekly_engineer (author_email); \
        CREATE INDEX IF NOT EXISTS idx_fwe_repo   ON fact_weekly_engineer (repository);",
    )
    .map_err(|e| {
        TgaError::MigrationError(format!(
            "migration 21 (agentic_mode) fact_weekly_engineer CREATE TABLE failed: {e}"
        ))
    })?;

    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use crate::core::db::Database;
    use rusqlite::params;

    /// Why: regression guard for issue #1113. Migration v21 must add
    /// `commits.agentic_mode` with default 'none' and create the
    /// `fact_weekly_engineer` table with the correct schema.
    /// What: opens an in-memory DB (runs all migrations), inserts rows that
    /// exercise both new structures, reads them back, and verifies UPSERT
    /// semantics on `fact_weekly_engineer`.
    /// Test: this test itself.
    #[test]
    pub(crate) fn migration_v21_adds_agentic_mode_and_fwe() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // --- commits.agentic_mode ---
        // Insert a commit with an explicit agentic_mode value.
        conn.execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, \
              is_ai_assisted, ai_tool, agentic_mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "sha_v21_agentic",
                "Alice",
                "alice@example.com",
                "2026-01-01T00:00:00Z",
                "feat: full-agentic\n\nCo-Authored-By: Claude Opus <noreply@anthropic.com>",
                "testrepo",
                1_i64,
                "claude",
                "full_agentic",
            ],
        )
        .expect("insert full_agentic commit");

        let mode: String = conn
            .query_row(
                "SELECT agentic_mode FROM commits WHERE sha = 'sha_v21_agentic'",
                [],
                |r| r.get(0),
            )
            .expect("read agentic_mode");
        assert_eq!(mode, "full_agentic");

        // Insert a commit with the default (should be 'none').
        conn.execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "sha_v21_none",
                "Bob",
                "bob@example.com",
                "2026-01-02T00:00:00Z",
                "chore: plain human commit",
                "testrepo",
            ],
        )
        .expect("insert plain commit");

        let default_mode: String = conn
            .query_row(
                "SELECT agentic_mode FROM commits WHERE sha = 'sha_v21_none'",
                [],
                |r| r.get(0),
            )
            .expect("read default agentic_mode");
        assert_eq!(
            default_mode, "none",
            "agentic_mode must default to 'none' for pre-migration rows"
        );

        // --- fact_weekly_engineer ---
        conn.execute(
            "INSERT OR REPLACE INTO fact_weekly_engineer \
             (author_email, iso_year, iso_week, repository, \
              net_commits, agentic_count, ide_assisted_count, agentic_pct, \
              formula_version, computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                "alice@example.com",
                2026_i64,
                3_i64,
                "testrepo",
                10_i64,   // net_commits
                7_i64,    // agentic_count
                1_i64,    // ide_assisted_count
                70.0_f64, // agentic_pct
                "v1",
                1_000_000_i64,
            ],
        )
        .expect("insert fwe row");

        let (net, agentic, ide, pct): (i64, i64, i64, f64) = conn
            .query_row(
                "SELECT net_commits, agentic_count, ide_assisted_count, agentic_pct \
                 FROM fact_weekly_engineer \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 AND iso_week = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("read fwe row");

        assert_eq!(net, 10);
        assert_eq!(agentic, 7);
        assert_eq!(ide, 1);
        assert!(
            (pct - 70.0).abs() < 1e-9,
            "agentic_pct must be 70.0, got {pct}"
        );

        // Verify UPSERT: second insert with updated agentic_pct must overwrite.
        conn.execute(
            "INSERT OR REPLACE INTO fact_weekly_engineer \
             (author_email, iso_year, iso_week, repository, \
              net_commits, agentic_count, ide_assisted_count, agentic_pct, \
              formula_version, computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                "alice@example.com",
                2026_i64,
                3_i64,
                "testrepo",
                10_i64,
                8_i64,
                1_i64,
                80.0_f64, // updated
                "v1",
                2_000_000_i64,
            ],
        )
        .expect("upsert fwe row");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fact_weekly_engineer \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 AND iso_week = 3",
                [],
                |r| r.get(0),
            )
            .expect("count fwe rows");
        assert_eq!(count, 1, "UPSERT must not duplicate the grain row");

        let new_pct: f64 = conn
            .query_row(
                "SELECT agentic_pct FROM fact_weekly_engineer \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 AND iso_week = 3",
                [],
                |r| r.get(0),
            )
            .expect("read new pct");
        assert!(
            (new_pct - 80.0).abs() < 1e-9,
            "UPSERT must overwrite agentic_pct with 80.0, got {new_pct}"
        );
    }

    /// Why: the pre-release guard must fire when `agentic_mode` already exists
    /// in `commits` before migration v21 runs.
    /// What: applies migrations 1-20 manually, then alters commits to add
    /// `agentic_mode` (simulating a pre-release build), then runs `run()`.
    /// Asserts no error and the column is still writable.
    /// Test: this test itself.
    #[test]
    pub(crate) fn migration_v21_is_idempotent_when_agentic_mode_already_exists() {
        use rusqlite::Connection;

        let mut conn = Connection::open_in_memory().expect("open raw connection");
        super::super::ensure_migrations_table(&conn).expect("ensure table");

        // Apply migrations 1 through 20 only.
        for m in super::super::MIGRATIONS {
            if m.version > 20 {
                break;
            }
            let tx = conn.transaction().expect("begin tx");
            if m.version == 17 {
                super::super::v17::apply(&tx).expect("v17");
            } else {
                tx.execute_batch(m.sql)
                    .unwrap_or_else(|e| panic!("migration {} failed: {e}", m.version));
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, name, applied_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![m.version, m.name, "2026-01-01T00:00:00Z"],
            )
            .expect("record migration");
            tx.commit().expect("commit");
        }

        // Simulate pre-release: add agentic_mode directly.
        conn.execute_batch(
            "ALTER TABLE commits ADD COLUMN agentic_mode TEXT NOT NULL DEFAULT 'none';",
        )
        .expect("pre-release ALTER TABLE (simulating old dev build)");

        // Now apply migration 21 — must NOT fail with duplicate column.
        super::super::run(&mut conn)
            .expect("migration v21 must succeed even when agentic_mode already exists");

        // Verify the column is still writable after the guarded migration.
        conn.execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, agentic_mode) \
             VALUES ('sha_idem21', 'Test', 't@e.com', '2026-01-01T00:00:00Z', 'msg', 'repo', \
                     'ide_assisted')",
            [],
        )
        .expect("insert with agentic_mode post-migration");

        let mode: String = conn
            .query_row(
                "SELECT agentic_mode FROM commits WHERE sha = 'sha_idem21'",
                [],
                |r| r.get(0),
            )
            .expect("read agentic_mode");
        assert_eq!(mode, "ide_assisted");
    }
}
