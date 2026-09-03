//! Persistence for `fact_pm_work` — the PM work meaningfulness tier
//! (issue #3916). Schema: `sql/0026_fact_pm_work.sql`.
//!
//! Why: the classifier in [`crate::core::pm_work`] is a pure function; this
//! module is the only thing that reads its inputs out of the database and
//! writes its verdicts back, so the classifier stays unit-testable on
//! fixtures with no connection.
//!
//! What: [`load_candidates`] reads every `work_items` row plus the
//! human-transition signal that separates `BOT_FILED` from `AUTO_GENERATED`;
//! [`upsert_pm_work`] writes one verdict.
//!
//! Scale separation: rows here are counted in tickets, `fact_commit_effort`
//! rows in commit effort points, and the two must never share a
//! visualization axis. See #3917.

use std::collections::HashSet;

use rusqlite::{params, Connection};
use tracing::debug;

use crate::core::errors::{Result, TgaError};
use crate::core::pm_work::{self, ExclusionReason, PmWorkVerdict};

/// The `work_items.source` tag whose tickets `fact_ticket_transitions` holds.
///
/// Why: that table has no source column, and its only production writer is
/// the JIRA sync — `upsert_ticket_transition` in `core::db::jira_facts`,
/// called from `commands::jira` (#3966). A ticket key is unique per PM
/// source, not globally, so matching a candidate on the bare key let a JIRA
/// ticket's human transition decide the verdict for an unrelated ticket that
/// happens to share its key in another source.
/// What: the literal `work_items.source` tag JIRA rows carry
/// (`sql/0005_work_items.sql`). Spelled here rather than imported from
/// `collect::pm_adapter::PmSource` because `core` is the layer `collect`
/// depends on, not the reverse. Should a second provider ever write
/// transitions, that table needs its own source column and this constant
/// goes away.
/// Test: `a_human_transition_does_not_leak_across_pm_sources`.
const TRANSITIONS_SOURCE: &str = "jira";

/// One `work_items` row plus the transition signal the classifier needs.
///
/// `raw_json` is carried verbatim rather than pre-parsed so the caller can
/// run [`crate::core::pm_work::extract::extract_fields`] once and keep both
/// the extracted fields and their derived week key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmWorkCandidate {
    /// `work_items.id`.
    pub id: String,
    /// `work_items.source`: `"azdo"`, `"jira"`, `"github"`, or `"linear"`.
    pub source: String,
    /// `work_items.title`.
    pub title: String,
    /// `work_items.raw_json`, the preserved upstream payload.
    pub raw_json: Option<String>,
    /// Whether some `fact_ticket_transitions` row for this ticket names a
    /// non-bot author.
    pub human_transitioned: bool,
}

/// One row of `fact_pm_work`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmWorkRow {
    /// `work_items.id` of the classified ticket.
    pub work_item_id: String,
    /// `work_items.source` of the classified ticket.
    pub work_item_source: String,
    /// Reporter display name, when the upstream payload carried one.
    pub pm_name: Option<String>,
    /// ISO week the ticket was created, `YYYY-Www`. `None` when the payload
    /// carried no parseable creation timestamp.
    pub week_key: Option<String>,
    /// The classifier verdict.
    pub verdict: PmWorkVerdict,
}

