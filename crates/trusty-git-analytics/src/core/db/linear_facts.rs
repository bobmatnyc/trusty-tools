//! Incremental-sync cursor bookkeeping for `tga linear sync` /
//! `tga linear freshness` (issue #7139).
//!
//! Why: `linear_issues` (migration v2) is written by two independent paths —
//! the per-commit-reference lookup (`linear.fetch_on_reference`) and, since
//! this issue, a bulk team sync — so `MAX(linear_issues.fetched_at)` cannot
//! tell "the bulk sync ran" apart from "a commit happened to mention a
//! ticket". `linear_sync_cursor` is the bulk sync's own bookkeeping table,
//! mirroring `jira_sync_cursor` (`core::db::jira_facts`, migration v23): one
//! row per team key, holding the `updatedAt >=` cursor for the next
//! incremental run and the wall-clock time of the last successful run.
//! What: [`LinearSyncCursor`], [`get_linear_cursor`], [`set_linear_cursor`],
//! and [`list_linear_cursor_teams`] — the same shape as their JIRA
//! counterparts, scoped by `team_key` instead of `project_key`.
//! Test: this module's own `tests`; `commands::linear::tests` covers the
//! command-level freshness reporting built on top of these primitives.

use rusqlite::{params, Connection, OptionalExtension};

use crate::core::errors::{Result, TgaError};

/// Stored incremental-sync cursor for a Linear team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearSyncCursor {
    /// RFC3339 timestamp: the `updatedAt >=` cursor for the next incremental
    /// run.
    pub last_synced_at: String,
    /// RFC3339 timestamp of the last successful sync invocation's wall-clock
    /// completion — the freshness signal `tga linear freshness` reads.
    pub last_run_at: String,
    /// Number of issues processed in the last successful run.
    pub issues_synced: i64,
}

/// Fetch the stored sync cursor for `team_key`, or `None` if this team has
/// never completed a bulk sync.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn get_linear_cursor(conn: &Connection, team_key: &str) -> Result<Option<LinearSyncCursor>> {
    conn.query_row(
        "SELECT last_synced_at, last_run_at, issues_synced \
         FROM linear_sync_cursor WHERE team_key = ?1",
        params![team_key],
        |row| {
            Ok(LinearSyncCursor {
                last_synced_at: row.get(0)?,
                last_run_at: row.get(1)?,
                issues_synced: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(TgaError::from)
}

/// Record (overwrite) the sync cursor for `team_key` after a successful run.
///
/// `last_run_at` is stamped with the current wall-clock time; callers supply
/// only the cursor (`last_synced_at`) and issue count.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn set_linear_cursor(
    conn: &Connection,
    team_key: &str,
    last_synced_at: &str,
    issues_synced: i64,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO linear_sync_cursor \
         (team_key, last_synced_at, last_run_at, issues_synced) \
         VALUES (?1, ?2, ?3, ?4)",
        params![team_key, last_synced_at, now, issues_synced],
    )
    .map_err(TgaError::from)?;
    Ok(())
}

/// Every Linear team key that has ever recorded a sync cursor.
///
/// Mirrors [`crate::core::db::jira_facts::list_cursor_projects`]: the
/// freshness guard defaults to checking every team with recorded state,
/// rather than an aggregate that lets one healthy team mask another team's
/// dead sync.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn list_linear_cursor_teams(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT team_key FROM linear_sync_cursor ORDER BY team_key")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    #[test]
    fn get_cursor_is_none_before_any_sync() {
        let db = Database::open_in_memory().expect("open");
        assert_eq!(
            get_linear_cursor(db.connection(), "ENG").expect("query"),
            None
        );
    }

    #[test]
    fn set_then_get_cursor_roundtrips() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 42).expect("set");
        let cursor = get_linear_cursor(db.connection(), "ENG")
            .expect("query")
            .expect("present");
        assert_eq!(cursor.last_synced_at, "2026-01-01T00:00:00+00:00");
        assert_eq!(cursor.issues_synced, 42);
        assert!(!cursor.last_run_at.is_empty());
    }

    #[test]
    fn set_cursor_overwrites_the_prior_row() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 10).expect("first");
        set_linear_cursor(db.connection(), "ENG", "2026-02-01T00:00:00+00:00", 20).expect("second");
        let cursor = get_linear_cursor(db.connection(), "ENG")
            .expect("query")
            .expect("present");
        assert_eq!(cursor.last_synced_at, "2026-02-01T00:00:00+00:00");
        assert_eq!(cursor.issues_synced, 20);
    }

    #[test]
    fn list_cursor_teams_returns_every_recorded_team_sorted() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "FE", "2026-01-01T00:00:00+00:00", 1).expect("fe");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 1).expect("eng");
        let teams = list_linear_cursor_teams(db.connection()).expect("list");
        assert_eq!(teams, vec!["ENG".to_string(), "FE".to_string()]);
    }
}
