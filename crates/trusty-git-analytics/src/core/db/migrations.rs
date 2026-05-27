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
