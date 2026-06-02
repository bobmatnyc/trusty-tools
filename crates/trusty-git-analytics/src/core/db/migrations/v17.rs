//! Migration 17 (`pushdown_445`) — column guard for `effort_tshirt`.
//!
//! Extracted from `migrations.rs` to keep that file under the 500-line cap.
//! All logic is identical; only the file location changed.

use rusqlite::Connection;
use tracing::debug;

use crate::core::errors::{Result, TgaError};

use super::column_names;

/// Apply migration 17 (`pushdown_445`) with a guard for the `effort_tshirt`
/// ADD COLUMN that some pre-release v16 builds already included inline.
///
/// Background: the `effort_tshirt` column was occasionally added directly
/// inside the v16 `fact_commit_effort` CREATE TABLE statement during
/// development, before migration 17 was written. SQLite has no
/// `ALTER TABLE … ADD COLUMN IF NOT EXISTS`, so we query `PRAGMA table_info`
/// before the ALTER and skip it when the column already exists.
///
/// The remainder of the migration SQL (classifications, commits, indexes) is
/// executed as normal; none of those columns were subject to the same
/// pre-release contamination.
pub(super) fn apply(conn: &Connection) -> Result<()> {
    // Part 1 (classification top-level): unconditional — classifications was
    // never modified by any pre-release v16 build.
    conn.execute_batch(
        "ALTER TABLE classifications ADD COLUMN top_level_category TEXT;\n\
         CREATE INDEX IF NOT EXISTS idx_classifications_top_level ON classifications(top_level_category);",
    )
    .map_err(|e| {
        TgaError::MigrationError(format!(
            "migration 17 (pushdown_445) classifications step failed: {e}"
        ))
    })?;

    // Part 2 (effort_tshirt): guarded — check if the column is already present
    // before issuing ALTER TABLE (SQLite has no ADD COLUMN IF NOT EXISTS).
    let fce_cols = column_names(conn, "fact_commit_effort")?;
    if !fce_cols.iter().any(|c| c == "effort_tshirt") {
        conn.execute_batch("ALTER TABLE fact_commit_effort ADD COLUMN effort_tshirt INTEGER;")
            .map_err(|e| {
                TgaError::MigrationError(format!(
                    "migration 17 (pushdown_445) effort_tshirt ALTER failed: {e}"
                ))
            })?;
    } else {
        debug!(
            "migration v17: effort_tshirt already present in fact_commit_effort \
             (pre-release v16 build detected), skipping ADD COLUMN"
        );
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_fact_commit_effort_tshirt ON fact_commit_effort(effort_tshirt);",
    )
    .map_err(|e| {
        TgaError::MigrationError(format!(
            "migration 17 (pushdown_445) effort_tshirt index failed: {e}"
        ))
    })?;

    // Part 3 (commits AI attribution): unconditional — commits columns were
    // not part of any pre-release v16 modification.
    conn.execute_batch(
        "ALTER TABLE commits ADD COLUMN is_ai_assisted INTEGER NOT NULL DEFAULT 0;\n\
         ALTER TABLE commits ADD COLUMN ai_tool TEXT;\n\
         CREATE INDEX IF NOT EXISTS idx_commits_is_ai_assisted ON commits(is_ai_assisted);",
    )
    .map_err(|e| {
        TgaError::MigrationError(format!(
            "migration 17 (pushdown_445) commits step failed: {e}"
        ))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::db::Database;
    use rusqlite::params;

    /// Why: regression guard for the duplicate-column bug (issue #445 / tga
    /// 2.5.0). Some pre-release builds modified migration v16 in-place to
    /// include `effort_tshirt` directly in the CREATE TABLE, so production
    /// databases at schema_version=16 already have the column. Migration v17
    /// then does ADD COLUMN IF NOT EXISTS, which must succeed (not raise
    /// "duplicate column name: effort_tshirt") on such databases.
    /// What: manually apply migrations 1-16, then ALTER the table to add
    /// effort_tshirt (simulating a pre-release v16), then run the remaining
    /// migrations (17+). Assert no error and the final schema is correct.
    /// Test: this test itself.
    #[test]
    fn migration_v17_is_idempotent_when_effort_tshirt_already_exists() {
        use rusqlite::Connection;

        // Open a raw in-memory connection so we can control migration application
        // manually without using Database::open_in_memory (which runs all migrations).
        let mut conn = Connection::open_in_memory().expect("open raw connection");

        // Apply only migrations 1–16 by calling the migration runner after
        // temporarily truncating the MIGRATIONS slice is not straightforward, so
        // we use the migration runner's SQL layer directly: first apply all
        // migrations normally (which will include 17+), but we need the
        // pre-release state.  Instead: create a fresh DB, run up to v16 manually,
        // insert effort_tshirt via ALTER TABLE (pre-release simulation), mark
        // schema_migrations at 16, then run migrations::run to apply 17+.
        super::super::ensure_migrations_table(&conn).expect("ensure table");

        // Apply migrations 1 through 16 only.
        for m in super::super::MIGRATIONS {
            if m.version > 16 {
                break;
            }
            let tx = conn.transaction().expect("begin tx");
            tx.execute_batch(m.sql)
                .unwrap_or_else(|e| panic!("migration {} failed: {e}", m.version));
            tx.execute(
                "INSERT INTO schema_migrations(version, name, applied_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![m.version, m.name, "2026-01-01T00:00:00Z"],
            )
            .expect("record migration");
            tx.commit().expect("commit");
        }

        // Simulate a pre-release v16 build that added effort_tshirt directly
        // to fact_commit_effort before migration 17 existed.
        conn.execute_batch("ALTER TABLE fact_commit_effort ADD COLUMN effort_tshirt SMALLINT;")
            .expect("pre-release ALTER TABLE (simulating old dev build)");

        // Now apply migration 17 and beyond using the official runner.
        // This must NOT fail with "duplicate column name: effort_tshirt".
        super::super::run(&mut conn).expect(
            "migration v17 must succeed even when effort_tshirt already exists \
             (ADD COLUMN IF NOT EXISTS guard must fire)",
        );

        // Verify the column is present and writable after migration.
        conn.execute(
            "INSERT INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, \
              formula_version, computed_at, effort_tshirt) \
             VALUES ('sha_idem', 'repo', 'S', 5.0, 10, 1, 0, 1.0, 'v1', 1000000, 2)",
            [],
        )
        .expect("insert with effort_tshirt must succeed post-migration");

        let tshirt: Option<i64> = conn
            .query_row(
                "SELECT effort_tshirt FROM fact_commit_effort WHERE sha = 'sha_idem'",
                [],
                |r| r.get(0),
            )
            .expect("read effort_tshirt");
        assert_eq!(
            tshirt,
            Some(2),
            "effort_tshirt must be readable after idempotent migration"
        );

        // Confirm the schema version advanced past 16.
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("read version");
        assert!(
            version >= 17,
            "schema_migrations must record at least v17 after run(), got {version}"
        );
    }

    /// Why: regression guard for issue #445. Migration v17 adds three additive
    /// columns (`classifications.top_level_category`,
    /// `fact_commit_effort.effort_tshirt`, `commits.is_ai_assisted`,
    /// `commits.ai_tool`) plus three indexes. This test verifies the migration
    /// applies without error and the new columns are writable/readable.
    /// What: opens an in-memory DB (which runs all migrations), inserts rows
    /// that exercise the new columns, and reads them back.
    /// Test: this test itself.
    #[test]
    fn migration_v17_adds_pushdown_columns() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Verify commits accepts is_ai_assisted and ai_tool.
        conn.execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, \
              is_ai_assisted, ai_tool) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "sha_v17_test",
                "Alice",
                "alice@example.com",
                "2026-01-01T00:00:00Z",
                "feat: AI-assisted commit\n\nCo-Authored-By: Claude Opus <noreply@anthropic.com>",
                "testrepo",
                1_i64,
                "claude",
            ],
        )
        .expect("insert AI-assisted commit");

        let (ai, tool): (i64, Option<String>) = conn
            .query_row(
                "SELECT is_ai_assisted, ai_tool FROM commits WHERE sha = 'sha_v17_test'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read back");
        assert_eq!(ai, 1, "is_ai_assisted must be 1");
        assert_eq!(tool, Some("claude".to_string()), "ai_tool must be 'claude'");

        // Verify fact_commit_effort accepts effort_tshirt.
        conn.execute(
            "INSERT INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, \
              formula_version, computed_at, effort_tshirt) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                "sha_v17_test",
                "testrepo",
                "M",
                9.5,
                50_i64,
                2_i64,
                0_i64,
                1.0_f64,
                "v1",
                1_000_000_i64,
                3_i64, // M=3
            ],
        )
        .expect("insert effort with tshirt");

        let tshirt: i64 = conn
            .query_row(
                "SELECT effort_tshirt FROM fact_commit_effort WHERE sha = 'sha_v17_test'",
                [],
                |r| r.get(0),
            )
            .expect("read effort_tshirt");
        assert_eq!(tshirt, 3, "effort_tshirt M must be 3");

        // Verify classifications accepts top_level_category.
        conn.execute(
            "INSERT INTO classifications \
             (category, subcategory, confidence, method, top_level_category) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["feature", "feature", 0.95_f64, "exact_rule", "feature"],
        )
        .expect("insert classification with top_level_category");

        let top: Option<String> = conn
            .query_row(
                "SELECT top_level_category FROM classifications WHERE category = 'feature' \
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("read top_level_category");
        assert_eq!(
            top,
            Some("feature".to_string()),
            "top_level_category must be 'feature'"
        );
    }
}
