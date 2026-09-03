//! Persistence for `fact_pm_effort` — the PM effort tier (issue #3915).
//! Schema: `sql/0027_fact_pm_effort.sql`.
//!
//! Why: the scorer in [`crate::core::pm_effort`] is a pure function; this
//! module is the only thing that reads its inputs out of the database and
//! writes its scores back, so the scorer stays unit-testable on fixtures
//! with no connection. Same split as [`super::pm_work`].
//!
//! What: [`load_effort_candidates`] reads the meaningful tickets and the
//! three count inputs that live in other tables; [`upsert_pm_effort`] writes
//! one score; [`prune_non_meaningful_effort`] removes rows whose ticket is
//! no longer meaningful.
//!
//! Scale separation: `effort_score` here is counted in PM complexity points
//! and `fact_commit_effort.effort_score` in commit effort points. The two
//! must never share a visualization axis. See #3917.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use tracing::debug;

use crate::core::errors::{Result, TgaError};
use crate::core::pm_effort::{self, EffortBucket, EffortCounts, PmEffortScore, ScoreStatus};

/// The `work_items.source` tag whose tickets `fact_ticket_transitions` and
/// `fact_jira_comment_detail` hold.
///
/// Why: neither table has a source column, and their only production writer
/// is the JIRA sync (#3966). A ticket key is unique per PM source, not
/// globally, so matching a candidate on the bare key would let a JIRA
/// ticket's comments inflate the effort score of an unrelated ticket that
/// happens to share its key in another source — the same defect
/// [`super::pm_work`] fixed for the transition signal.
/// What: the literal `work_items.source` tag JIRA rows carry
/// (`sql/0005_work_items.sql`).
/// Test: `jira_comments_and_transitions_do_not_leak_across_pm_sources`.
const TRANSITIONS_SOURCE: &str = "jira";

/// One meaningful `work_items` row plus every count input that lives
/// outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmEffortCandidate {
    /// `work_items.id`.
    pub id: String,
    /// `work_items.source`: `"azdo"`, `"jira"`, `"github"`, or `"linear"`.
    pub source: String,
    /// `work_items.item_type`, which decides whether the recency floor
    /// applies.
    pub item_type: String,
    /// `work_items.raw_json`, the preserved upstream payload.
    pub raw_json: Option<String>,
    /// `work_items` rows in the same source naming this ticket as parent.
    pub epic_children_count: u32,
    /// `fact_jira_comment_detail` rows for this ticket.
    pub comment_count: u32,
    /// `fact_ticket_transitions` rows for this ticket.
    pub transition_count: u32,
}

/// One row of `fact_pm_effort`.
#[derive(Debug, Clone, PartialEq)]
pub struct PmEffortRow {
    /// `work_items.id` of the scored ticket.
    pub work_item_id: String,
    /// `work_items.source` of the scored ticket.
    pub work_item_source: String,
    /// Reporter display name, when the upstream payload carried one.
    pub pm_name: Option<String>,
    /// ISO week the ticket was created, `YYYY-Www`; `None` when unknown.
    pub week_key: Option<String>,
    /// Ticket age in whole days at scoring time; `None` when the payload
    /// carried no parseable creation timestamp.
    pub age_days: Option<i64>,
    /// The measured inputs, stored alongside the score so a retune can be
    /// evaluated without re-reading upstream payloads.
    pub counts: EffortCounts,
    /// The scorer verdict.
    pub score: PmEffortScore,
}

