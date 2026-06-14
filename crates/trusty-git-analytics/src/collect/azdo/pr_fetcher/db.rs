//! Database helpers for ADO pull-request persistence.
//!
//! Why: isolates the SQLite read/write logic from the HTTP-fetching layer so
//! each concern can be tested and evolved independently.
//! What: `extract_pr_ids` parses commit messages; `get_existing_pr_numbers`,
//! `upsert_pr`, and `upsert_pr_reviewer` manage the `pull_requests` and
//! `pr_reviewers` tables.
//! Test: `pr_fetcher_tests.rs` covers all four functions.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{params, Connection};

use crate::collect::azdo::pr_fetcher::types::{AdoPrReviewer, AdoPullRequest};
use crate::core::errors::{Result as CoreResult, TgaError};

/// Regex matching the standard ADO merge-commit subject line.
///
/// ADO emits `Merged PR 1234: <title>` when a PR is completed via squash or
/// merge. The match is case-insensitive to tolerate hand-typed references.
fn merged_pr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)Merged PR (\d+):").expect("MERGED_PR_RE is a static valid pattern")
    })
}

/// Extract the set of unique ADO PR IDs referenced by a stream of commit
/// messages.
///
/// Why: ADO's standard merge-commit subject is `Merged PR 1234: <title>`, so
/// the union of commit-message matches gives the full list of PRs that
/// touched the analyzed history without needing a paginated repo-wide PR
/// query.
/// What: returns sorted unique IDs; messages with no match are ignored.
/// Test: covered by `extracts_unique_pr_ids` and `ignores_non_merge_lines`.
pub fn extract_pr_ids<I, S>(messages: I) -> Vec<i64>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen: HashSet<i64> = HashSet::new();
    let re = merged_pr_re();
    for msg in messages {
        for cap in re.captures_iter(msg.as_ref()) {
            if let Some(m) = cap.get(1) {
                if let Ok(id) = m.as_str().parse::<i64>() {
                    seen.insert(id);
                }
            }
        }
    }
    let mut out: Vec<i64> = seen.into_iter().collect();
    out.sort_unstable();
    out
}

/// Return the set of `pr_number`s already persisted for the given
/// `(provider, repository)` scope, so callers can skip work already on disk.
///
/// `repository` is the per-provider repository identifier as written by
/// [`upsert_pr`] (for Azure DevOps this is the project name); see migration
/// `0012_pull_requests_repository.sql`. Scoping to a single repository
/// matches the UNIQUE constraint and prevents one project's IDs from
/// masking another's.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn get_existing_pr_numbers(
    conn: &Connection,
    provider: &str,
    repository: &str,
) -> CoreResult<HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT pr_number FROM pull_requests WHERE provider = ?1 AND repository = ?2")?;
    let rows = stmt
        .query_map(params![provider, repository], |row| row.get::<_, i64>(0))
        .map_err(TgaError::from)?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// Upsert an [`AdoPullRequest`] into `pull_requests` (provider = 'azdo')
/// and return the row id (existing or newly inserted).
///
/// Why: ADO PRs reuse the shared `pull_requests` table; the
/// `(provider, repository, pr_number)` triple scopes uniqueness so neither
/// cross-provider IDs nor cross-project IDs collide. We need the row id
/// back to attach reviewers via FK.
/// What: `INSERT OR REPLACE` keyed by `(provider, repository, pr_number)`
/// per migration `0012_pull_requests_repository.sql`, then a `SELECT id`
/// to recover the row id (REPLACE may renumber on conflict). The
/// `repository` parameter is the ADO project name — Azure DevOps PR IDs
/// are project-scoped, not org-scoped, so the project is the right
/// uniqueness boundary.
/// Test: `upsert_pr_round_trips_basic_fields` exercises insert + re-insert.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn upsert_pr(conn: &Connection, pr: &AdoPullRequest, repository: &str) -> CoreResult<i64> {
    let state = match pr.status.to_ascii_lowercase().as_str() {
        "completed" => "merged",
        "abandoned" => "closed",
        _ => "open",
    };

    let commit_shas = match &pr.merge_commit_sha {
        Some(sha) => serde_json::to_string(&[sha.as_str()])?,
        None => "[]".to_string(),
    };

    conn.execute(
        "INSERT OR REPLACE INTO pull_requests \
         (provider, repository, pr_number, title, author, state, created_at, merged_at, commit_shas) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            "azdo",
            repository,
            pr.pr_number,
            pr.title,
            pr.author,
            state,
            pr.created_at.to_rfc3339(),
            pr.closed_at.map(|t| t.to_rfc3339()),
            commit_shas,
        ],
    )?;

    let id: i64 = conn
        .query_row(
            "SELECT id FROM pull_requests WHERE provider = ?1 AND repository = ?2 AND pr_number = ?3",
            params!["azdo", repository, pr.pr_number],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    Ok(id)
}

/// Upsert a single reviewer row attached to `pr_db_id`.
///
/// Uses `INSERT OR REPLACE` on the unique `(pr_id, provider, reviewer_id)`
/// index so re-running collection refreshes the vote without duplicating rows.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn upsert_pr_reviewer(
    conn: &Connection,
    pr_db_id: i64,
    reviewer: &AdoPrReviewer,
) -> CoreResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pr_reviewers \
         (pr_id, provider, reviewer_id, display_name, vote, is_required, is_container) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            pr_db_id,
            "azdo",
            reviewer.reviewer_id,
            reviewer.display_name,
            reviewer.vote,
            reviewer.is_required as i32,
            reviewer.is_container as i32,
        ],
    )?;
    Ok(())
}
