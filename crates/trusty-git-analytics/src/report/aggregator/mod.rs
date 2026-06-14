//! Database aggregation: turn raw rows into [`ReportData`].
//!
//! The aggregator runs a single scan of the `commits` table (left-joined
//! against `classifications`) and groups the results in-memory. For the
//! data sizes typical of `trusty-git-analytics` this is simpler and
//! faster than emitting multiple grouped SQL queries.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use regex::Regex;
use tracing::{debug, warn};

use crate::collect::ai_attribution::AgenticMode;
use crate::core::config::Config;
use crate::core::db::Database;
use crate::report::errors::{ReportError, Result};
use crate::report::models::{ActivityWeights, ReportData, VelocitySummary};

/// Helper that walks the database and assembles [`ReportData`].
///
/// Why: report generation needs a single named entry point so callers (the
/// CLI, integration tests) can share one aggregation path instead of
/// duplicating SQL across formatters.
/// What: namespace type with no fields; all behaviour is on associated
/// functions like [`Aggregator::build`].
/// Test: see `report::tests::aggregator_builds_report_data` for end-to-end
/// coverage from a seeded SQLite DB.
pub struct Aggregator;

/// Internal row pulled from the commit/classification join.
pub(super) struct CommitRow {
    pub(super) sha: String,
    pub(super) author_name: String,
    pub(super) author_email: String,
    pub(super) timestamp: DateTime<Utc>,
    pub(super) repository: String,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
    pub(super) files_changed: i64,
    pub(super) category: Option<String>,
    pub(super) message: String,
    pub(super) ticketed: bool,
    /// True when the commit carries a recognised AI co-authorship trailer
    /// (issue #445: `Co-Authored-By: Claude/Copilot/Cursor`).
    pub(super) is_ai_assisted: bool,
    /// LLM-assigned complexity score 1–5 from `classifications.complexity`
    /// (issue #445 batch B, request #6). `None` for non-LLM classifications
    /// or commits without a classification row.
    pub(super) complexity: Option<i64>,
    /// Canonical agentic mode (issue #1113): full_agentic / ide_assisted / none.
    pub(super) agentic_mode: AgenticMode,
}

/// Minimal PR row used by velocity / DORA computations and (issue #377)
/// abandoned-PR counting.
pub(super) struct PrRow {
    /// PR author login as recorded by the provider (e.g. GitHub login).
    /// Note: this is NOT a canonical engineer email — see
    /// [`build_abandoned_pr_counts`] for the attribution limitation.
    pub(super) author: String,
    /// Provider lifecycle state: `"open"`, `"closed"`, or `"merged"`.
    pub(super) state: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) merged_at: Option<DateTime<Utc>>,
}

/// Default regex patterns identifying machine-generated commits.
///
/// Why: keep boilerplate (lock-file bumps, version bumps, merge commits, …)
/// from skewing per-developer averages. Matched case-insensitively against
/// the first line of each commit message.
pub(super) const DEFAULT_BOILERPLATE_PATTERNS: &[&str] = &[
    r"^[Mm]erge branch",
    r"^[Mm]erge pull request",
    r"^[Bb]ump version",
    r"^[Uu]pdate package-lock",
    r"^[Uu]pdate yarn\.lock",
    r"[Gg]enerated by",
    r"[Aa]uto-generated",
];

/// Boilerplate threshold (avg lines per commit) above which a commit is
/// flagged independently of message-pattern match.
pub(super) const BOILERPLATE_LINES_THRESHOLD: i64 = 500;

/// Heuristic boilerplate detector.
///
/// Why: prevents auto-generated commits (lock-file bumps, version bumps,
/// generated code) from skewing per-developer averages.
/// What: returns `true` when the message matches any boilerplate pattern OR
/// the lines-changed budget exceeds [`BOILERPLATE_LINES_THRESHOLD`].
/// Test: feed a `"Update package-lock.json"` message → `true`; a normal
/// `"feat: x"` message with small diff → `false`.
pub(super) fn is_boilerplate(message: &str, lines_changed: i64, patterns: &[Regex]) -> bool {
    let first_line = message.lines().next().unwrap_or(message);
    if lines_changed > BOILERPLATE_LINES_THRESHOLD {
        // Large diff alone is not enough; require pattern OR very-large diff
        // (10x threshold) to flag as boilerplate.
        if lines_changed > BOILERPLATE_LINES_THRESHOLD * 10 {
            return true;
        }
    }
    patterns.iter().any(|p| p.is_match(first_line))
}

