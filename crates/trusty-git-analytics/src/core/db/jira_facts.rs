//! Persistence + freshness helpers for the JIRA ingestion fact tables
//! (issue #3966): `fact_ticket_transitions`, `fact_jira_comment_detail`, and
//! the `jira_sync_cursor` incremental-sync bookkeeping table.
//!
//! Schema: `sql/0023_jira_ingestion.sql`. See that file's header comment for
//! the field-coverage constraints (NULL `epic_key`, 0% `acceptance_criteria`,
//! 76%-NULL `story_points`) baked into the design — none of those fields are
//! part of either fact table's grain, so they are documented rather than
//! modeled.

use rusqlite::{params, Connection, OptionalExtension};
use tracing::debug;

use crate::core::errors::{Result, TgaError};

/// One row of `fact_ticket_transitions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketTransitionRow {
    /// JIRA issue key, e.g. `PROJ-123`.
    pub ticket_key: String,
    /// JIRA project key, e.g. `PROJ`.
    pub project_key: String,
    /// Status before the transition. `None` for a ticket's initial creation
    /// state (JIRA's first changelog entry for a field has no `from`).
    pub from_status: Option<String>,
    /// Status after the transition.
    pub to_status: String,
    /// RFC3339 timestamp of the transition.
    pub transitioned_at: String,
    /// Display name of the author who made the transition, when JIRA
    /// reports one.
    pub author: Option<String>,
}

/// One row of `fact_jira_comment_detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentDetailRow {
    /// JIRA issue key the comment belongs to.
    pub ticket_key: String,
    /// JIRA comment ID (unique per-instance).
    pub comment_id: String,
    /// JIRA project key, denormalized for cheap filtering.
    pub project_key: String,
    /// Display name of the comment author, when JIRA reports one.
    pub author: Option<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// Length of the comment body. See `collect::jira::client` doc comments
    /// for the Atlassian-Document-Format-vs-plain-text caveat.
    pub body_len: i64,
}

/// Upsert one `fact_ticket_transitions` row.
///
/// Idempotent: `INSERT OR REPLACE` against the
/// `(ticket_key, transitioned_at, to_status)` primary key means re-running a
/// sync (incremental or full backfill) over the same transition is a no-op
/// besides refreshing `synced_at` — which is exactly the freshness signal
/// the assertion relies on.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn upsert_ticket_transition(conn: &Connection, row: &TicketTransitionRow) -> Result<()> {
    let synced_at = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT OR REPLACE INTO fact_ticket_transitions \
         (ticket_key, project_key, from_status, to_status, transitioned_at, author, synced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.ticket_key,
            row.project_key,
            row.from_status,
            row.to_status,
            row.transitioned_at,
            row.author,
            synced_at,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(
        ticket = %row.ticket_key,
        to_status = %row.to_status,
        transitioned_at = %row.transitioned_at,
        "upserted ticket transition"
    );
    Ok(())
}

/// Upsert one `fact_jira_comment_detail` row.
///
/// Idempotent: `INSERT OR REPLACE` against `(ticket_key, comment_id)`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn upsert_comment_detail(conn: &Connection, row: &CommentDetailRow) -> Result<()> {
    let synced_at = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT OR REPLACE INTO fact_jira_comment_detail \
         (ticket_key, comment_id, project_key, author, created_at, body_len, synced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.ticket_key,
            row.comment_id,
            row.project_key,
            row.author,
            row.created_at,
            row.body_len,
            synced_at,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(
        ticket = %row.ticket_key,
        comment_id = %row.comment_id,
        "upserted comment detail"
    );
    Ok(())
}

/// Stored incremental-sync cursor for a JIRA project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraSyncCursor {
    /// RFC3339 timestamp: the `updated >=` JQL cursor for the next
    /// incremental run.
    pub last_synced_at: String,
    /// RFC3339 timestamp of the last successful sync invocation's wall-clock
    /// completion.
    pub last_run_at: String,
    /// Number of tickets processed in the last successful run.
    pub tickets_synced: i64,
}

