//! Versioned SQL migrations.
//!
//! Migrations are stored as a static list of `(version, name, sql)` tuples
//! and applied in order. Each migration is wrapped in a transaction along
//! with the corresponding row insert into `schema_migrations`, so partial
//! application is impossible.
//!
//! Adding a new migration:
//! 1. Append a new entry to [`MIGRATIONS`] with a strictly increasing version.
//! 2. Never edit an existing migration in place — write a follow-up migration.

use rusqlite::Connection;
use tracing::{debug, info};

use crate::core::errors::{Result, TgaError};

/// A single migration step.
pub struct Migration {
    /// Strictly increasing version number; must be unique.
    pub version: i64,
    /// Human-readable label, recorded for audit/debugging.
    pub name: &'static str,
    /// The SQL to execute. May contain multiple statements separated by `;`.
    pub sql: &'static str,
}

/// All migrations known to this binary, in order of application.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("sql/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "linear_issues",
        sql: include_str!("sql/0002_linear_issues.sql"),
    },
    Migration {
        version: 3,
        name: "commits_ticketed",
        sql: include_str!("sql/0003_commits_ticketed.sql"),
    },
    Migration {
        version: 4,
        name: "collection_runs",
        sql: include_str!("sql/0004_collection_runs.sql"),
    },
    Migration {
        version: 5,
        name: "work_items",
        sql: include_str!("sql/0005_work_items.sql"),
    },
    Migration {
        version: 6,
        name: "classification_overrides",
        sql: include_str!("sql/0006_classification_overrides.sql"),
    },
    Migration {
        version: 7,
        name: "pr_metrics_and_backfill",
        sql: include_str!("sql/0007_pr_metrics_and_backfill.sql"),
    },
    Migration {
        version: 8,
        name: "azdo_iterations",
        sql: include_str!("sql/0008_azdo_iterations.sql"),
    },
    Migration {
        version: 9,
        name: "collection_runs_repo_count",
        sql: include_str!("sql/0009_collection_runs_repo_count.sql"),
    },
    Migration {
        version: 10,
        name: "pull_requests_provider",
        sql: include_str!("sql/0010_pull_requests_provider.sql"),
    },
    Migration {
        version: 11,
        name: "pr_reviewers",
        sql: include_str!("sql/0011_pr_reviewers.sql"),
    },
    Migration {
        version: 12,
        name: "pull_requests_repository",
        sql: include_str!("sql/0012_pull_requests_repository.sql"),
    },
    Migration {
        version: 13,
        name: "complexity",
        sql: include_str!("sql/0013_complexity.sql"),
    },
    Migration {
        version: 14,
        name: "dora_tables",
        sql: include_str!("sql/0014_dora_tables.sql"),
    },
    Migration {
        version: 15,
        name: "tag_release_branch_reachability",
        sql: include_str!("sql/0015_tag_release_branch_reachability.sql"),
    },
    Migration {
        version: 16,
        name: "fact_commit_effort",
        sql: include_str!("sql/0016_fact_commit_effort.sql"),
    },
    Migration {
        version: 17,
        name: "pushdown_445",
        sql: include_str!("sql/0017_pushdown_445.sql"),
    },
    Migration {
        version: 18,
        name: "fact_weekly_quality",
        sql: include_str!("sql/0018_fact_weekly_quality.sql"),
    },
];

/// Ensure the `schema_migrations` bookkeeping table exists.
fn ensure_migrations_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations ( \
            version    INTEGER PRIMARY KEY, \
            name       TEXT NOT NULL, \
            applied_at TEXT NOT NULL \
        );",
    )?;
    Ok(())
}

/// Return the highest applied migration version, or 0 if none have been applied.
fn current_version(conn: &Connection) -> Result<i64> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    Ok(v.unwrap_or(0))
}

