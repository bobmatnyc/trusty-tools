//! Database aggregation: turn raw rows into [`ReportData`].
//!
//! The aggregator runs a single scan of the `commits` table (left-joined
//! against `classifications`) and groups the results in-memory. For the
//! data sizes typical of `trusty-git-analytics` this is simpler and
//! faster than emitting multiple grouped SQL queries.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Datelike, Utc};
use regex::Regex;
use tracing::{debug, warn};

use crate::core::config::Config;
use crate::core::db::Database;
use crate::report::errors::{ReportError, Result};
use crate::report::models::{
    ActivityWeights, AuthorSummary, DeveloperActivitySummary, DoraMetrics, QualitySummary,
    ReportData, ReportSummary, RepositorySummary, UntrackedCommit, VelocitySummary, WeeklyActivity,
    WeeklyCategorization, WeeklyMetrics, WeeklyVelocity,
};

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
struct CommitRow {
    sha: String,
    author_name: String,
    author_email: String,
    timestamp: DateTime<Utc>,
    repository: String,
    insertions: i64,
    deletions: i64,
    files_changed: i64,
    category: Option<String>,
    message: String,
    ticketed: bool,
}

/// Minimal PR row used by velocity / DORA computations and (issue #377)
/// abandoned-PR counting.
struct PrRow {
    /// PR author login as recorded by the provider (e.g. GitHub login).
    /// Note: this is NOT a canonical engineer email — see
    /// [`build_abandoned_pr_counts`] for the attribution limitation.
    author: String,
    /// Provider lifecycle state: `"open"`, `"closed"`, or `"merged"`.
    state: String,
    created_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
}