/// Read every meaningful ticket, with its three external count inputs.
///
/// Why: the meaningfulness gate is issue #3915's first exclusion — a ticket
/// `fact_pm_work` excluded must get no effort row at all, or the effort tier
/// re-admits exactly the boilerplate the work tier removed. Enforcing it in
/// the JOIN rather than in the scorer means there is no code path that can
/// score an excluded ticket.
/// What: an inner join from `work_items` to `fact_pm_work` on
/// `is_meaningful = 1`, plus three lookups — child counts derived from every
/// ticket's parent key (including non-meaningful children, which still
/// represent decomposition), and comment/transition counts from the JIRA
/// fact tables, both scoped to [`TRANSITIONS_SOURCE`].
/// Test: `an_excluded_ticket_gets_no_effort_row`,
/// `epic_children_are_counted_from_parent_keys`,
/// `jira_comments_and_transitions_do_not_leak_across_pm_sources`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if any of the four queries fails.
pub fn load_effort_candidates(conn: &Connection) -> Result<Vec<PmEffortCandidate>> {
    let children = child_counts_by_parent(conn)?;
    let comments = counts_by_ticket(conn, "fact_jira_comment_detail")?;
    let transitions = counts_by_ticket(conn, "fact_ticket_transitions")?;

    let mut stmt = conn
        .prepare(
            "SELECT w.id, w.source, w.item_type, w.raw_json \
             FROM work_items w \
             JOIN fact_pm_work f \
               ON f.work_item_id = w.id AND f.work_item_source = w.source \
             WHERE f.is_meaningful = 1 \
             ORDER BY w.source, w.id",
        )
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
        let (id, source, item_type, raw_json) = row.map_err(TgaError::from)?;
        // #3915: scope every cross-table lookup by source — ticket keys are
        // unique per PM source, not globally.
        let jira = source == TRANSITIONS_SOURCE;
        out.push(PmEffortCandidate {
            epic_children_count: children
                .get(&(source.clone(), id.clone()))
                .copied()
                .unwrap_or(0),
            comment_count: if jira {
                comments.get(&id).copied().unwrap_or(0)
            } else {
                0
            },
            transition_count: if jira {
                transitions.get(&id).copied().unwrap_or(0)
            } else {
                0
            },
            id,
            source,
            item_type,
            raw_json,
        });
    }
    Ok(out)
}

/// `(source, parent_key) -> child count` over every `work_items` row.
///
/// Children are counted regardless of their own meaningfulness verdict: a
/// terse sub-task is still evidence that the parent was decomposed, which is
/// what the child term measures.
fn child_counts_by_parent(conn: &Connection) -> Result<HashMap<(String, String), u32>> {
    let mut stmt = conn
        .prepare("SELECT source, raw_json FROM work_items WHERE raw_json IS NOT NULL")
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(TgaError::from)?;

    let mut out: HashMap<(String, String), u32> = HashMap::new();
    for row in rows {
        let (source, raw_json) = row.map_err(TgaError::from)?;
        if let Some(parent) = pm_effort::extract::extract_fields(Some(&raw_json)).parent_key {
            *out.entry((source, parent)).or_insert(0) += 1;
        }
    }
    Ok(out)
}

/// `ticket_key -> row count` for one of the JIRA fact tables.
///
/// `table` is a compile-time literal chosen by the caller, never user input,
/// so interpolating it carries no injection risk; `rusqlite` cannot bind a
/// table name as a parameter.
fn counts_by_ticket(conn: &Connection, table: &'static str) -> Result<HashMap<String, u32>> {
    let sql = format!("SELECT ticket_key, COUNT(*) FROM {table} GROUP BY ticket_key");
    let mut stmt = conn.prepare(&sql).map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(TgaError::from)?;

    let mut out = HashMap::new();
    for row in rows {
        let (key, count) = row.map_err(TgaError::from)?;
        out.insert(key, u32::try_from(count).unwrap_or(u32::MAX));
    }
    Ok(out)
}