/// Compile a list of pattern strings into [`Regex`] values, logging and
/// skipping any that fail to parse so a bad user-supplied pattern can't
/// brick the entire report run.
pub(super) fn compile_patterns(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| match Regex::new(p) {
            Ok(r) => Some(r),
            Err(e) => {
                warn!(pattern = %p, error = %e, "skipping invalid regex pattern");
                None
            }
        })
        .collect()
}

impl Aggregator {
    /// Build a full [`ReportData`] from the given database.
    ///
    /// Why: report formatters all need the same denormalised view of the
    /// data; this is the one place that knows how to build it.
    /// What: loads rows + PR rows, runs aggregation, then layers
    /// coverage / unresolved-identity diagnostics on top of the result.
    /// Test: see `report::tests::aggregator_builds_report_data` and
    /// `aggregator_computes_summary_and_dora_and_quality`.
    ///
    /// The `config` argument feeds the configured-alias check used to
    /// detect "phantom" identities (authors whose email is not in the
    /// configured alias map) so consumers know whether developer counts
    /// are inflated by unmapped commit-author identities.
    ///
    /// # Errors
    ///
    /// Returns [`crate::report::ReportError::Core`] if the underlying queries fail.
    pub fn build(db: &Database, config: &Config) -> Result<ReportData> {
        Self::build_filtered(db, config, None)
    }

    /// Build a [`ReportData`] optionally scoped to one canonical identity.
    ///
    /// Why: `tga report --author <email>` lets users drill into a single
    /// engineer's contribution without generating a full team report.
    /// What: when `author_email` is `Some`, validates that the email exists
    /// in the `authors` table (case-insensitive) before filtering the commit
    /// rows to that identity; when `None`, behaves identically to
    /// [`Self::build`].
    /// Test: see `report::tests::aggregator_author_filter_returns_single_author`
    /// and `aggregator_author_filter_unknown_email_errors`.
    ///
    /// # Errors
    ///
    /// - Returns [`ReportError::Report`] (exit-non-zero) when `author_email`
    ///   is provided but does not match any `canonical_email` in the
    ///   `authors` table.
    /// - Returns [`crate::report::ReportError::Core`] if underlying queries fail.
    pub fn build_filtered(
        db: &Database,
        config: &Config,
        author_email: Option<&str>,
    ) -> Result<ReportData> {
        // Validate and canonicalize the author filter before loading rows.
        let canonical_email: Option<String> = if let Some(email) = author_email {
            let resolved = Self::resolve_canonical_email(db, email)?;
            Some(resolved)
        } else {
            None
        };

        let rows = Self::load_rows_filtered(db, canonical_email.as_deref())?;
        let prs = Self::load_prs(db).unwrap_or_default();
        let unresolved_db = if canonical_email.is_none() {
            Self::count_unresolved_author_commits(db).unwrap_or(0)
        } else {
            // When scoped to one author, the "unresolved" count is not
            // meaningful for the per-author view — suppress it.
            0
        };
        let mut data = Self::aggregate(rows, prs);

        // Issue #68 / #67: surface coverage and unresolved-identity counts
        // so consumers know the scope of the report. `repository_coverage`
        // counts distinct repositories observed in the data (not the size
        // of the configured roster, so that a misconfigured `repositories[]`
        // entry that produced no commits is not double-counted).
        data.repository_coverage = data.repositories.len();

        // Aggregate the configured-alias set so we can flag author summaries
        // whose canonical email is not part of any configured identity. These
        // are "phantom" identities that inflate distinct-developer counts.
        let alias_set = configured_alias_emails(config);
        let unresolved_authors = if alias_set.is_empty() {
            // Without a configured alias map there is no signal — every
            // author is "unresolved" in that sense, which would be noise.
            // Surface zero so downstream consumers don't double-count.
            0
        } else {
            data.authors
                .iter()
                .filter(|a| !alias_set.contains(&a.email.to_lowercase()))
                .count()
        };
        data.unresolved_authors = unresolved_authors;
        data.unresolved_author_commits = unresolved_db;

        // Issue #69: warn when adjacent weeks have different repository
        // coverage in `collection_runs`. This detects baseline drift that
        // would otherwise silently break week-over-week deltas.
        check_weekly_coverage_drift(db, &data.weekly_metrics);

        // Issue #445 batch B: persist per-engineer-per-week quality scores to
        // `fact_weekly_quality` so downstream warehouses can query without
        // re-running the aggregator. Non-fatal: a persistence failure is
        // logged but does not abort report generation (the in-memory data is
        // still complete and formatters will still write their files).
        match Self::persist_weekly_quality(db, &data) {
            Ok(n) => {
                tracing::debug!(
                    rows = n,
                    "persisted weekly quality rows to fact_weekly_quality"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "WARNING: could not persist to fact_weekly_quality; \
                     run `tga backfill quality` to retry. Report generation continues."
                );
            }
        }

        // Issue #1113: persist per-engineer-per-week agentic counts to
        // `fact_weekly_engineer`. Same non-fatal pattern as quality above.
        match Self::persist_weekly_engineer(db, &data) {
            Ok(n) => {
                tracing::debug!(
                    rows = n,
                    "persisted weekly engineer rows to fact_weekly_engineer"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "WARNING: could not persist to fact_weekly_engineer; \
                     report generation continues."
                );
            }
        }

        if unresolved_db > 0 {
            tracing::warn!(
                count = unresolved_db,
                "WARNING: {unresolved_db} commits have unresolved author identities and may \
                 inflate developer counts. Run `tga aliases list` to review, or extend \
                 `developer_aliases` in the config to map missing identities."
            );
        }
        Ok(data)
    }