/// Fetch the stored sync cursor for `project_key`, or `None` if this project
/// has never completed a sync.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn get_cursor(conn: &Connection, project_key: &str) -> Result<Option<JiraSyncCursor>> {
    conn.query_row(
        "SELECT last_synced_at, last_run_at, tickets_synced \
         FROM jira_sync_cursor WHERE project_key = ?1",
        params![project_key],
        |row| {
            Ok(JiraSyncCursor {
                last_synced_at: row.get(0)?,
                last_run_at: row.get(1)?,
                tickets_synced: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(TgaError::from)
}

/// Record (overwrite) the sync cursor for `project_key` after a successful
/// run.
///
/// `last_run_at` is stamped with the current wall-clock time; callers supply
/// only the JQL cursor (`last_synced_at`) and ticket count.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn set_cursor(
    conn: &Connection,
    project_key: &str,
    last_synced_at: &str,
    tickets_synced: i64,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO jira_sync_cursor \
         (project_key, last_synced_at, last_run_at, tickets_synced) \
         VALUES (?1, ?2, ?3, ?4)",
        params![project_key, last_synced_at, now, tickets_synced],
    )
    .map_err(TgaError::from)?;
    debug!(project = %project_key, last_synced_at, tickets_synced, "recorded jira sync cursor");
    Ok(())
}

/// Freshness verdict for a single JIRA fact table.
///
/// Why: the issue calls out (as a CRITICAL, non-negotiable acceptance
/// criterion) that any TGA-produced fact table needs an explicit guard that
/// fails loudly — not silently — when it goes stale or stays empty. This is
/// the same failure mode that caused `fact_commit_effort` to serve a
/// hardcoded fallback with no alarm when its populating command was never
/// wired into cron.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessStatus {
    /// Table name checked.
    pub table: &'static str,
    /// JIRA project this verdict covers, or `None` when the check was run
    /// across every project in the table.
    pub project: Option<String>,
    /// Total row count in the table.
    pub row_count: i64,
    /// Unix-seconds timestamp of the most recently *synced* (not
    /// business-dated) row, or `None` if the table is empty.
    pub max_synced_at: Option<i64>,
    /// Age of `max_synced_at` in seconds relative to "now", or `None` when
    /// the table is empty.
    pub age_seconds: Option<i64>,
    /// `true` if the table is empty OR its freshest row is older than the
    /// caller's threshold.
    pub stale: bool,
}