/// Upsert one `fact_pm_effort` row.
///
/// Idempotent: `INSERT OR REPLACE` against the
/// `(work_item_id, work_item_source)` primary key, so re-running the scorer
/// over an unchanged corpus rewrites the same rows and adds none.
/// `formula_version` is written from
/// [`crate::core::pm_effort::FORMULA_VERSION`] and is deliberately NOT part
/// of the key — the table holds the current score per ticket, and the
/// version column records which weight set produced it.
///
/// A deferred ticket stores NULL in both `effort_score` and `effort_bucket`,
/// never a zero: see the `score_status` column's rationale in the migration.
///
/// Test: `backfill_pm_effort_is_idempotent`,
/// `a_recent_epic_is_recorded_as_deferred_not_scored_zero`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the SQL execution fails. A foreign-key
/// violation surfaces here when the referenced `work_items` row is absent.
pub fn upsert_pm_effort(conn: &Connection, row: &PmEffortRow) -> Result<()> {
    let computed_at = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT OR REPLACE INTO fact_pm_effort \
         (work_item_id, work_item_source, pm_name, week_key, effort_score, effort_bucket, \
          score_status, epic_children_count, description_word_count, comment_count, \
          transition_count, story_points, inputs_present, age_days_at_score, \
          formula_version, computed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            row.work_item_id,
            row.work_item_source,
            row.pm_name,
            row.week_key,
            row.score.effort_score,
            row.score.effort_bucket.map(EffortBucket::as_wire_str),
            row.score.status.as_wire_str(),
            row.counts.epic_children,
            row.counts.description_words,
            row.counts.comments,
            row.counts.transitions,
            row.counts.story_points,
            row.score.inputs_present.to_wire_string(),
            row.age_days,
            pm_effort::FORMULA_VERSION,
            computed_at,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(
        id = %row.work_item_id,
        source = %row.work_item_source,
        status = %row.score.status,
        score = ?row.score.effort_score,
        "upserted pm effort score"
    );
    Ok(())
}

/// Delete `fact_pm_effort` rows whose ticket is no longer meaningful.
///
/// Why: the meaningfulness gate is only durable if it is re-applied. Without
/// this, a ticket that `tga backfill pm-work` reclassified as boilerplate —
/// after a retune, or after its payload was re-collected — would keep the
/// effort row an earlier run wrote, and the excluded ticket would go on
/// contributing to every dashboard that reads this table.
/// What: one `DELETE` for every row with no matching
/// `fact_pm_work` row marked meaningful. Returns the number deleted.
/// Test: `a_ticket_that_becomes_non_meaningful_loses_its_effort_row`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the delete fails.
pub fn prune_non_meaningful_effort(conn: &Connection) -> Result<usize> {
    let deleted = conn
        .execute(
            "DELETE FROM fact_pm_effort WHERE NOT EXISTS ( \
               SELECT 1 FROM fact_pm_work f \
               WHERE f.work_item_id = fact_pm_effort.work_item_id \
                 AND f.work_item_source = fact_pm_effort.work_item_source \
                 AND f.is_meaningful = 1)",
            [],
        )
        .map_err(TgaError::from)?;
    Ok(deleted)
}

/// Count `fact_pm_effort` rows per bucket, plus the deferred total.
///
/// Returns `(deferred_count, [(bucket, count), ...])` with the buckets in
/// LOW / MEDIUM / HIGH order. A row whose stored bucket or status this
/// binary does not recognise — written by a newer `formula_version` — is
/// skipped rather than miscounted.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn summarize_effort(conn: &Connection) -> Result<(i64, Vec<(EffortBucket, i64)>)> {
    let mut stmt = conn
        .prepare(
            "SELECT score_status, effort_bucket, COUNT(*) FROM fact_pm_effort \
             GROUP BY score_status, effort_bucket",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(TgaError::from)?;

    let mut deferred = 0i64;
    let mut by_bucket: HashMap<EffortBucket, i64> = HashMap::new();
    for row in rows {
        let (status, bucket, count) = row.map_err(TgaError::from)?;
        match ScoreStatus::from_wire_str(&status) {
            Some(ScoreStatus::DeferredRecent) => deferred += count,
            Some(ScoreStatus::Scored) => {
                match bucket.as_deref().and_then(EffortBucket::from_wire_str) {
                    Some(b) => *by_bucket.entry(b).or_insert(0) += count,
                    None => debug!(bucket = ?bucket, "skipping unrecognised effort_bucket"),
                }
            }
            None => debug!(status = %status, "skipping unrecognised score_status"),
        }
    }

    let ordered = [EffortBucket::Low, EffortBucket::Medium, EffortBucket::High]
        .into_iter()
        .map(|b| (b, by_bucket.get(&b).copied().unwrap_or(0)))
        .collect();
    Ok((deferred, ordered))
}
