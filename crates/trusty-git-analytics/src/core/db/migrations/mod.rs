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
//! 3. If the migration requires a pre-flight column check (e.g. ADD COLUMN
//!    guard), add a dedicated `v<N>.rs` submodule following the pattern in
//!    `v17.rs`.

mod v17;
mod v20;
pub(crate) mod v21;

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
        sql: include_str!("../sql/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "linear_issues",
        sql: include_str!("../sql/0002_linear_issues.sql"),
    },
    Migration {
        version: 3,
        name: "commits_ticketed",
        sql: include_str!("../sql/0003_commits_ticketed.sql"),
    },
    Migration {
        version: 4,
        name: "collection_runs",
        sql: include_str!("../sql/0004_collection_runs.sql"),
    },
    Migration {
        version: 5,
        name: "work_items",
        sql: include_str!("../sql/0005_work_items.sql"),
    },
    Migration {
        version: 6,
        name: "classification_overrides",
        sql: include_str!("../sql/0006_classification_overrides.sql"),
    },
    Migration {
        version: 7,
        name: "pr_metrics_and_backfill",
        sql: include_str!("../sql/0007_pr_metrics_and_backfill.sql"),
    },
    Migration {
        version: 8,
        name: "azdo_iterations",
        sql: include_str!("../sql/0008_azdo_iterations.sql"),
    },
    Migration {
        version: 9,
        name: "collection_runs_repo_count",
        sql: include_str!("../sql/0009_collection_runs_repo_count.sql"),
    },
    Migration {
        version: 10,
        name: "pull_requests_provider",
        sql: include_str!("../sql/0010_pull_requests_provider.sql"),
    },
    Migration {
        version: 11,
        name: "pr_reviewers",
        sql: include_str!("../sql/0011_pr_reviewers.sql"),
    },
    Migration {
        version: 12,
        name: "pull_requests_repository",
        sql: include_str!("../sql/0012_pull_requests_repository.sql"),
    },
    Migration {
        version: 13,
        name: "complexity",
        sql: include_str!("../sql/0013_complexity.sql"),
    },
    Migration {
        version: 14,
        name: "dora_tables",
        sql: include_str!("../sql/0014_dora_tables.sql"),
    },
    Migration {
        version: 15,
        name: "tag_release_branch_reachability",
        sql: include_str!("../sql/0015_tag_release_branch_reachability.sql"),
    },
    Migration {
        version: 16,
        name: "fact_commit_effort",
        sql: include_str!("../sql/0016_fact_commit_effort.sql"),
    },
    Migration {
        version: 17,
        name: "pushdown_445",
        sql: include_str!("../sql/0017_pushdown_445.sql"),
    },
    Migration {
        version: 18,
        name: "fact_weekly_quality",
        sql: include_str!("../sql/0018_fact_weekly_quality.sql"),
    },
    Migration {
        version: 19,
        name: "effort_percentile_stats",
        sql: include_str!("../sql/0019_effort_percentile_stats.sql"),
    },
    Migration {
        version: 20,
        name: "pr_reviewers_review_state",
        sql: include_str!("../sql/0020_pr_reviewers_review_state.sql"),
    },
    Migration {
        version: 21,
        name: "agentic_mode",
        // Placeholder — execution is routed to v21::apply (with column guard).
        sql: "",
    },
    Migration {
        version: 22,
        name: "pull_requests_fetched_at",
        sql: include_str!("../sql/0022_pull_requests_fetched_at.sql"),
    },
    Migration {
        version: 23,
        name: "jira_ingestion",
        sql: include_str!("../sql/0023_jira_ingestion.sql"),
    },
    // #5734: a PR's source branch and its body-declared issue ref, so branch
    // names and PR bodies feed the same ticket extraction as commit subjects.
    Migration {
        version: 24,
        name: "pull_requests_head_ref_and_body_ticket",
        sql: include_str!("../sql/0024_pull_requests_head_ref_and_body_ticket.sql"),
    },
    // #6073: per-repo full-history walk bookkeeping.
    Migration {
        version: 25,
        name: "repo_walk_state",
        sql: include_str!("../sql/0025_repo_walk_state.sql"),
    },
    // #3916: PM work meaningfulness tier — `fact_pm_work`.
    Migration {
        version: 26,
        name: "fact_pm_work",
        sql: include_str!("../sql/0026_fact_pm_work.sql"),
    },
    // #3915: PM effort tier — `fact_pm_effort`.
    Migration {
        version: 27,
        name: "fact_pm_effort",
        sql: include_str!("../sql/0027_fact_pm_effort.sql"),
    },
    // #6748: which detector generation produced each stored AI verdict. The
    // DEFAULT of 0 marks every pre-existing row as older than any shipped
    // generation, so the next collect re-classifies it.
    Migration {
        version: 28,
        name: "commit_detector_version",
        sql: include_str!("../sql/0028_commit_detector_version.sql"),
    },
];