/// Apply all migrations whose version is greater than the current schema version.
///
/// Idempotent: running it twice in a row is a no-op the second time.
///
/// # Errors
///
/// Returns [`TgaError::MigrationError`] if a migration's SQL fails. The
/// transaction guarantees partial application cannot occur.
pub fn run(conn: &mut Connection) -> Result<()> {
    ensure_migrations_table(conn)?;
    let current = current_version(conn)?;
    debug!(current_version = current, "running migrations");

    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        info!(version = m.version, name = m.name, "applying migration");
        let tx = conn.transaction().map_err(TgaError::from)?;
        tx.execute_batch(m.sql).map_err(|e| {
            TgaError::MigrationError(format!("migration {} ({}) failed: {e}", m.version, m.name))
        })?;
        tx.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.version, m.name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(TgaError::from)?;
        tx.commit().map_err(TgaError::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::db::Database;
    use rusqlite::params;

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

    /// Why: regression guard for issue #445 batch B. Migration v18 creates
    /// `fact_weekly_quality` with all required columns and a PRIMARY KEY on
    /// (author_email, iso_year, iso_week, repository).
    /// What: opens an in-memory DB (which runs all migrations up to v18),
    /// UPSERTs a quality row, reads it back, verifies all columns, and
    /// confirms that re-inserting the same grain key overwrites rather than
    /// duplicating (UPSERT semantics).
    /// Test: this test itself.
    #[test]
    fn migration_v18_creates_fact_weekly_quality() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Insert a quality row.
        conn.execute(
            "INSERT OR REPLACE INTO fact_weekly_quality \
             (author_email, iso_year, iso_week, repository, quality_score, quality_tshirt, \
              revert_count, bugfix_count, ticketed_count, commit_count, formula_version, \
              computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                "alice@example.com",
                2026_i64,
                5_i64,
                "testrepo",
                0.6875_f64,
                4_i64,
                1_i64,
                1_i64,
                2_i64,
                4_i64,
                "v1",
                1_000_000_i64,
            ],
        )
        .expect("insert quality row");

        // Read it back and verify columns.
        let (score, tshirt, reverts, bugfixes, ticketed, commits): (f64, i64, i64, i64, i64, i64) =
            conn.query_row(
                "SELECT quality_score, quality_tshirt, revert_count, bugfix_count, \
                 ticketed_count, commit_count \
                 FROM fact_weekly_quality \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 \
                   AND iso_week = 5 AND repository = 'testrepo'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("read back");
        assert!(
            (score - 0.6875).abs() < 1e-9,
            "quality_score must be 0.6875, got {score}"
        );
        assert_eq!(tshirt, 4, "quality_tshirt must be 4");
        assert_eq!(reverts, 1);
        assert_eq!(bugfixes, 1);
        assert_eq!(ticketed, 2);
        assert_eq!(commits, 4);

        // Verify UPSERT: second insert with updated score must overwrite (not duplicate).
        conn.execute(
            "INSERT OR REPLACE INTO fact_weekly_quality \
             (author_email, iso_year, iso_week, repository, quality_score, quality_tshirt, \
              revert_count, bugfix_count, ticketed_count, commit_count, formula_version, \
              computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                "alice@example.com",
                2026_i64,
                5_i64,
                "testrepo",
                1.0_f64, // updated score
                5_i64,
                0_i64,
                0_i64,
                4_i64,
                4_i64,
                "v1",
                2_000_000_i64,
            ],
        )
        .expect("upsert quality row");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fact_weekly_quality \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 \
                   AND iso_week = 5 AND repository = 'testrepo'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "UPSERT must not duplicate the grain row");

        let new_score: f64 = conn
            .query_row(
                "SELECT quality_score FROM fact_weekly_quality \
                 WHERE author_email = 'alice@example.com' AND iso_year = 2026 \
                   AND iso_week = 5 AND repository = 'testrepo'",
                [],
                |r| r.get(0),
            )
            .expect("new score");
        assert!(
            (new_score - 1.0).abs() < 1e-9,
            "UPSERT must overwrite the score with 1.0, got {new_score}"
        );
    }

    /// Why: regression guard for issue #88. Before migration v12, the
    /// UNIQUE(provider, pr_number) index collapsed cross-repo PRs that
    /// happened to share a number (e.g. #1 in repo A and #1 in repo B),
    /// losing ~62% of rows in real org-wide collection runs.
    /// What: after running all migrations, two rows with identical
    /// `(provider, pr_number)` but different `repository` must coexist;
    /// inserting a third row with the same `(provider, repository, pr_number)`
    /// must replace, not duplicate.
    /// Test: open in-memory DB (runs all migrations), insert, assert counts.
    #[test]
    fn migration_v12_allows_same_pr_number_across_repositories() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Two PRs, same provider and pr_number, different repositories.
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "github",
                "acme/widgets",
                1_i64,
                "first repo PR #1",
                "alice",
                "open",
                "2024-01-01T00:00:00Z",
                "[]"
            ],
        )
        .expect("insert A");
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "github",
                "acme/gadgets",
                1_i64,
                "second repo PR #1",
                "bob",
                "open",
                "2024-01-02T00:00:00Z",
                "[]"
            ],
        )
        .expect("insert B");

        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_requests WHERE provider = 'github' AND pr_number = 1",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            total, 2,
            "same (provider, pr_number) across two repositories must yield two rows after v12"
        );

        // INSERT OR REPLACE on the same triple must still deduplicate.
        conn.execute(
            "INSERT OR REPLACE INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "github",
                "acme/widgets",
                1_i64,
                "first repo PR #1 (updated)",
                "alice",
                "merged",
                "2024-01-01T00:00:00Z",
                "[]"
            ],
        )
        .expect("replace A");

        let still_two: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_requests WHERE provider = 'github' AND pr_number = 1",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            still_two, 2,
            "INSERT OR REPLACE on the same triple must not add a row"
        );

        let updated_state: String = conn
            .query_row(
                "SELECT state FROM pull_requests \
                 WHERE provider = 'github' AND repository = 'acme/widgets' AND pr_number = 1",
                [],
                |row| row.get(0),
            )
            .expect("read state");
        assert_eq!(
            updated_state, "merged",
            "REPLACE must update fields in place"
        );
    }
}