/// Default regex patterns identifying machine-generated commits.
///
/// Why: keep boilerplate (lock-file bumps, version bumps, merge commits, …)
/// from skewing per-developer averages. Matched case-insensitively against
/// the first line of each commit message.
const DEFAULT_BOILERPLATE_PATTERNS: &[&str] = &[
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
const BOILERPLATE_LINES_THRESHOLD: i64 = 500;

/// Heuristic boilerplate detector.
///
/// Why: prevents auto-generated commits (lock-file bumps, version bumps,
/// generated code) from skewing per-developer averages.
/// What: returns `true` when the message matches any boilerplate pattern OR
/// the lines-changed budget exceeds [`BOILERPLATE_LINES_THRESHOLD`].
/// Test: feed a `"Update package-lock.json"` message → `true`; a normal
/// `"feat: x"` message with small diff → `false`.
fn is_boilerplate(message: &str, lines_changed: i64, patterns: &[Regex]) -> bool {
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
fn compile_patterns(patterns: &[&str]) -> Vec<Regex> {
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
        let sql_base = "SELECT c.sha, \
                        COALESCE(a.canonical_name,  c.author_name)  AS author_name, \
                        COALESCE(NULLIF(a.canonical_email, ''), c.author_email) AS author_email, \
                        c.timestamp, c.repository, \
                        c.insertions, c.deletions, c.files_changed, cl.category, \
                        c.message, c.ticketed \
                 FROM commits c \
                 LEFT JOIN authors a ON a.id = c.author_id \
                 LEFT JOIN classifications cl ON cl.id = c.classification_id";

        let row_mapper = |row: &rusqlite::Row<'_>| -> rusqlite::Result<CommitRow> {
            let ts_str: String = row.get(3)?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let ticketed: i64 = row.get(10).unwrap_or(0);
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
}

// ===========================================================================
// Aggregation helpers (decomposed phases of `Aggregator::aggregate`)
// ===========================================================================

/// Pre-pass boilerplate / revert flags per row.
///
/// Why: every later phase (DORA bugfix counting, weekly-categorization
/// boilerplate bucketing) needs these bits, and recomputing per phase
/// would scan the row vector multiple times.
/// What: bundles a parallel `is_boilerplate` / `is_revert` `Vec<bool>`
/// indexed by row position, plus the aggregate counts.
/// Test: behavior preserved — the same `is_boilerplate` / `is_revert`
/// helpers run inline previously.
struct RowFlags {
    is_boilerplate: Vec<bool>,
    is_revert: Vec<bool>,
    boilerplate_count: usize,
    revert_count: usize,
}

/// Why: keep flag computation in one named place so the main aggregate
/// function reads as a recipe of phases.
/// What: compiles the default regex sets once, walks the rows, and returns
/// a [`RowFlags`] capturing both per-row bits and aggregate counts.
/// Test: indirectly via report tests; identical to the inline loop that
/// existed in `aggregate` before this refactor.
fn compute_row_flags(rows: &[CommitRow]) -> RowFlags {
    let boilerplate_re = compile_patterns(DEFAULT_BOILERPLATE_PATTERNS);

    let mut is_boilerplate: Vec<bool> = Vec::with_capacity(rows.len());
    let mut is_revert: Vec<bool> = Vec::with_capacity(rows.len());
    for row in rows {
        let lines = row.insertions + row.deletions;
        is_boilerplate.push(self::is_boilerplate(&row.message, lines, &boilerplate_re));
        // Issue #377: route revert detection through the shared core helper so
        // the report-time revert rate matches the persisted `is_revert` column.
        is_revert.push(crate::core::revert::is_revert(&row.message));
    }
    let boilerplate_count = is_boilerplate.iter().filter(|b| **b).count();
    let revert_count = is_revert.iter().filter(|b| **b).count();
    RowFlags {
        is_boilerplate,
        is_revert,
        boilerplate_count,
        revert_count,
    }
}

/// Per-author running totals during accumulation.
struct AuthorAcc {
    name: String,
    email: String,
    commits: usize,
    insertions: i64,
    deletions: i64,
    files_changed: i64,
    categories: HashMap<String, usize>,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

/// Per-repository running totals during accumulation.
struct RepoAcc {
    commits: usize,
    authors: HashSet<String>,
    insertions: i64,
    deletions: i64,
    categories: HashMap<String, usize>,
}

/// Per-(week, author, repo) running totals during accumulation.
struct WeekAcc {
    commits: usize,
    insertions: i64,
    deletions: i64,
    categories: HashMap<String, usize>,
    /// Revert commits in this bucket (issue #377 quality metric).
    reverts: usize,
    /// Bugfix-classified commits in this bucket (issue #377).
    bugfixes: usize,
    /// Ticketed commits in this bucket (issue #377).
    ticketed: usize,
}

/// Cross-developer per-week running totals during accumulation.
#[derive(Default)]
struct WeekTotal {
    commits: usize,
    categories: HashMap<String, usize>,
    developers: HashSet<String>,
}

/// Bundle of accumulator state that the single-pass scan produces.
///
/// Why: the row scan computes many parallel histograms at once; returning
/// them as a single struct keeps the orchestration in `aggregate` readable.
/// What: groups author / repo / weekly buckets and per-developer trackers
/// alongside the period bounds and aggregate counts.
/// Test: see `Aggregator::build` tests which exercise the full pipeline.
struct Accumulators {
    authors: HashMap<String, AuthorAcc>,
    repos: HashMap<String, RepoAcc>,
    weekly: BTreeMap<(String, String, String), WeekAcc>,
    category_total: HashMap<String, usize>,
    week_totals: BTreeMap<String, WeekTotal>,
    dev_weeks: HashMap<String, HashSet<String>>,
    dev_categories: HashMap<String, HashMap<String, usize>>,
    dev_ticketed: HashMap<String, usize>,
    min_ts: DateTime<Utc>,
    max_ts: DateTime<Utc>,
    boilerplate_count: usize,
    revert_count: usize,
}

/// Why: the row scan touches a dozen parallel histograms; isolating it in a
/// named function lets the aggregator orchestration read as a sequence of
/// phases.
/// What: runs one pass over `rows`, updating the per-author / per-repo /
/// per-week / per-developer accumulators in lockstep. Caller is `aggregate`.
/// Test: indirectly via the `aggregator_*` tests in `report::tests`; this
/// is a literal lift of the inline loop that lived in `aggregate`.
fn accumulate_rows(rows: &[CommitRow], flags: &RowFlags) -> Accumulators {
    // Period bounds initialised to the first row's timestamp.
    let mut min_ts = rows[0].timestamp;
    let mut max_ts = rows[0].timestamp;

    let mut authors: HashMap<String, AuthorAcc> = HashMap::new();
    let mut repos: HashMap<String, RepoAcc> = HashMap::new();
    let mut weekly: BTreeMap<(String, String, String), WeekAcc> = BTreeMap::new();
    let mut category_total: HashMap<String, usize> = HashMap::new();
    let mut week_totals: BTreeMap<String, WeekTotal> = BTreeMap::new();
    let mut dev_weeks: HashMap<String, HashSet<String>> = HashMap::new();
    let mut dev_categories: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut dev_ticketed: HashMap<String, usize> = HashMap::new();

    for (idx, row) in rows.iter().enumerate() {
        if row.timestamp < min_ts {
            min_ts = row.timestamp;
        }
        if row.timestamp > max_ts {
            max_ts = row.timestamp;
        }

        // Authors. Group by email only; pick the longest display name seen
        // as the canonical name (heuristic: longer names tend to be the full
        // "Firstname Lastname" form rather than a short login handle).
        let key = row.author_email.clone();
        let a = authors.entry(key).or_insert_with(|| AuthorAcc {
            name: row.author_name.clone(),
            email: row.author_email.clone(),
            commits: 0,
            insertions: 0,
            deletions: 0,
            files_changed: 0,
            categories: HashMap::new(),
            first: row.timestamp,
            last: row.timestamp,
        });
        if row.author_name.len() > a.name.len() {
            a.name = row.author_name.clone();
        }
        a.commits += 1;
        a.insertions += row.insertions;
        a.deletions += row.deletions;
        a.files_changed += row.files_changed;
        if row.timestamp < a.first {
            a.first = row.timestamp;
        }
        if row.timestamp > a.last {
            a.last = row.timestamp;
        }
        if let Some(cat) = &row.category {
            *a.categories.entry(cat.clone()).or_insert(0) += 1;
        }

        // Repositories.
        let r = repos
            .entry(row.repository.clone())
            .or_insert_with(|| RepoAcc {
                commits: 0,
                authors: HashSet::new(),
                insertions: 0,
                deletions: 0,
                categories: HashMap::new(),
            });
        r.commits += 1;
        r.authors.insert(row.author_email.clone());
        r.insertions += row.insertions;
        r.deletions += row.deletions;
        if let Some(cat) = &row.category {
            *r.categories.entry(cat.clone()).or_insert(0) += 1;
        }

        // Weekly. Keyed by email (not display name) so that the same identity
        // committing under multiple names lands in a single weekly bucket.
        let week = iso_week_label(&row.timestamp);
        let wkey = (week, row.author_email.clone(), row.repository.clone());
        let w = weekly.entry(wkey).or_insert_with(|| WeekAcc {
            commits: 0,
            insertions: 0,
            deletions: 0,
            categories: HashMap::new(),
            reverts: 0,
            bugfixes: 0,
            ticketed: 0,
        });
        w.commits += 1;
        w.insertions += row.insertions;
        w.deletions += row.deletions;
        if let Some(cat) = &row.category {
            *w.categories.entry(cat.clone()).or_insert(0) += 1;
        }
        // Issue #377: per-(week, engineer, repo) quality signals. `is_revert`
        // is the shared-helper verdict computed in `compute_row_flags`;
        // `bugfix` comes from the classifier category; `ticketed` from the
        // commit's ticket-reference flag.
        if flags.is_revert[idx] {
            w.reverts += 1;
        }
        if row.category.as_deref() == Some("bugfix") {
            w.bugfixes += 1;
        }
        if row.ticketed {
            w.ticketed += 1;
        }

        // Category totals.
        if let Some(cat) = &row.category {
            *category_total.entry(cat.clone()).or_insert(0) += 1;
        }

        // Cross-developer weekly totals.
        let week_label = iso_week_label(&row.timestamp);
        let wt = week_totals.entry(week_label.clone()).or_default();
        wt.commits += 1;
        wt.developers.insert(row.author_email.clone());
        // Treat boilerplate rows as a synthetic category so they show
        // up in `weekly_categorization.csv` rather than being silently
        // bucketed into whatever the classifier returned.
        if flags.is_boilerplate[idx] {
            *wt.categories.entry("boilerplate".to_string()).or_insert(0) += 1;
        } else if let Some(cat) = &row.category {
            *wt.categories.entry(cat.clone()).or_insert(0) += 1;
        } else {
            *wt.categories.entry("unclassified".to_string()).or_insert(0) += 1;
        }

        // Per-developer week / category / ticketed tracking.
        dev_weeks
            .entry(row.author_email.clone())
            .or_default()
            .insert(week_label);
        if let Some(cat) = &row.category {
            *dev_categories
                .entry(row.author_email.clone())
                .or_default()
                .entry(cat.clone())
                .or_insert(0) += 1;
        }
        if row.ticketed {
            *dev_ticketed.entry(row.author_email.clone()).or_insert(0) += 1;
        }
    }

    Accumulators {
        authors,
        repos,
        weekly,
        category_total,
        week_totals,
        dev_weeks,
        dev_categories,
        dev_ticketed,
        min_ts,
        max_ts,
        boilerplate_count: flags.boilerplate_count,
        revert_count: flags.revert_count,
    }
}

/// Why: report consumers expect authors sorted by commit count with the
/// canonical (longest-seen) display name.
/// What: drains the author accumulator into [`AuthorSummary`] rows and
/// sorts them by descending commit count.
/// Test: indirectly via `aggregator_builds_report_data`.
fn materialize_authors(authors: HashMap<String, AuthorAcc>) -> Vec<AuthorSummary> {
    let mut summaries: Vec<AuthorSummary> = authors
        .into_values()
        .map(|a| AuthorSummary {
            name: a.name,
            email: a.email,
            commit_count: a.commits,
            insertions: a.insertions,
            deletions: a.deletions,
            files_changed: a.files_changed,
            categories: a.categories,
            first_commit: a.first.to_rfc3339(),
            last_commit: a.last.to_rfc3339(),
        })
        .collect();
    summaries.sort_by_key(|a| std::cmp::Reverse(a.commit_count));
    summaries
}

/// Why: per-repo rows in reports include the top categories for the repo,
/// sorted by frequency, so reviewers can see at a glance what work
/// dominates each codebase.
/// What: drains the repo accumulator into [`RepositorySummary`] with the
/// top-categories vector sorted descending by count; the outer Vec is
/// sorted by descending repo commit count.
/// Test: indirectly via `aggregator_builds_report_data`.
fn materialize_repositories(repos: HashMap<String, RepoAcc>) -> Vec<RepositorySummary> {
    let mut summaries: Vec<RepositorySummary> = repos
        .into_iter()
        .map(|(name, r)| {
            let mut top: Vec<(String, usize)> = r.categories.into_iter().collect();
            top.sort_by_key(|t| std::cmp::Reverse(t.1));
            RepositorySummary {
                name,
                commit_count: r.commits,
                author_count: r.authors.len(),
                insertions: r.insertions,
                deletions: r.deletions,
                top_categories: top,
            }
        })
        .collect();
    summaries.sort_by_key(|r| std::cmp::Reverse(r.commit_count));
    summaries
}

/// Why: the weekly bucket key uses email, but reports want canonical
/// display names so a single identity reads the same across the report.
/// What: drains the weekly map into [`WeeklyActivity`] rows, resolving each
/// row's email to its canonical display name via the `email_to_name` lookup
/// built from the already-materialised author summaries.
/// Test: indirectly via `aggregator_builds_report_data` (two weekly rows
/// for two authors in different weeks).
fn materialize_weekly_activity(
    weekly: BTreeMap<(String, String, String), WeekAcc>,
    email_to_name: &HashMap<String, String>,
    abandoned_by_week_identity: &HashMap<(String, String), usize>,
) -> Vec<WeeklyActivity> {
    weekly
        .into_iter()
        .map(|((week, email, repository), w)| {
            let author = email_to_name.get(&email).cloned().unwrap_or(email.clone());
            // Issue #377 quality score for this (week, engineer, repo) bucket.
            let (quality_score, quality_tshirt) =
                crate::core::quality::score_and_tshirt(crate::core::quality::QualityInputs {
                    commits: w.commits,
                    reverts: w.reverts,
                    bugfixes: w.bugfixes,
                    ticketed: w.ticketed,
                });
            // Best-effort abandoned-PR attribution: match the PR author login
            // against either the resolved display name or the email
            // (case-insensitive). See `build_abandoned_pr_counts` for why this
            // is heuristic. Repository is not part of the PR identity key, so
            // a week's abandoned PRs land on the engineer's first repo bucket
            // for that week — counted once via the `.remove`-style guard would
            // require mutation; instead we look up by (week, identity) and
            // accept that an engineer active in multiple repos in one week
            // sees the same abandoned count echoed per repo row. Downstream
            // joins on (week, author) so this is acceptable and documented.
            let abandoned_pr_count = abandoned_by_week_identity
                .get(&(week.clone(), author.to_lowercase()))
                .or_else(|| abandoned_by_week_identity.get(&(week.clone(), email.to_lowercase())))
                .copied()
                .unwrap_or(0);
            WeeklyActivity {
                week,
                author,
                repository,
                commit_count: w.commits,
                insertions: w.insertions,
                deletions: w.deletions,
                categories: w.categories,
                revert_count: w.reverts,
                bugfix_count: w.bugfixes,
                ticketed_count: w.ticketed,
                quality_score,
                quality_tshirt,
                abandoned_pr_count,
            }
        })
        .collect()
}

/// Build a `(iso_week, author_identity_lowercased) → abandoned_pr_count` map.
///
/// Why: closed-but-unmerged PRs are a strong quality signal that today is
/// impossible to compute downstream (issue #377). Counting them per engineer
/// per week lets reports surface the abandoned-PR rate.
/// What: filters `prs` to `state == "closed" && merged_at.is_none()`, buckets
/// each by the ISO week of its `created_at` (abandoned PRs have no merge/close
/// timestamp available, so creation week is the only stable anchor), and keys
/// the count by the lowercased author login.
///
/// Limitation: the PR `author` is a provider login (e.g. a GitHub handle),
/// NOT a canonical engineer email. TGA has no login→engineer mapping at
/// aggregation time, so attribution in [`materialize_weekly_activity`] is a
/// best-effort case-insensitive match of the login against the engineer's
/// display name or email. When a login matches neither, the abandoned PR is
/// counted here but cannot be attributed to a weekly-activity row and is
/// effectively dropped from the per-engineer column. A future change that
/// persists a login→author_id mapping would make this exact.
/// Test: `aggregator_counts_abandoned_prs` in `report::tests`.
fn build_abandoned_pr_counts(prs: &[PrRow]) -> HashMap<(String, String), usize> {
    let mut out: HashMap<(String, String), usize> = HashMap::new();
    for pr in prs {
        if pr.state == "closed" && pr.merged_at.is_none() {
            let week = iso_week_label(&pr.created_at);
            *out.entry((week, pr.author.to_lowercase())).or_insert(0) += 1;
        }
    }
    out
}

/// Why: weekly metrics are the cross-developer roll-up used for trend
/// charts; bucketing per category keeps the schema fixed regardless of
/// which categories appeared in the data.
/// What: walks the week-totals map and emits one [`WeeklyMetrics`] row per
/// ISO week with named bucket counters (feature / bugfix / maintenance /
/// refactor / test / docs).
/// Test: indirectly via `aggregator_builds_report_data` (asserts the
/// weekly_metrics vector is populated).
fn build_weekly_metrics(week_totals: &BTreeMap<String, WeekTotal>) -> Vec<WeeklyMetrics> {
    week_totals
        .iter()
        .map(|(week, wt)| WeeklyMetrics {
            week: week.clone(),
            total_commits: wt.commits,
            feature_commits: *wt.categories.get("feature").unwrap_or(&0),
            bugfix_commits: *wt.categories.get("bugfix").unwrap_or(&0),
            maintenance_commits: *wt.categories.get("maintenance").unwrap_or(&0),
            refactor_commits: *wt.categories.get("refactor").unwrap_or(&0),
            test_commits: *wt.categories.get("test").unwrap_or(&0),
            doc_commits: *wt.categories.get("documentation").unwrap_or(&0)
                + *wt.categories.get("docs").unwrap_or(&0),
            active_developers: wt.developers.len(),
            story_points: 0.0,
        })
        .collect()
}

/// Why: the `weekly_categorization.csv` report needs one row per
/// (week, change-type) with the percentage share, so consumers can build
/// stacked-bar charts of "what work happened this week".
/// What: iterates the week-totals map and emits one row per category seen
/// in each week, sorted by category name for deterministic output.
/// Test: covered by `csv_formatter_writes_new_report_files` which writes
/// the weekly_categorization CSV.
fn build_weekly_categorization(
    week_totals: &BTreeMap<String, WeekTotal>,
) -> Vec<WeeklyCategorization> {
    let mut rows: Vec<WeeklyCategorization> = Vec::new();
    for (week, wt) in week_totals {
        let total = wt.commits as f64;
        let mut entries: Vec<(&String, &usize)> = wt.categories.iter().collect();
        entries.sort_by_key(|e| e.0);
        for (cat, count) in entries {
            rows.push(WeeklyCategorization {
                week: week.clone(),
                change_type: cat.clone(),
                commit_count: *count,
                pct_of_week: if total > 0.0 {
                    (*count as f64) * 100.0 / total
                } else {
                    0.0
                },
            });
        }
    }
    rows
}

/// Why: untracked-commit rows surface commits without a ticket reference so
/// PMs can chase down missing trackable work.
/// What: filters `rows` to those that are unticketed and not boilerplate,
/// resolves each row's author email to its canonical display name, and
/// emits rows sorted newest-first.
/// Test: covered indirectly via `csv_formatter_writes_new_report_files`
/// (writes the `untracked.csv` file from this data).
fn build_untracked_commits(
    rows: &[CommitRow],
    email_to_name: &HashMap<String, String>,
) -> Vec<UntrackedCommit> {
    let mut out: Vec<UntrackedCommit> = rows
        .iter()
        .filter(|r| !r.ticketed && r.category.as_deref() != Some("boilerplate"))
        .filter(|r| {
            // Treat NULL category OR explicit "unclassified" as untracked.
            r.category.is_none() || r.category.as_deref() == Some("unclassified") || !r.ticketed
        })
        .map(|r| UntrackedCommit {
            sha: r.sha.clone(),
            author: email_to_name
                .get(&r.author_email)
                .cloned()
                .unwrap_or_else(|| r.author_name.clone()),
            date: r.timestamp.to_rfc3339(),
            message: r.message.lines().next().unwrap_or("").to_string(),
        })
        .collect();
    // Deterministic ordering: newest first.
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// Outputs of [`compute_velocity_inputs`].
struct VelocityInputs {
    cycle_time_avg: f64,
    cycle_time_median: f64,
    pr_throughput_per_week: f64,
    pr_count: usize,
    pr_per_week: HashMap<String, usize>,
}

/// Why: the velocity summary, weekly-velocity rows, and DORA lead-time all
/// derive from the same PR cycle-time arithmetic; computing it once keeps
/// the orchestrator readable and prevents drift between the metrics.
/// What: filters merged PRs to a sane cycle-time range (0.5–720 hours),
/// computes mean and median in hours, buckets merge timestamps by ISO week
/// for throughput, and returns the bundle.
/// Test: indirectly via `aggregator_computes_summary_and_dora_and_quality`.
fn compute_velocity_inputs(prs: &[PrRow]) -> VelocityInputs {
    let mut cycle_times: Vec<f64> = prs
        .iter()
        .filter_map(|p| {
            p.merged_at.map(|m| {
                let secs = (m - p.created_at).num_seconds();
                (secs as f64) / 3600.0
            })
        })
        .filter(|h| *h >= 0.5 && *h <= 720.0)
        .collect();
    cycle_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pr_count = cycle_times.len();
    let cycle_time_avg = if pr_count == 0 {
        0.0
    } else {
        cycle_times.iter().sum::<f64>() / pr_count as f64
    };
    let cycle_time_median = if pr_count == 0 {
        0.0
    } else {
        cycle_times[pr_count / 2]
    };

    let mut pr_per_week: HashMap<String, usize> = HashMap::new();
    for pr in prs {
        if let Some(merged) = pr.merged_at {
            *pr_per_week.entry(iso_week_label(&merged)).or_insert(0) += 1;
        }
    }
    let pr_throughput_per_week = if pr_per_week.is_empty() {
        0.0
    } else {
        pr_per_week.values().copied().sum::<usize>() as f64 / pr_per_week.len() as f64
    };

    VelocityInputs {
        cycle_time_avg,
        cycle_time_median,
        pr_throughput_per_week,
        pr_count,
        pr_per_week,
    }
}

/// Why: per-week velocity rows align PR throughput with active-developer
/// counts so reports can show team-level pace.
/// What: walks the week-totals map and emits one [`WeeklyVelocity`] row
/// per ISO week, joining against the `pr_per_week` lookup built by
/// [`compute_velocity_inputs`].
/// Test: covered indirectly by `csv_formatter_writes_new_report_files`
/// (writes the weekly velocity CSV).
fn build_weekly_velocity(
    week_totals: &BTreeMap<String, WeekTotal>,
    pr_per_week: &HashMap<String, usize>,
    cycle_time_avg: f64,
) -> Vec<WeeklyVelocity> {
    week_totals
        .iter()
        .map(|(week, wt)| {
            let prs_merged = *pr_per_week.get(week).unwrap_or(&0);
            let active = wt.developers.len();
            let commits_per_dev = if active == 0 {
                0.0
            } else {
                wt.commits as f64 / active as f64
            };
            WeeklyVelocity {
                week: week.clone(),
                prs_merged,
                avg_pr_cycle_time_hours: cycle_time_avg,
                story_points: 0.0,
                commits_per_developer: commits_per_dev,
            }
        })
        .collect()
}

/// Why: DORA metrics are the standard rubric stakeholders use to score
/// engineering performance; computing them in one place keeps the four
/// values consistent with each other.
/// What: derives deployment frequency from merged PRs, change-failure-rate
/// from bugfix totals (clamped by revert count), and MTTR from the spacing
/// between consecutive bugfix/revert commits; classifies the team via
/// [`dora_level`].
/// Test: covered by `aggregator_computes_summary_and_dora_and_quality`
/// (asserts a well-formed `performance_level` is set).
fn compute_dora(
    rows: &[CommitRow],
    flags: &RowFlags,
    category_total: &HashMap<String, usize>,
    prs: &[PrRow],
    cycle_time_avg: f64,
    total_weeks: usize,
    revert_count: usize,
) -> DoraMetrics {
    let total_weeks_f = total_weeks.max(1) as f64;
    let total_commits = rows.len();
    let deploys = prs.iter().filter(|p| p.merged_at.is_some()).count();
    let deployment_frequency = deploys as f64 / total_weeks_f;
    let bugfix_total = category_total
        .get("bugfix")
        .copied()
        .unwrap_or(0)
        .max(revert_count);
    let change_failure_rate = if total_commits == 0 {
        0.0
    } else {
        bugfix_total as f64 / total_commits as f64
    };

    // MTTR approximation: average hours from a revert commit's predecessor
    // (assumed bug introduction) to the revert itself. Without a richer
    // mapping we approximate via the gap between consecutive bugfix
    // commits, capped by available data.
    let mut bugfix_ts: Vec<DateTime<Utc>> = rows
        .iter()
        .zip(flags.is_revert.iter())
        .filter(|(r, is_rev)| **is_rev || r.category.as_deref() == Some("bugfix"))
        .map(|(r, _)| r.timestamp)
        .collect();
    bugfix_ts.sort();
    let mttr_hours = if bugfix_ts.len() < 2 {
        0.0
    } else {
        let mut gaps: Vec<f64> = Vec::new();
        for w in bugfix_ts.windows(2) {
            let secs = (w[1] - w[0]).num_seconds().abs();
            gaps.push(secs as f64 / 3600.0);
        }
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    let performance_level = dora_level(
        deployment_frequency,
        cycle_time_avg,
        change_failure_rate,
        mttr_hours,
    );
    DoraMetrics {
        deployment_frequency,
        lead_time_hours: cycle_time_avg,
        change_failure_rate,
        mttr_hours,
        performance_level,
    }
}

/// Why: a single 0.0–1.0 quality score lets stakeholders compare teams /
/// time periods without internalising the DORA rubric.
/// What: combines bugfix-pct and revert-pct (weighted 0.4 / 0.6) into a
/// clamped score, computes defect-rate as bugfix-over-non-bugfix, and
/// packages them in a [`QualitySummary`].
/// Test: covered by `aggregator_computes_summary_and_dora_and_quality`
/// (asserts `quality_score` is in `[0.0, 1.0]`).
fn compute_quality(
    total_commits: usize,
    category_total: &HashMap<String, usize>,
    revert_count: usize,
) -> QualitySummary {
    let bugfix_total = category_total
        .get("bugfix")
        .copied()
        .unwrap_or(0)
        .max(revert_count);
    let bugfix_pct = if total_commits == 0 {
        0.0
    } else {
        bugfix_total as f64 / total_commits as f64
    };
    let revert_pct = if total_commits == 0 {
        0.0
    } else {
        revert_count as f64 / total_commits as f64
    };
    let raw_quality = 1.0 - (bugfix_pct * 0.4) - (revert_pct * 0.6);
    let quality_score = raw_quality.clamp(0.0, 1.0);
    let non_bugfix = total_commits.saturating_sub(bugfix_total);
    let defect_rate = if non_bugfix == 0 {
        0.0
    } else {
        bugfix_total as f64 / non_bugfix as f64
    };
    QualitySummary {
        quality_score,
        revert_count,
        revert_pct,
        bugfix_pct,
        defect_rate,
    }
}

/// Why: every report needs a one-line "what does this cover" header so
/// readers can validate scope at a glance.
/// What: assembles a [`ReportSummary`] with the date range, totals, and
/// classification coverage percent.
/// Test: covered by `aggregator_computes_summary_and_dora_and_quality`
/// (asserts coverage_pct ≈ 50 with one of two commits classified).
fn build_summary(
    rows: &[CommitRow],
    total_commits: usize,
    total_authors: usize,
    total_weeks: usize,
    min_ts: DateTime<Utc>,
    max_ts: DateTime<Utc>,
) -> ReportSummary {
    let classified_commits = rows.iter().filter(|r| r.category.is_some()).count();
    let classification_coverage_pct = if total_commits == 0 {
        0.0
    } else {
        classified_commits as f64 * 100.0 / total_commits as f64
    };
    let date_range = format!("{} .. {}", min_ts.to_rfc3339(), max_ts.to_rfc3339());
    ReportSummary {
        date_range,
        total_commits,
        total_developers: total_authors,
        total_weeks,
        classification_coverage_pct,
    }
}

/// Compute composite developer activity scores and roll-up rows.
///
/// Why: provides a single configurable number for ranking developers across
/// commits / impact / hygiene without committing to one dimension.
/// What: applies min-max normalization to each component across the period,
/// then a weighted sum per `ActivityWeights`.
/// Test: seed two authors with different commit counts; assert the higher
/// commit count yields the higher activity score.
fn compute_developer_activity(
    authors: &[AuthorSummary],
    dev_weeks: &HashMap<String, HashSet<String>>,
    dev_categories: &HashMap<String, HashMap<String, usize>>,
    weights: &ActivityWeights,
) -> Vec<DeveloperActivitySummary> {
    if authors.is_empty() {
        return Vec::new();
    }

    // Min-max normalization helper. Returns 0.0 when all values are equal.
    fn norm(values: &[f64], idx: usize) -> f64 {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if (max - min).abs() < f64::EPSILON {
            0.0
        } else {
            (values[idx] - min) / (max - min)
        }
    }

    let commits_v: Vec<f64> = authors.iter().map(|a| a.commit_count as f64).collect();
    let impact_v: Vec<f64> = authors
        .iter()
        .map(|a| (a.insertions + a.deletions) as f64)
        .collect();
    let complexity_v: Vec<f64> = authors
        .iter()
        .map(|a| {
            if a.commit_count == 0 {
                0.0
            } else {
                a.files_changed as f64 / a.commit_count as f64
            }
        })
        .collect();
    // PRs and ticketing are placeholders until per-developer PR aggregation
    // exists; using categories-sum as a stand-in keeps the field stable.
    let prs_v: Vec<f64> = vec![0.0; authors.len()];
    let ticketing_v: Vec<f64> = authors
        .iter()
        .map(|a| a.categories.values().copied().sum::<usize>() as f64)
        .collect();

    authors
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let score = weights.commits * norm(&commits_v, i)
                + weights.prs * norm(&prs_v, i)
                + weights.code_impact * norm(&impact_v, i)
                + weights.complexity * norm(&complexity_v, i)
                + weights.ticketing * norm(&ticketing_v, i);
            let active_weeks = dev_weeks.get(&a.email).map(|s| s.len()).unwrap_or(0);
            let avg_commits_per_week = if active_weeks == 0 {
                0.0
            } else {
                a.commit_count as f64 / active_weeks as f64
            };
            let primary_work_type = dev_categories
                .get(&a.email)
                .and_then(|m| m.iter().max_by_key(|(_, v)| **v).map(|(k, _)| k.clone()))
                .unwrap_or_else(|| "unknown".to_string());
            DeveloperActivitySummary {
                developer_id: a.email.clone(),
                display_name: a.name.clone(),
                total_commits: a.commit_count,
                active_weeks,
                avg_commits_per_week,
                primary_work_type,
                story_points_total: 0.0,
                activity_score: score,
            }
        })
        .collect()
}

/// DORA performance-level classifier.
///
/// Why: surface the four-band rubric defined in `docs/trusty-git-analytics/requirements/reporting.md`.
/// What: returns `"elite" | "high" | "medium" | "low"` based on the four DORA
/// metrics.
/// Test: feed elite-range inputs (>= 1 deploy/week, < 1h lead, < 0.15 cfr,
/// < 1h MTTR) and assert the returned label is `"elite"`.
fn dora_level(deploys_per_week: f64, lead_h: f64, cfr: f64, mttr_h: f64) -> String {
    let elite = deploys_per_week >= 1.0 && lead_h < 1.0 && cfr < 0.15 && mttr_h < 1.0;
    if elite {
        return "elite".to_string();
    }
    let high = deploys_per_week >= 0.25 && lead_h < 168.0 && cfr < 0.30 && mttr_h < 24.0;
    if high {
        return "high".to_string();
    }
    let medium = deploys_per_week >= 0.04 && lead_h < 720.0 && cfr < 0.30 && mttr_h < 168.0;
    if medium {
        return "medium".to_string();
    }
    "low".to_string()
}

/// Parse an ISO week label of the form `"YYYY-Www"` into `(year, week)`.
///
/// Why: parsing once gives the coverage-drift check a way to look up the
/// recorded `repo_count` for the week.
/// What: returns `None` for malformed labels — callers should skip the
/// entry rather than abort the entire report.
/// Test: indirectly via `check_weekly_coverage_drift`.
fn parse_iso_week_label(label: &str) -> Option<(i32, u32)> {
    let (year_s, week_s) = label.split_once("-W")?;
    let year: i32 = year_s.parse().ok()?;
    let week: u32 = week_s.parse().ok()?;
    Some((year, week))
}

/// Emit a warning when adjacent weekly metric rows were collected with
/// different repository counts (issue #69). Coverage drift between weeks
/// makes week-over-week deltas misleading.
///
/// Why: weekly snapshots collected at different times may have different
/// `repositories[]` rosters; without surfacing this, WoW deltas look like
/// engineering changes when they're really configuration changes.
/// What: walks consecutive `weekly_metrics` entries, looks up the recorded
/// `repo_count` per week via [`crate::core::db::repo_count_for_week`], and
/// warns when the values disagree.
/// Test: seed `collection_runs` with two weeks at different repo_counts,
/// build a report, assert a warning is logged (smoke-tested via the
/// public `Aggregator::build` path).
fn check_weekly_coverage_drift(
    db: &Database,
    weekly_metrics: &[crate::report::models::WeeklyMetrics],
) {
    if weekly_metrics.len() < 2 {
        return;
    }
    let mut prev: Option<(String, i64)> = None;
    for wm in weekly_metrics {
        let (year, week) = match parse_iso_week_label(&wm.week) {
            Some(v) => v,
            None => continue,
        };
        let count = match crate::core::db::repo_count_for_week(db, year, week) {
            Ok(Some(n)) => n,
            // No recorded count for this week — either pre-migration data or
            // legacy `record_collection_run` calls. Skip silently; the user
            // will see normal output and we avoid noisy warnings on fresh
            // databases.
            _ => continue,
        };
        if let Some((prev_label, prev_count)) = &prev {
            if *prev_count != count {
                tracing::warn!(
                    prev_week = %prev_label,
                    prev_repo_count = prev_count,
                    week = %wm.week,
                    repo_count = count,
                    "WARNING: Week-over-week comparison may be inaccurate — W{prev} was \
                     collected with {n_prev} repos, W{cur} with {n_cur} repos. Re-run \
                     `tga collect --force --from <week-start> --to <week-end>` for the \
                     prior week to normalize coverage.",
                    prev = prev_label,
                    n_prev = prev_count,
                    cur = wm.week,
                    n_cur = count,
                );
            }
        }
        prev = Some((wm.week.clone(), count));
    }
}

/// Collect every email address referenced by the configured alias map
/// (`developer_aliases` + `team.members.email` + `team.members.aliases`)
/// for "is this author in the configured roster?" lookups.
///
/// Why: see issue #68 — when an author's canonical email is not in the
/// configured alias map they are a "phantom" identity that inflates the
/// developer count.
/// What: returns a set of lowercased email addresses; non-email aliases
/// (login handles) are filtered out so case-insensitive email comparison
/// is sufficient.
/// Test: build a `Config` with one developer_aliases entry, assert the
/// returned set contains the lowercased email.
fn configured_alias_emails(config: &Config) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    for entries in config.developer_aliases.values() {
        for e in entries {
            if e.contains('@') {
                out.insert(e.to_lowercase());
            }
        }
    }
    if let Some(team) = &config.team {
        for m in &team.members {
            if m.email.contains('@') {
                out.insert(m.email.to_lowercase());
            }
            for a in &m.aliases {
                if a.contains('@') {
                    out.insert(a.to_lowercase());
                }
            }
        }
    }
    out
}

/// Format an ISO week label such as `"2024-W03"` from a UTC timestamp.
///
/// Why: weekly buckets are keyed by a stable lexically-sortable string so
/// BTreeMap iteration yields chronological output without an extra sort.
/// What: returns `YYYY-W{:02}` from the timestamp's ISO week.
/// Test: exercised by every aggregator test (all weekly buckets use this).
fn iso_week_label(ts: &DateTime<Utc>) -> String {
    let iso = ts.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}