/// Read every classifiable ticket, with its human-transition signal.
///
/// Why: the `BOT_FILED` / `AUTO_GENERATED` split turns on whether a person
/// ever moved a bot-filed ticket, which lives in `fact_ticket_transitions`
/// (migration v23) rather than on the ticket itself.
/// What: one pass over `work_items` and one over the distinct transition
/// authors, joined in memory on ticket key — but only for candidates whose
/// source is [`TRANSITIONS_SOURCE`], since keys collide across sources. The
/// author-side bot test is [`crate::core::pm_work::is_bot_account`], so a
/// transition made by another bot does not count as human contact.
/// Test: `pm_work_backfill_marks_a_bot_filed_ticket_when_a_human_moved_it`,
/// `a_human_transition_does_not_leak_across_pm_sources`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if either query fails.
pub fn load_candidates(conn: &Connection) -> Result<Vec<PmWorkCandidate>> {
    let human_moved = tickets_with_a_human_transition(conn)?;

    let mut stmt = conn
        .prepare("SELECT id, source, title, raw_json FROM work_items ORDER BY source, id")
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(TgaError::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (id, source, title, raw_json) = row.map_err(TgaError::from)?;
        // #3916: scope the lookup by source — a bare ticket-key match let a
        // JIRA ticket's human transition decide a same-keyed Linear ticket.
        let human_transitioned = source == TRANSITIONS_SOURCE && human_moved.contains(&id);
        out.push(PmWorkCandidate {
            id,
            source,
            title,
            raw_json,
            human_transitioned,
        });
    }
    Ok(out)
}

/// Ticket keys that a non-bot author transitioned at least once.
fn tickets_with_a_human_transition(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT ticket_key, author FROM fact_ticket_transitions \
             WHERE author IS NOT NULL",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(TgaError::from)?;

    let mut out = HashSet::new();
    for row in rows {
        let (ticket_key, author) = row.map_err(TgaError::from)?;
        if !pm_work::is_bot_account(&author) {
            out.insert(ticket_key);
        }
    }
    Ok(out)
}

/// Upsert one `fact_pm_work` row.
///
/// Idempotent: `INSERT OR REPLACE` against the
/// `(work_item_id, work_item_source)` primary key, so re-running the
/// classifier over an unchanged corpus rewrites the same rows and adds none.
/// `formula_version` is written from
/// [`crate::core::pm_work::FORMULA_VERSION`] and is deliberately NOT part of
/// the key — the table holds the current verdict per ticket, and the version
/// column records which threshold set produced it.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails. A
/// foreign-key violation surfaces here when the referenced `work_items` row
/// does not exist.
pub fn upsert_pm_work(conn: &Connection, row: &PmWorkRow) -> Result<()> {
    let computed_at = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT OR REPLACE INTO fact_pm_work \
         (work_item_id, work_item_source, pm_name, week_key, is_meaningful, \
          exclusion_reason, title_word_count, body_word_count, formula_version, computed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.work_item_id,
            row.work_item_source,
            row.pm_name,
            row.week_key,
            i64::from(row.verdict.is_meaningful),
            row.verdict.exclusion_reason.as_wire_str(),
            row.verdict.title_word_count as i64,
            row.verdict.body_word_count as i64,
            pm_work::FORMULA_VERSION,
            computed_at,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(
        id = %row.work_item_id,
        source = %row.work_item_source,
        meaningful = row.verdict.is_meaningful,
        reason = %row.verdict.exclusion_reason,
        "upserted pm work verdict"
    );
    Ok(())
}

/// Count `fact_pm_work` rows per exclusion reason, for run summaries.
///
/// Returns `(meaningful_count, [(reason, count), ...])` with the excluded
/// reasons in a stable order. An unrecognised stored reason — a row written
/// by a newer `formula_version` — is skipped rather than miscounted as
/// `NONE`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn summarize(conn: &Connection) -> Result<(i64, Vec<(ExclusionReason, i64)>)> {
    let mut stmt = conn
        .prepare("SELECT exclusion_reason, COUNT(*) FROM fact_pm_work GROUP BY exclusion_reason")
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(TgaError::from)?;

    let mut meaningful = 0;
    let mut excluded: Vec<(ExclusionReason, i64)> = Vec::new();
    for row in rows {
        let (reason, count) = row.map_err(TgaError::from)?;
        match ExclusionReason::from_wire_str(&reason) {
            Some(ExclusionReason::None) => meaningful += count,
            Some(other) => excluded.push((other, count)),
            None => debug!(reason = %reason, "skipping unrecognised exclusion_reason"),
        }
    }
    excluded.sort_by_key(|(reason, _)| reason.as_wire_str());
    Ok((meaningful, excluded))
}