    /// Count commits where `author_id IS NULL` — the canonical "unresolved"
    /// signal. This is distinct from `unresolved_authors` (configured-alias
    /// membership): an `author_id IS NULL` commit means identity resolution
    /// never ran for it, so it is silently treated as its own developer.
    fn count_unresolved_author_commits(db: &Database) -> Result<usize> {
        let conn = db.connection();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE author_id IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(crate::core::TgaError::from)?;
        Ok(n as usize)
    }

    /// Load PR rows for velocity / DORA computations.
    ///
    /// Why: lead-time, cycle-time, and deployment frequency depend on
    /// merged-PR timing; issue #377 additionally needs `author` and `state`
    /// to count closed-but-unmerged ("abandoned") PRs per engineer.
    /// What: returns the subset of `pull_requests` with a parseable
    /// `created_at`; rows with an un-parseable created timestamp are silently
    /// dropped (they cannot be week-bucketed).
    /// Test: insert a row with valid `created_at`/`merged_at`, assert vector
    /// length 1 with matching timestamps; abandoned-PR counting is covered by
    /// `aggregator_counts_abandoned_prs`.
    fn load_prs(db: &Database) -> Result<Vec<PrRow>> {
        let conn = db.connection();
        let mut stmt = conn
            .prepare("SELECT created_at, merged_at, author, state FROM pull_requests")
            .map_err(crate::core::TgaError::from)?;
        let rows = stmt
            .query_map([], |row| {
                let created: String = row.get(0)?;
                let merged: Option<String> = row.get(1)?;
                let author: String = row.get(2)?;
                let state: String = row.get(3)?;
                Ok((created, merged, author, state))
            })
            .map_err(crate::core::TgaError::from)?;
        let mut out = Vec::new();
        for r in rows {
            let (created_s, merged_s, author, state) = r.map_err(crate::core::TgaError::from)?;
            let created_at = match DateTime::parse_from_rfc3339(&created_s) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(_) => continue,
            };
            let merged_at = merged_s
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            out.push(PrRow {
                author,
                state,
                created_at,
                merged_at,
            });
        }
        Ok(out)
    }

    /// Resolve an author email filter to the stored `canonical_email`.
    ///
    /// Why: `canonical_email` values in the DB may differ in case from what
    /// the user typed; resolving once up-front ensures the SQL `WHERE` clause
    /// uses the exact stored value and produces consistent results across
    /// collation settings.
    /// What: queries `authors` with a case-insensitive match on
    /// `LOWER(canonical_email)`; returns the stored value on success, or a
    /// helpful `ReportError::Report` that names the `tga aliases list`
    /// remedy when no match exists.
    /// Test: see `report::tests::aggregator_author_filter_unknown_email_errors`.
    fn resolve_canonical_email(db: &Database, email: &str) -> Result<String> {
        let conn = db.connection();
        let lower = email.to_lowercase();
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT canonical_email FROM authors WHERE LOWER(canonical_email) = LOWER(?1) LIMIT 1",
            rusqlite::params![lower],
            |row| row.get(0),
        );
        match result {
            Ok(stored) => Ok(stored),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(ReportError::Report(format!(
                "no canonical identity with canonical_email '{email}' found in authors table.\n\
                 Run `tga aliases list` to see all canonical identities, or \
                 `tga aliases merge` to consolidate duplicate identities."
            ))),
            Err(e) => Err(ReportError::Core(crate::core::TgaError::from(e))),
        }
    }

    /// Load commit rows, optionally filtered to a single canonical email.
    ///
    /// Why: separating row loading from the `build_filtered` orchestration
    /// keeps the SQL in one place and makes the filter opt-in without
    /// duplicating the large query string.
    /// What: when `author_email` is `Some`, appends
    /// `WHERE LOWER(a.canonical_email) = LOWER(?)` to the base JOIN query;
    /// when `None`, returns all rows.
    /// Test: covered by `aggregator_author_filter_returns_single_author`
    /// (filters to alice's rows) and `aggregator_builds_report_data`
    /// (no filter, returns all rows).
    fn load_rows_filtered(db: &Database, author_email: Option<&str>) -> Result<Vec<CommitRow>> {
        let conn = db.connection();
        // Prefer the canonical identity from the `authors` table when the
        // commit has been linked (i.e. `author_id IS NOT NULL`). This ensures
        // that aliases configured in `developer_aliases` are honored at
        // aggregation time: every commit by the same person — regardless of
        // the raw name/email recorded in git — collapses to one canonical
        // `(name, email)` pair in reports.
        //
        // Falls back to the raw commit fields when no `author_id` is set
        // (which can happen for commits inserted before
        // `upsert_observed_authors` ran).
        //
        // The optional `author_email` filter restricts to rows whose resolved
        // canonical email matches case-insensitively.  We use
        // `LOWER(COALESCE(...)) = LOWER(?)` so that the filter still works
        // for commits that pre-date `upsert_observed_authors` and fall back
        // to the raw `c.author_email` field.
        // Issue #445 batch B (request #6): include cl.complexity so the weekly
        // aggregator can surface avg_complexity without a second DB scan.
        // Issue #1113: include c.agentic_mode for per-week agentic-% aggregation.
        let sql_base = "SELECT c.sha, \
                        COALESCE(a.canonical_name,  c.author_name)  AS author_name, \
                        COALESCE(NULLIF(a.canonical_email, ''), c.author_email) AS author_email, \
                        c.timestamp, c.repository, \
                        c.insertions, c.deletions, c.files_changed, cl.category, \
                        c.message, c.ticketed, c.is_ai_assisted, cl.complexity, \
                        COALESCE(c.agentic_mode, 'none') AS agentic_mode \
                 FROM commits c \
                 LEFT JOIN authors a ON a.id = c.author_id \
                 LEFT JOIN classifications cl ON cl.id = c.classification_id";

        let row_mapper = |row: &rusqlite::Row<'_>| -> rusqlite::Result<CommitRow> {
            let ts_str: String = row.get(3)?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let ticketed: i64 = row.get(10).unwrap_or(0);
            // Issue #445: column added in migration v17; default 0 for rows
            // that pre-date the migration (SQLite returns NULL as 0 via unwrap_or).
            let is_ai_assisted: i64 = row.get(11).unwrap_or(0);
            // Issue #445 batch B (request #6): complexity from classifications.
            // NULL for non-LLM tiers; pre-migration rows also return NULL.
            let complexity: Option<i64> = row.get(12).unwrap_or(None);
            // Issue #1113: agentic_mode TEXT; defaults to 'none' for pre-v21 rows.
            let agentic_mode_str: String = row.get(13).unwrap_or_else(|_| "none".to_string());
            let agentic_mode = agentic_mode_str
                .parse::<AgenticMode>()
                .unwrap_or(AgenticMode::None);
            Ok(CommitRow {
                sha: row.get(0)?,
                author_name: row.get(1)?,
                author_email: row.get(2)?,
                timestamp,
                repository: row.get(4)?,
                insertions: row.get(5)?,
                deletions: row.get(6)?,
                files_changed: row.get(7)?,
                category: row.get(8)?,
                message: row.get(9)?,
                ticketed: ticketed != 0,
                is_ai_assisted: is_ai_assisted != 0,
                complexity,
                agentic_mode,
            })
        };

        let mut out: Vec<CommitRow> = Vec::new();

        if let Some(email) = author_email {
            let sql = format!(
                "{sql_base} \
                 WHERE LOWER(COALESCE(NULLIF(a.canonical_email, ''), c.author_email)) = LOWER(?1)"
            );
            let mut stmt = conn.prepare(&sql).map_err(crate::core::TgaError::from)?;
            let rows = stmt
                .query_map(rusqlite::params![email], row_mapper)
                .map_err(crate::core::TgaError::from)?;
            for r in rows {
                out.push(r.map_err(crate::core::TgaError::from)?);
            }
        } else {
            let mut stmt = conn
                .prepare(sql_base)
                .map_err(crate::core::TgaError::from)?;
            let rows = stmt
                .query_map([], row_mapper)
                .map_err(crate::core::TgaError::from)?;
            for r in rows {
                out.push(r.map_err(crate::core::TgaError::from)?);
            }
        }

        debug!(count = out.len(), "loaded commit rows for aggregation");
        Ok(out)
    }

    /// Build the in-memory [`ReportData`] from already-loaded rows.
    ///
    /// Why: keeping the row→report transformation pure (no I/O) makes it
    /// trivial to unit-test against fixture data and to decompose into
    /// named phases.
    /// What: orchestrates the pipeline — pre-pass row flagging,
    /// single-pass accumulation, materialisation of each output slice,
    /// and computation of derived metrics (velocity / DORA / quality /
    /// developer activity).
    /// Test: indirectly via `Aggregator::build` tests; behaviour is a
    /// pure refactor — every output field is produced by a named helper
    /// below.
    fn aggregate(rows: Vec<CommitRow>, prs: Vec<PrRow>) -> ReportData {
        let generated_at = Utc::now().to_rfc3339();
        let mut data = ReportData::empty(generated_at);

        if rows.is_empty() {
            return data;
        }

        // Pre-pass: flag boilerplate / revert rows once and reuse the bits
        // throughout the rest of the pipeline.
        let row_flags = compute_row_flags(&rows);

        // Single-pass scan: accumulate per-author / per-repo / per-week /
        // per-developer state from `rows`.
        let acc = accumulate_rows(&rows, &row_flags);

        // Materialise the canonical author / repo / weekly-activity slices.
        let author_summaries = materialize_authors(acc.authors);
        let repo_summaries = materialize_repositories(acc.repos);
        let email_to_name: HashMap<String, String> = author_summaries
            .iter()
            .map(|a| (a.email.clone(), a.name.clone()))
            .collect();
        // Issue #377: abandoned (closed-unmerged) PRs, bucketed per week per
        // author login, for best-effort per-engineer attribution.
        let abandoned_by_week_identity = build_abandoned_pr_counts(&prs);
        let weekly_activity =
            materialize_weekly_activity(acc.weekly, &email_to_name, &abandoned_by_week_identity);

        let total_commits = rows.len();
        let total_authors = author_summaries.len();
        let total_weeks = acc.week_totals.len();

        let weekly_metrics = build_weekly_metrics(&acc.week_totals);
        let weekly_categorization = build_weekly_categorization(&acc.week_totals);
        let untracked_commits = build_untracked_commits(&rows, &email_to_name);

        // Velocity inputs depend on PR cycle-time arithmetic; compute once
        // and reuse for the per-week velocity rows and DORA lead-time.
        let velocity_inputs = compute_velocity_inputs(&prs);
        let velocity = Some(VelocitySummary {
            pr_cycle_time_avg_hours: velocity_inputs.cycle_time_avg,
            pr_cycle_time_median_hours: velocity_inputs.cycle_time_median,
            pr_throughput_per_week: velocity_inputs.pr_throughput_per_week,
            revision_rate: 0.0,
            pr_count: velocity_inputs.pr_count,
        });
        let weekly_velocity = build_weekly_velocity(
            &acc.week_totals,
            &velocity_inputs.pr_per_week,
            velocity_inputs.cycle_time_avg,
        );

        let dora = Some(compute_dora(
            &rows,
            &row_flags,
            &acc.category_total,
            &prs,
            velocity_inputs.cycle_time_avg,
            total_weeks,
            acc.revert_count,
        ));

        let quality = Some(compute_quality(
            total_commits,
            &acc.category_total,
            acc.revert_count,
        ));

        // Per-developer composite activity score and roll-up rows.
        let weights = ActivityWeights::default();
        let developer_activity = compute_developer_activity(
            &author_summaries,
            &acc.dev_weeks,
            &acc.dev_categories,
            &weights,
        );

        let summary = Some(build_summary(
            &rows,
            total_commits,
            total_authors,
            total_weeks,
            acc.min_ts,
            acc.max_ts,
        ));

        data.total_commits = total_commits;
        data.total_authors = total_authors;
        data.period_start = Some(acc.min_ts.to_rfc3339());
        data.period_end = Some(acc.max_ts.to_rfc3339());
        data.authors = author_summaries;
        data.repositories = repo_summaries;
        data.weekly_activity = weekly_activity;
        data.category_breakdown = acc.category_total;
        data.weekly_metrics = weekly_metrics;
        data.developer_activity = developer_activity;
        data.summary = summary;
        data.untracked_commits = untracked_commits;
        data.weekly_categorization = weekly_categorization;
        data.weekly_velocity = weekly_velocity;
        data.dora = dora;
        data.velocity = velocity;
        data.quality = quality;
        data.boilerplate_count = acc.boilerplate_count;
        data.revert_count = acc.revert_count;
        // Silence unused-field warnings for trackers that today only feed
        // activity scoring; future scoring tweaks will consume these.
        let _ = acc.dev_ticketed;
        data
    }

    /// Persist per-engineer-per-week quality scores to `fact_weekly_quality`.
    ///
    /// Why: callers (CLI pipeline, tests) use `Aggregator::persist_weekly_quality`;
    /// the implementation lives in [`crate::report::persist`] to keep this file
    /// within the 500-line cap.
    /// What: delegates to [`crate::report::persist::persist_weekly_quality`].
    /// Test: `report::tests::persist_weekly_quality_upserts_rows`.
    pub fn persist_weekly_quality(db: &Database, data: &ReportData) -> Result<usize> {
        crate::report::persist::persist_weekly_quality(db, data)
    }

    /// Persist per-engineer-per-week agentic counts to `fact_weekly_engineer`.
    ///
    /// Why: callers (CLI pipeline, tests) use `Aggregator::persist_weekly_engineer`;
    /// the implementation lives in [`crate::report::persist`] to keep this file
    /// within the 500-line cap.
    /// What: delegates to [`crate::report::persist::persist_weekly_engineer`].
    /// Test: `report::tests::persist_weekly_engineer_upserts_rows`.
    pub fn persist_weekly_engineer(db: &Database, data: &ReportData) -> Result<usize> {
        crate::report::persist::persist_weekly_engineer(db, data)
    }
}

mod accumulate;
mod metrics;

use accumulate::{
    accumulate_rows, build_abandoned_pr_counts, build_untracked_commits,
    build_weekly_categorization, build_weekly_metrics, compute_row_flags, materialize_authors,
    materialize_repositories, materialize_weekly_activity,
};
use metrics::{
    build_summary, build_weekly_velocity, check_weekly_coverage_drift, compute_developer_activity,
    compute_dora, compute_quality, compute_velocity_inputs, configured_alias_emails,
};