/// Ensure the `schema_migrations` bookkeeping table exists.
pub(super) fn ensure_migrations_table(conn: &Connection) -> Result<()> {
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

/// Return the set of column names for `table` by querying `PRAGMA table_info`.
///
/// Used by migration pre-flight checks to guard ADD COLUMN statements that
/// may already have been applied by a pre-release build (SQLite has no
/// `ALTER TABLE … ADD COLUMN IF NOT EXISTS`).
pub(super) fn column_names(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(TgaError::from)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(TgaError::from)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(TgaError::from)?;
    Ok(names)
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
    run_through(conn, i64::MAX)
}

/// Apply migrations up to and including `max_version`.
///
/// Why: `run` always migrates to the newest version, so nothing could build a
/// database at an OLDER schema — and without that, no test can prove an
/// existing database survives a new migration. #5734 adds a column to
/// `pull_requests`, which every deployed database already has rows in.
/// What: the body `run` used to have, with a ceiling. `run` passes
/// [`i64::MAX`], so its behaviour is unchanged.
/// Test: `tests::migration_v24_preserves_an_existing_v23_database`.
///
/// # Errors
///
/// Returns [`TgaError::MigrationError`] if a migration's SQL fails.
pub(crate) fn run_through(conn: &mut Connection, max_version: i64) -> Result<()> {
    ensure_migrations_table(conn)?;
    let current = current_version(conn)?;
    debug!(current_version = current, "running migrations");

    for m in MIGRATIONS {
        if m.version <= current || m.version > max_version {
            continue;
        }
        info!(version = m.version, name = m.name, "applying migration");
        let tx = conn.transaction().map_err(TgaError::from)?;

        if m.version == 17 {
            // Migration 17 requires a pre-flight column check because some
            // pre-release builds added `effort_tshirt` directly in v16's
            // CREATE TABLE; see `v17::apply` for details.
            v17::apply(&tx)?;
        } else if m.version == 21 {
            // Migration 21 requires a pre-flight column check because some
            // pre-release builds may have added `agentic_mode` to `commits`
            // directly; see `v21::apply` for details.
            v21::apply(&tx)?;
        } else {
            tx.execute_batch(m.sql).map_err(|e| {
                TgaError::MigrationError(format!(
                    "migration {} ({}) failed: {e}",
                    m.version, m.name
                ))
            })?;
        }

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

    /// Why: #3916 — `fact_pm_work` carries a FOREIGN KEY back to
    /// `work_items`, which no other fact table in this schema does. A
    /// migration that created the table without the referenced grain, or
    /// with the columns in a different order than the classifier writes,
    /// would fail only at backfill time on a real database.
    /// What: opens an in-memory DB (running every migration through v26),
    /// inserts the parent `work_items` row, writes one verdict, reads every
    /// column back, then re-inserts the same grain and asserts the row count
    /// is still 1.
    /// Test: this test itself.
    #[test]
    fn migration_v26_creates_fact_pm_work() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        conn.execute(
            "INSERT INTO work_items (id, source, title, status, item_type) \
             VALUES ('PM-10215', 'jira', 'Final', 'Done', 'Sub-task')",
            [],
        )
        .expect("insert parent work item");

        let insert = "INSERT OR REPLACE INTO fact_pm_work \
             (work_item_id, work_item_source, pm_name, week_key, is_meaningful, \
              exclusion_reason, title_word_count, body_word_count, formula_version, computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
        conn.execute(
            insert,
            params![
                "PM-10215",
                "jira",
                "Fabiana Calabrese",
                "2026-W03",
                0_i64,
                "TERSE_TITLE",
                1_i64,
                0_i64,
                "pm-work-1",
                1_000_000_i64,
            ],
        )
        .expect("insert pm work row");

        let (pm, week, meaningful, reason, version): (String, String, i64, String, String) = conn
            .query_row(
                "SELECT pm_name, week_key, is_meaningful, exclusion_reason, formula_version \
                 FROM fact_pm_work WHERE work_item_id = 'PM-10215' AND work_item_source = 'jira'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("read back");
        assert_eq!(pm, "Fabiana Calabrese");
        assert_eq!(week, "2026-W03");
        assert_eq!(meaningful, 0);
        assert_eq!(reason, "TERSE_TITLE");
        assert_eq!(version, "pm-work-1");

        // Re-running the classifier over the same ticket replaces the row.
        conn.execute(
            insert,
            params![
                "PM-10215",
                "jira",
                "Fabiana Calabrese",
                "2026-W03",
                1_i64,
                "NONE",
                1_i64,
                42_i64,
                "pm-work-1",
                2_000_000_i64,
            ],
        )
        .expect("upsert pm work row");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_pm_work", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "UPSERT must not duplicate the grain row");
    }

    /// Why: the FOREIGN KEY is the design decision that separates this fact
    /// table from `fact_commit_effort` (#3916) — every verdict is DERIVED
    /// from a `work_items` row, so one for a ticket the database has never
    /// seen is a bug rather than a valid write ordering.
    /// What: writes a verdict whose parent grain does not exist and asserts
    /// SQLite rejects it.
    /// Test: this test itself.
    #[test]
    fn fact_pm_work_rejects_a_verdict_with_no_work_item() {
        let db = Database::open_in_memory().expect("open db");
        let err = db.connection().execute(
            "INSERT INTO fact_pm_work \
             (work_item_id, work_item_source, is_meaningful, exclusion_reason, \
              title_word_count, body_word_count, formula_version, computed_at) \
             VALUES ('NOPE-1', 'jira', 1, 'NONE', 3, 40, 'pm-work-1', 1)",
            [],
        );
        assert!(
            err.is_err(),
            "a verdict without its work_items parent must be rejected"
        );
    }

    /// Why: #3915 — `fact_pm_effort` is the first fact table in this schema
    /// whose measure columns are deliberately NULLABLE. A ticket inside the
    /// recency floor stores NULL in `effort_score` and `effort_bucket` and
    /// says why in `score_status`; a migration that made either column NOT
    /// NULL would force the zero the issue exists to prevent, and would fail
    /// only at backfill time on a real database.
    /// What: opens an in-memory DB (running every migration through v27),
    /// inserts the parent `work_items` row, writes one scored row and one
    /// deferred row, reads every column back, then re-inserts the same grain
    /// and asserts the row count is unchanged.
    /// Test: this test itself.
    #[test]
    fn migration_v27_creates_fact_pm_effort() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        for id in ["ML-2314", "ML-2315"] {
            conn.execute(
                "INSERT INTO work_items (id, source, title, status, item_type) \
                 VALUES (?1, 'jira', 'ML Plat - Observability', 'To Do', 'Epic')",
                params![id],
            )
            .expect("insert parent work item");
        }

        let insert = "INSERT OR REPLACE INTO fact_pm_effort \
             (work_item_id, work_item_source, pm_name, week_key, effort_score, effort_bucket, \
              score_status, epic_children_count, description_word_count, comment_count, \
              transition_count, story_points, inputs_present, age_days_at_score, \
              formula_version, computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)";
        conn.execute(
            insert,
            params![
                "ML-2314",
                "jira",
                "Rohit Puntambekar",
                "2026-W28",
                31.5_f64,
                "HIGH",
                "SCORED",
                9_i64,
                400_i64,
                5_i64,
                6_i64,
                8.0_f64,
                "CHILDREN,DESCRIPTION,COMMENTS,TRANSITIONS,STORY_POINTS",
                45_i64,
                "pm-effort-1",
                1_000_000_i64,
            ],
        )
        .expect("insert scored row");

        // The deferred shape: NULL score and NULL bucket, with the reason in
        // score_status and no story points.
        conn.execute(
            insert,
            params![
                "ML-2315",
                "jira",
                "Rohit Puntambekar",
                "2026-W28",
                None::<f64>,
                None::<String>,
                "DEFERRED_RECENT",
                0_i64,
                12_i64,
                0_i64,
                0_i64,
                None::<f64>,
                "NONE",
                2_i64,
                "pm-effort-1",
                1_000_000_i64,
            ],
        )
        .expect("insert deferred row");

        /// Every column `migration_v27_creates_fact_pm_effort` reads back.
        type ScoredRow = (
            Option<f64>,
            Option<String>,
            String,
            i64,
            Option<f64>,
            String,
            Option<i64>,
            String,
        );
        let (score, bucket, status, children, points, inputs, age, version): ScoredRow = conn
            .query_row(
                "SELECT effort_score, effort_bucket, score_status, epic_children_count, \
                 story_points, inputs_present, age_days_at_score, formula_version \
                 FROM fact_pm_effort WHERE work_item_id = 'ML-2314' AND work_item_source = 'jira'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .expect("read scored row back");
        assert_eq!(score, Some(31.5));
        assert_eq!(bucket.as_deref(), Some("HIGH"));
        assert_eq!(status, "SCORED");
        assert_eq!(children, 9);
        assert_eq!(points, Some(8.0));
        assert_eq!(
            inputs,
            "CHILDREN,DESCRIPTION,COMMENTS,TRANSITIONS,STORY_POINTS"
        );
        assert_eq!(age, Some(45));
        assert_eq!(version, "pm-effort-1");

        let (deferred_score, deferred_bucket, deferred_status): (
            Option<f64>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT effort_score, effort_bucket, score_status FROM fact_pm_effort \
                 WHERE work_item_id = 'ML-2315'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read deferred row back");
        assert_eq!(
            deferred_score, None,
            "a deferred ticket stores NULL, never a zero"
        );
        assert_eq!(deferred_bucket, None);
        assert_eq!(deferred_status, "DEFERRED_RECENT");

        // Re-running the scorer over the same ticket replaces the row.
        conn.execute(
            insert,
            params![
                "ML-2314",
                "jira",
                "Rohit Puntambekar",
                "2026-W28",
                12.0_f64,
                "LOW",
                "SCORED",
                1_i64,
                400_i64,
                0_i64,
                0_i64,
                None::<f64>,
                "CHILDREN,DESCRIPTION",
                60_i64,
                "pm-effort-1",
                2_000_000_i64,
            ],
        )
        .expect("upsert pm effort row");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_pm_effort", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 2, "UPSERT must not duplicate the grain row");
    }

    /// Why: same reasoning as `fact_pm_work_rejects_a_verdict_with_no_work_item`
    /// — every `fact_pm_effort` row is DERIVED from a `work_items` row
    /// (#3915), so one for a ticket the database has never seen is a bug.
    /// What: writes a score whose parent grain does not exist and asserts
    /// SQLite rejects it.
    /// Test: this test itself.
    #[test]
    fn fact_pm_effort_rejects_a_score_with_no_work_item() {
        let db = Database::open_in_memory().expect("open db");
        let err = db.connection().execute(
            "INSERT INTO fact_pm_effort \
             (work_item_id, work_item_source, score_status, epic_children_count, \
              description_word_count, comment_count, transition_count, inputs_present, \
              formula_version, computed_at) \
             VALUES ('NOPE-1', 'jira', 'SCORED', 0, 0, 0, 0, 'NONE', 'pm-effort-1', 1)",
            [],
        );
        assert!(
            err.is_err(),
            "a score without its work_items parent must be rejected"
        );
    }

    /// Why: #3915 lands `fact_pm_effort` on databases that already hold v26
    /// data — `work_items` rows and the `fact_pm_work` verdicts derived from
    /// them. `migration_v27_creates_fact_pm_effort` only opens a brand-new
    /// in-memory database, which runs every migration from v1 forward and so
    /// never exercises the upgrade path a deployed database takes. A v27 that
    /// dropped or rebuilt `fact_pm_work` would pass that test while silently
    /// destroying every stored verdict.
    /// What: builds a genuine v26 database (the schema shipped before #3915),
    /// writes a `work_items` row and the `fact_pm_work` verdict for it,
    /// upgrades to v27, then asserts both rows survive unchanged, a
    /// `fact_pm_effort` row referencing that same ticket inserts, and the
    /// registry reads 27. Mirrors
    /// `migration_v24_preserves_an_existing_v23_database`.
    /// Test: this test itself.
    #[test]
    fn migration_v27_preserves_an_existing_v26_database() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open");
        // The pragma every real connection carries — see `Database::apply_pragmas`.
        // Without it the new table's FOREIGN KEY is inert.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("enable foreign keys");
        super::run_through(&mut conn, 26).expect("migrate to v26");

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("read version");
        assert_eq!(version, 26, "the fixture must be a real pre-#3915 database");

        // Rows written the way the v26 classifier wrote them, before
        // `fact_pm_effort` existed.
        conn.execute(
            "INSERT INTO work_items (id, source, title, status, item_type) \
             VALUES ('ML-2200', 'jira', 'ML Plat - Feature store rollout', 'Done', 'Epic')",
            [],
        )
        .expect("insert v26-era work item");
        conn.execute(
            "INSERT INTO fact_pm_work \
             (work_item_id, work_item_source, pm_name, week_key, is_meaningful, \
              exclusion_reason, title_word_count, body_word_count, formula_version, computed_at) \
             VALUES ('ML-2200', 'jira', 'Rohit Puntambekar', '2026-W20', 1, 'NONE', 5, 180, \
                     'pm-work-1', 1700000000)",
            [],
        )
        .expect("insert v26-era verdict");

        // The upgrade every existing v26 database performs on next open.
        super::run_through(&mut conn, 27).expect("migrate to v27");

        let upgraded: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("read version");
        assert_eq!(upgraded, 27, "the registry must record the new migration");

        // The seeded rows survive the new CREATE TABLE untouched.
        let (title, status): (String, String) = conn
            .query_row(
                "SELECT title, status FROM work_items \
                 WHERE id = 'ML-2200' AND source = 'jira'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the pre-migration work item must still be readable");
        assert_eq!(title, "ML Plat - Feature store rollout");
        assert_eq!(status, "Done");

        let (pm, week, meaningful, body_words, computed): (String, String, i64, i64, i64) = conn
            .query_row(
                "SELECT pm_name, week_key, is_meaningful, body_word_count, computed_at \
                 FROM fact_pm_work WHERE work_item_id = 'ML-2200' AND work_item_source = 'jira'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("the pre-migration verdict must still be readable");
        assert_eq!(pm, "Rohit Puntambekar");
        assert_eq!(week, "2026-W20");
        assert_eq!(meaningful, 1);
        assert_eq!(body_words, 180);
        assert_eq!(computed, 1_700_000_000);

        let verdicts: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_pm_work", [], |r| r.get(0))
            .expect("count verdicts");
        assert_eq!(verdicts, 1, "v27 must not drop or duplicate v26 rows");

        // The new table accepts a score for that pre-existing ticket.
        conn.execute(
            "INSERT INTO fact_pm_effort \
             (work_item_id, work_item_source, pm_name, week_key, effort_score, \
              effort_bucket, score_status, epic_children_count, description_word_count, \
              comment_count, transition_count, story_points, inputs_present, \
              age_days_at_score, formula_version, computed_at) \
             VALUES ('ML-2200', 'jira', 'Rohit Puntambekar', '2026-W20', 22.5, 'MEDIUM', \
                     'SCORED', 4, 180, 3, 7, NULL, \
                     'CHILDREN,DESCRIPTION,COMMENTS,TRANSITIONS', 90, 'pm-effort-1', \
                     1700000001)",
            [],
        )
        .expect("scoring a ticket the v26 database already knew must succeed");

        let (score, bucket): (Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT effort_score, effort_bucket FROM fact_pm_effort \
                 WHERE work_item_id = 'ML-2200' AND work_item_source = 'jira'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read the new row back");
        assert_eq!(score, Some(22.5));
        assert_eq!(bucket.as_deref(), Some("MEDIUM"));

        // Re-running is a no-op, per the runner's idempotence contract.
        super::run_through(&mut conn, 27).expect("re-run is idempotent");
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

    /// Why: #5734 adds two columns to `pull_requests`, a table every deployed
    /// database already has rows in. A migration that dropped or invalidated
    /// those rows would be silent — the collector would simply re-fetch and
    /// nobody would see the loss.
    /// What: builds a genuine v23 database (the schema shipped before #5734),
    /// writes a PR row through the OLD column set, then migrates to the head
    /// and asserts the row survives byte-for-byte, its new columns read as the
    /// documented "no claim" defaults, and the new columns are writable.
    /// Test: this test itself.
    #[test]
    fn migration_v24_preserves_an_existing_v23_database() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open");
        super::run_through(&mut conn, 23).expect("migrate to v23");

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("read version");
        assert_eq!(version, 23, "the fixture must be a real pre-#5734 database");

        // A row written the way the v23 collector wrote it — no head_ref, no
        // body_ticket_id, because neither column existed.
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, \
              merged_at, commit_shas, fetched_at) \
             VALUES ('github','acme/widgets',7,'Old PR','ada','merged', \
                     '2026-01-01T00:00:00Z','2026-01-02T00:00:00Z','[\"deadbeef\"]', \
                     '2026-01-02T00:00:01Z')",
            [],
        )
        .expect("insert v23-era pr");

        // The upgrade every existing database performs on next open.
        super::run(&mut conn).expect("migrate to head");

        let (title, shas, fetched, head_ref, body_key): (
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT title, commit_shas, fetched_at, head_ref, body_ticket_id \
                 FROM pull_requests WHERE pr_number = 7",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .expect("the pre-migration row must still be readable");

        assert_eq!(title, "Old PR", "existing data must survive untouched");
        assert_eq!(shas, "[\"deadbeef\"]");
        assert_eq!(fetched, "2026-01-02T00:00:01Z");
        assert_eq!(head_ref, "", "the documented default for an existing row");
        assert_eq!(body_key, None);

        // #5734: and the new columns are writable on the upgraded database.
        conn.execute(
            "UPDATE pull_requests SET head_ref = 'feature/PROJ-1-x', \
             body_ticket_id = '#42' WHERE pr_number = 7",
            [],
        )
        .expect("write the new columns");
        let round_trip: String = conn
            .query_row(
                "SELECT head_ref FROM pull_requests WHERE pr_number = 7",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(round_trip, "feature/PROJ-1-x");

        // Re-running is a no-op, per the runner's idempotence contract.
        super::run(&mut conn).expect("re-run is idempotent");
    }

    // Migration v20 tests live in `v20.rs`.
    #[test]
    fn migration_v20_adds_review_state_columns() {
        crate::core::db::migrations::v20::tests::migration_v20_adds_review_state_columns();
    }

    // Migration v21 tests live in `v21.rs`.
    #[test]
    fn migration_v21_adds_agentic_mode_and_fwe() {
        crate::core::db::migrations::v21::tests::migration_v21_adds_agentic_mode_and_fwe();
    }

    #[test]
    fn migration_v21_is_idempotent_when_agentic_mode_already_exists() {
        crate::core::db::migrations::v21::tests::migration_v21_is_idempotent_when_agentic_mode_already_exists();
    }

    /// Why: #6748 — every deployed `tga.db` predates `ai_detector_version`, and
    /// the whole repair depends on those rows sorting as older than the shipped
    /// detector. A column added with any other default, or a second `run` that
    /// re-stamped rows the first pass had already advanced, would leave the
    /// affected corpus unrepaired or repair it on every open forever.
    /// What: builds a real v27 database, writes a commit the v27 collector's
    /// way, upgrades to head, and asserts the column exists at 0 with the row
    /// intact. Then runs the migrations a second time and asserts nothing
    /// moved — neither the column value nor the `schema_migrations` row.
    /// Test: this test itself.
    #[test]
    fn migration_v28_marks_an_existing_database_as_stale_and_is_idempotent() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open");
        super::run_through(&mut conn, 27).expect("migrate to v27");

        conn.execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, \
              is_ai_assisted, ai_tool, agentic_mode) \
             VALUES ('v27sha', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', \
                     ?1, 'testrepo', 0, NULL, 'none')",
            params!["feat: x\n\nCo-Authored-By: Claude <noreply@anthropic.com>"],
        )
        .expect("insert v27-era commit");

        // The upgrade every existing database performs on next open.
        super::run(&mut conn).expect("migrate to head");

        fn read(conn: &rusqlite::Connection) -> (i64, i64, String) {
            conn.query_row(
                "SELECT ai_detector_version, is_ai_assisted, agentic_mode \
                 FROM commits WHERE sha = 'v27sha'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read migrated row")
        }
        assert_eq!(
            read(&conn),
            (0, 0, "none".to_string()),
            "an existing row is stale, and its stored verdict is left untouched"
        );

        // Run it twice: the second pass must be a no-op.
        super::run(&mut conn).expect("re-run is idempotent");
        assert_eq!(read(&conn), (0, 0, "none".to_string()));
        let applied: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 28",
                [],
                |r| r.get(0),
            )
            .expect("count v28 rows");
        assert_eq!(applied, 1, "v28 is recorded exactly once");
    }
}