/// Every JIRA project that has ever recorded a sync cursor.
///
/// Why: the freshness guard defaults to checking *all* of them. A
/// table-wide freshness aggregate is blind to a per-project outage — with
/// projects A and B on a schedule, B's sync dying (revoked credentials, a
/// renamed key, a cron entry that quietly stopped) leaves A's writes keeping
/// `MAX(synced_at)` recent, and the guard prints OK for a dead ingestion
/// path. Enumerating the cursor table makes "which projects should be
/// fresh?" a fact the checker reads rather than an argument the operator has
/// to remember to pass.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn list_cursor_projects(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT project_key FROM jira_sync_cursor ORDER BY project_key")
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// Check freshness of both JIRA fact tables against `max_age_days`,
/// optionally restricted to one `project_key`.
///
/// Why measuring `MAX(synced_at)` and not `MAX(transitioned_at)` /
/// `MAX(created_at)`: a historical backfill populates old business
/// timestamps once and then the sync could silently stop running forever —
/// business-dated freshness would never notice. `synced_at` is stamped at
/// write time on every run, so it directly measures "did the sync run
/// recently", which is the actual question a cron-health check needs
/// answered.
///
/// Why `project_key` matters: without it this aggregate is table-wide, so
/// one healthy project masks another project's dead sync — see
/// [`list_cursor_projects`]. `None` keeps the original all-projects
/// behaviour, which is still the right answer for a single-project install
/// and for a database with no cursors yet.
///
/// An empty (or empty-for-this-project) table is always reported
/// `stale = true` regardless of `max_age_days` — this is the "effort-scoring
/// cron gap" failure mode from the issue (a fact table that was never wired
/// into cron at all).
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if either query fails.
pub fn check_freshness(
    conn: &Connection,
    max_age_days: i64,
    project_key: Option<&str>,
) -> Result<Vec<FreshnessStatus>> {
    let now = chrono::Utc::now().timestamp();
    let threshold_seconds = max_age_days.saturating_mul(86_400);

    let mut out = Vec::with_capacity(2);
    for table in ["fact_ticket_transitions", "fact_jira_comment_detail"] {
        let sql = match project_key {
            Some(_) => {
                format!("SELECT COUNT(*), MAX(synced_at) FROM {table} WHERE project_key = ?1")
            }
            None => format!("SELECT COUNT(*), MAX(synced_at) FROM {table}"),
        };
        let map_row = |row: &rusqlite::Row<'_>| Ok((row.get(0)?, row.get(1)?));
        let (row_count, max_synced_at): (i64, Option<i64>) = match project_key {
            Some(key) => conn.query_row(&sql, params![key], map_row),
            None => conn.query_row(&sql, [], map_row),
        }
        .map_err(TgaError::from)?;

        let age_seconds = max_synced_at.map(|t| now - t);
        let stale = match age_seconds {
            None => true, // empty table (for this scope)
            Some(age) => age > threshold_seconds,
        };

        out.push(FreshnessStatus {
            table,
            project: project_key.map(str::to_string),
            row_count,
            max_synced_at,
            age_seconds,
            stale,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    fn sample_transition(key: &str, to_status: &str, at: &str) -> TicketTransitionRow {
        TicketTransitionRow {
            ticket_key: key.to_string(),
            project_key: "PROJ".to_string(),
            from_status: Some("To Do".to_string()),
            to_status: to_status.to_string(),
            transitioned_at: at.to_string(),
            author: Some("Jane Doe".to_string()),
        }
    }

    fn sample_comment(key: &str, id: &str) -> CommentDetailRow {
        CommentDetailRow {
            ticket_key: key.to_string(),
            comment_id: id.to_string(),
            project_key: "PROJ".to_string(),
            author: Some("Jane Doe".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            body_len: 42,
        }
    }

    #[test]
    fn upsert_transition_then_query_roundtrips() {
        let db = Database::open_in_memory().expect("open");
        let row = sample_transition("PROJ-1", "In Progress", "2026-01-01T00:00:00Z");
        upsert_ticket_transition(db.connection(), &row).expect("upsert");

        let (count, to_status): (i64, String) = db
            .connection()
            .query_row(
                "SELECT COUNT(*), to_status FROM fact_ticket_transitions WHERE ticket_key = ?1",
                params!["PROJ-1"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query");
        assert_eq!(count, 1);
        assert_eq!(to_status, "In Progress");
    }

    /// Re-running the same transition (same ticket/timestamp/to_status) must
    /// not duplicate the row — this is what makes both incremental and
    /// backfill re-runs idempotent.
    #[test]
    fn upsert_transition_is_idempotent_on_grain_key() {
        let db = Database::open_in_memory().expect("open");
        let row = sample_transition("PROJ-1", "In Progress", "2026-01-01T00:00:00Z");
        upsert_ticket_transition(db.connection(), &row).expect("first");
        upsert_ticket_transition(db.connection(), &row).expect("second");

        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_ticket_transitions WHERE ticket_key = ?1",
                params!["PROJ-1"],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 1,
            "re-inserting the same grain key must not duplicate"
        );
    }

    /// A ticket that revisits the same `to_status` twice (e.g. reopened)
    /// must yield two distinct rows because `transitioned_at` differs.
    #[test]
    fn upsert_transition_keeps_repeat_status_visits_distinct() {
        let db = Database::open_in_memory().expect("open");
        upsert_ticket_transition(
            db.connection(),
            &sample_transition("PROJ-2", "Done", "2026-01-01T00:00:00Z"),
        )
        .expect("first done");
        upsert_ticket_transition(
            db.connection(),
            &sample_transition("PROJ-2", "Done", "2026-02-01T00:00:00Z"),
        )
        .expect("reopened then re-done");

        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_ticket_transitions WHERE ticket_key = 'PROJ-2'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 2,
            "revisiting the same to_status at a new time is a distinct row"
        );
    }

    #[test]
    fn upsert_comment_then_query_roundtrips() {
        let db = Database::open_in_memory().expect("open");
        let row = sample_comment("PROJ-1", "1001");
        upsert_comment_detail(db.connection(), &row).expect("upsert");

        let (count, body_len): (i64, i64) = db
            .connection()
            .query_row(
                "SELECT COUNT(*), body_len FROM fact_jira_comment_detail WHERE ticket_key = ?1",
                params!["PROJ-1"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query");
        assert_eq!(count, 1);
        assert_eq!(body_len, 42);
    }

    #[test]
    fn upsert_comment_is_idempotent_on_grain_key() {
        let db = Database::open_in_memory().expect("open");
        let row = sample_comment("PROJ-1", "1001");
        upsert_comment_detail(db.connection(), &row).expect("first");
        let mut updated = row.clone();
        updated.body_len = 100;
        upsert_comment_detail(db.connection(), &updated).expect("second");

        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_jira_comment_detail WHERE ticket_key = 'PROJ-1'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 1,
            "re-upserting the same comment id must not duplicate"
        );

        let body_len: i64 = db
            .connection()
            .query_row(
                "SELECT body_len FROM fact_jira_comment_detail WHERE ticket_key = 'PROJ-1'",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(body_len, 100, "re-upsert must refresh the row's fields");
    }

    #[test]
    fn cursor_round_trips_and_is_absent_initially() {
        let db = Database::open_in_memory().expect("open");
        assert!(get_cursor(db.connection(), "PROJ")
            .expect("query")
            .is_none());

        set_cursor(db.connection(), "PROJ", "2026-01-01T00:00:00Z", 42).expect("set");
        let cursor = get_cursor(db.connection(), "PROJ")
            .expect("query")
            .expect("present");
        assert_eq!(cursor.last_synced_at, "2026-01-01T00:00:00Z");
        assert_eq!(cursor.tickets_synced, 42);

        // Overwriting must replace, not duplicate.
        set_cursor(db.connection(), "PROJ", "2026-02-01T00:00:00Z", 7).expect("overwrite");
        let cursor2 = get_cursor(db.connection(), "PROJ")
            .expect("query")
            .expect("present");
        assert_eq!(cursor2.last_synced_at, "2026-02-01T00:00:00Z");
        assert_eq!(cursor2.tickets_synced, 7);
    }

    #[test]
    fn cursor_is_scoped_per_project() {
        let db = Database::open_in_memory().expect("open");
        set_cursor(db.connection(), "PROJ", "2026-01-01T00:00:00Z", 1).expect("set proj");
        set_cursor(db.connection(), "OTHER", "2026-03-01T00:00:00Z", 2).expect("set other");

        assert_eq!(
            get_cursor(db.connection(), "PROJ")
                .expect("query")
                .unwrap()
                .last_synced_at,
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            get_cursor(db.connection(), "OTHER")
                .expect("query")
                .unwrap()
                .last_synced_at,
            "2026-03-01T00:00:00Z"
        );
    }

    #[test]
    fn freshness_reports_stale_when_tables_are_empty() {
        let db = Database::open_in_memory().expect("open");
        let statuses = check_freshness(db.connection(), 2, None).expect("check");
        assert_eq!(statuses.len(), 2);
        for s in &statuses {
            assert_eq!(s.row_count, 0);
            assert!(s.max_synced_at.is_none());
            assert!(s.stale, "{} must be stale when empty", s.table);
        }
    }

    #[test]
    fn freshness_reports_fresh_for_recently_synced_row() {
        let db = Database::open_in_memory().expect("open");
        upsert_ticket_transition(
            db.connection(),
            &sample_transition("PROJ-1", "Done", "2026-01-01T00:00:00Z"),
        )
        .expect("upsert transition");
        upsert_comment_detail(db.connection(), &sample_comment("PROJ-1", "1"))
            .expect("upsert comment");

        let statuses = check_freshness(db.connection(), 2, None).expect("check");
        for s in &statuses {
            assert_eq!(s.row_count, 1);
            assert!(s.max_synced_at.is_some());
            assert!(!s.stale, "{} must be fresh right after a write", s.table);
        }
    }

    #[test]
    fn freshness_reports_stale_when_synced_at_is_older_than_threshold() {
        let db = Database::open_in_memory().expect("open");
        upsert_ticket_transition(
            db.connection(),
            &sample_transition("PROJ-1", "Done", "2026-01-01T00:00:00Z"),
        )
        .expect("upsert");

        // Backdate synced_at to 10 days ago, simulating a sync that stopped
        // running (the exact failure mode the assertion exists to catch).
        let ten_days_ago = chrono::Utc::now().timestamp() - 10 * 86_400;
        db.connection()
            .execute(
                "UPDATE fact_ticket_transitions SET synced_at = ?1 WHERE ticket_key = 'PROJ-1'",
                params![ten_days_ago],
            )
            .expect("backdate");

        let statuses = check_freshness(db.connection(), 2, None).expect("check");
        let transitions = statuses
            .iter()
            .find(|s| s.table == "fact_ticket_transitions")
            .expect("present");
        assert!(
            transitions.stale,
            "a row synced 10 days ago must be stale against a 2-day threshold"
        );
        assert!(transitions.age_seconds.unwrap() >= 10 * 86_400);
    }

    /// The HIGH finding from PR #4067 review: one project's ongoing writes
    /// must not mask another project's dead sync. Unscoped, the table-wide
    /// aggregate reports OK; scoped, the dead project reports STALE.
    #[test]
    fn freshness_is_project_scoped() {
        let db = Database::open_in_memory().expect("open");

        // Project A: written now (healthy).
        let mut healthy = sample_transition("A-1", "Done", "2026-01-01T00:00:00Z");
        healthy.project_key = "A".into();
        upsert_ticket_transition(db.connection(), &healthy).expect("upsert A");
        let mut healthy_comment = sample_comment("A-1", "1");
        healthy_comment.project_key = "A".into();
        upsert_comment_detail(db.connection(), &healthy_comment).expect("upsert A comment");

        // Project B: written 10 days ago (its sync stopped running).
        let mut dead = sample_transition("B-1", "Done", "2026-01-01T00:00:00Z");
        dead.project_key = "B".into();
        upsert_ticket_transition(db.connection(), &dead).expect("upsert B");
        let ten_days_ago = chrono::Utc::now().timestamp() - 10 * 86_400;
        db.connection()
            .execute(
                "UPDATE fact_ticket_transitions SET synced_at = ?1 WHERE project_key = 'B'",
                params![ten_days_ago],
            )
            .expect("backdate B");

        let unscoped = check_freshness(db.connection(), 2, None).expect("unscoped");
        let unscoped_transitions = unscoped
            .iter()
            .find(|s| s.table == "fact_ticket_transitions")
            .expect("present");
        assert!(
            !unscoped_transitions.stale,
            "the table-wide aggregate is exactly the blind spot: A's write hides B"
        );

        let scoped = check_freshness(db.connection(), 2, Some("B")).expect("scoped");
        let scoped_transitions = scoped
            .iter()
            .find(|s| s.table == "fact_ticket_transitions")
            .expect("present");
        assert!(
            scoped_transitions.stale,
            "project B's dead sync must surface once the check is scoped to it"
        );
        assert_eq!(scoped_transitions.project.as_deref(), Some("B"));
    }

    #[test]
    fn list_cursor_projects_returns_every_synced_project() {
        let db = Database::open_in_memory().expect("open");
        assert!(list_cursor_projects(db.connection())
            .expect("query")
            .is_empty());

        set_cursor(db.connection(), "B", "2026-01-01T00:00:00Z", 1).expect("set B");
        set_cursor(db.connection(), "A", "2026-01-01T00:00:00Z", 1).expect("set A");
        assert_eq!(
            list_cursor_projects(db.connection()).expect("query"),
            vec!["A".to_string(), "B".to_string()]
        );
    }
}
