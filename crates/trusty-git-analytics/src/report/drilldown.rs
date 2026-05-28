//! Per-engineer drill-down queries, data model, and report formatters.
//!
//! This module provides:
//! - Raw data-access free functions (effort histogram, PR metrics, commit summary)
//! - [`AuthorDrilldownData`] — the assembled report model
//! - [`format_markdown`] / [`format_json`] — output renderers for `tga author`
//!
//! Each data-access function is independently testable with a seeded in-memory
//! SQLite database.

use std::collections::HashMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::core::db::Database;
use crate::report::errors::{ReportError, Result};

// ─── Effort histogram ────────────────────────────────────────────────────────

/// Per-size commit counts from `fact_commit_effort`.
///
/// Why: the effort histogram shows how an engineer's work is distributed
/// across XS/S/M/L/XL buckets; this struct is the raw query result before
/// formatting.
/// What: holds the five bucket counts, the number of effort-scored commits,
/// and the total commits for the author in the window (including unscored ones)
/// so the formatter can render the "N / M commits scored" coverage fraction.
/// Test: see `tests::effort_histogram_counts` below.
#[derive(Debug, Clone)]
pub struct EffortHistogram {
    /// Bucket → commit count (only buckets with at least one commit present).
    pub histogram: HashMap<String, u32>,
    /// Number of commits that have a row in `fact_commit_effort`.
    pub scored_commits: u64,
    /// Total commits for this author in the window (scored + unscored).
    pub total_commits: u64,
}

/// Query the effort histogram for a single canonical author.
///
/// Why: the join from `fact_commit_effort` through `commits` to `authors` is
/// the only route from effort data to canonical identity; centralising it here
/// avoids duplicating the three-table join across callers.
/// What: groups `fact_commit_effort.size` rows by size for the given
/// canonical email, optionally filtered by a `[since, until)` commit-timestamp
/// window. Returns an [`EffortHistogram`] with scored-count and total-count
/// for coverage reporting. Commits with no effort row are silently excluded
/// from the histogram (counted in `total_commits` but not `scored_commits`).
/// Test: see `tests::effort_histogram_counts` and
/// `tests::effort_histogram_empty_when_no_effort_rows`.
pub fn query_effort_histogram(
    db: &Database,
    email: &str,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<EffortHistogram> {
    let conn = db.connection();

    // Total commits for this author in the window (including unscored).
    let total_commits: u64 = {
        let mut stmt = conn
            .prepare(
                "SELECT COUNT(*) FROM commits c \
                 JOIN authors a ON a.id = c.author_id \
                 WHERE LOWER(a.canonical_email) = LOWER(?1) \
                   AND (?2 IS NULL OR c.timestamp >= ?2) \
                   AND (?3 IS NULL OR c.timestamp <= ?3)",
            )
            .map_err(crate::core::TgaError::from)?;
        stmt.query_row(params![email, since, until], |r| r.get::<_, i64>(0))
            .map_err(crate::core::TgaError::from)? as u64
    };

    // Histogram: only effort-scored commits.
    let mut stmt = conn
        .prepare(
            "SELECT fce.size, COUNT(*) AS cnt \
             FROM fact_commit_effort fce \
             JOIN commits c ON c.sha = fce.sha \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND (?2 IS NULL OR c.timestamp >= ?2) \
               AND (?3 IS NULL OR c.timestamp <= ?3) \
             GROUP BY fce.size \
             ORDER BY CASE fce.size \
               WHEN 'XS' THEN 1 WHEN 'S' THEN 2 WHEN 'M' THEN 3 \
               WHEN 'L'  THEN 4 WHEN 'XL' THEN 5 ELSE 6 END",
        )
        .map_err(crate::core::TgaError::from)?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(crate::core::TgaError::from)?;

    let mut histogram: HashMap<String, u32> = HashMap::new();
    let mut scored_commits: u64 = 0;
    for r in rows {
        let (size, count) = r.map_err(crate::core::TgaError::from)?;
        let count_u32 = count as u32;
        scored_commits += u64::from(count_u32);
        histogram.insert(size, count_u32);
    }

    Ok(EffortHistogram {
        histogram,
        scored_commits,
        total_commits,
    })
}

// ─── PR metrics ──────────────────────────────────────────────────────────────

/// Aggregated PR metrics for a single engineer.
///
/// Why: `tga author` needs to surface PR throughput and cycle-time stats;
/// this struct carries everything computed from `pull_requests` rows matched
/// to the engineer's provider logins.
/// What: total/merged counts plus optional cycle-time statistics (omitted
/// when no merged PRs are present, or when the sample is too small for p95).
/// Test: see `tests::pr_metrics_basic` and `tests::pr_metrics_no_prs`.
#[derive(Debug, Clone)]
pub struct PrMetrics {
    /// Total PRs authored (all states).
    pub total: u64,
    /// Merged PRs.
    pub merged: u64,
    /// Average cycle time (hours) for merged PRs with valid timestamps.
    /// `None` when no merged PRs are present.
    pub avg_cycle_time_hours: Option<f64>,
    /// Median (p50) cycle time (hours). `None` when no merged PRs.
    pub median_cycle_time_hours: Option<f64>,
    /// p95 cycle time (hours). `None` when < 20 merged PRs (spec threshold).
    pub p95_cycle_time_hours: Option<f64>,
}

/// Minimum merged-PR count before p95 is emitted.
const P95_MIN_SAMPLE: usize = 20;

/// Cycle-time filter: exclude same-minute merges (< 0.5 h) and stale PRs (> 720 h).
const CYCLE_TIME_MIN_HOURS: f64 = 0.5;
const CYCLE_TIME_MAX_HOURS: f64 = 720.0;

/// Query PR metrics for an engineer identified by a set of provider logins.
///
/// Why: `pull_requests.author` holds raw provider logins, not canonical emails;
/// the caller must supply the resolved login list (extracted from `authors.aliases`
/// by the command layer) so this query can match across providers.
/// What: counts total and merged PRs, then fetches raw cycle-time durations for
/// merged PRs (filtered to [0.5, 720] hours). Median and p95 are computed in
/// Rust by sorting the duration vector — SQLite has no native MEDIAN aggregate.
/// Test: see `tests::pr_metrics_basic`, `tests::pr_metrics_p95_requires_20_prs`.
pub fn query_pr_metrics(
    db: &Database,
    logins: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> Result<PrMetrics> {
    if logins.is_empty() {
        return Ok(PrMetrics {
            total: 0,
            merged: 0,
            avg_cycle_time_hours: None,
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        });
    }

    let conn = db.connection();

    // Build dynamic IN(...) clause — one placeholder per login.
    let placeholders: String = logins
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 3)) // slots 1,2 are since/until
        .collect::<Vec<_>>()
        .join(", ");

    // Total + merged counts.
    let count_sql = format!(
        "SELECT COUNT(*), COUNT(CASE WHEN state = 'merged' THEN 1 END) \
         FROM pull_requests \
         WHERE author IN ({placeholders}) \
           AND (?1 IS NULL OR created_at >= ?1) \
           AND (?2 IS NULL OR created_at <= ?2)"
    );
    let mut count_stmt = conn
        .prepare(&count_sql)
        .map_err(crate::core::TgaError::from)?;

    let (total, merged): (u64, u64) = {
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(since.map(str::to_string)),
            Box::new(until.map(str::to_string)),
        ];
        for login in logins {
            params_vec.push(Box::new(login.clone()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|b| b.as_ref()).collect();
        count_stmt
            .query_row(params_refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })
            .map_err(crate::core::TgaError::from)?
    };

    if merged == 0 {
        return Ok(PrMetrics {
            total,
            merged,
            avg_cycle_time_hours: None,
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        });
    }

    // Fetch raw cycle-time hours for merged PRs.
    let durations_sql = format!(
        "SELECT (julianday(merged_at) - julianday(created_at)) * 24.0 \
         FROM pull_requests \
         WHERE author IN ({placeholders}) \
           AND state = 'merged' \
           AND merged_at IS NOT NULL \
           AND (?1 IS NULL OR created_at >= ?1) \
           AND (?2 IS NULL OR created_at <= ?2)"
    );
    let mut dur_stmt = conn
        .prepare(&durations_sql)
        .map_err(crate::core::TgaError::from)?;

    let mut durations: Vec<f64> = {
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(since.map(str::to_string)),
            Box::new(until.map(str::to_string)),
        ];
        for login in logins {
            params_vec.push(Box::new(login.clone()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|b| b.as_ref()).collect();
        let rows = dur_stmt
            .query_map(params_refs.as_slice(), |row| row.get::<_, f64>(0))
            .map_err(crate::core::TgaError::from)?;
        let mut v = Vec::new();
        for r in rows {
            let h = r.map_err(crate::core::TgaError::from)?;
            if (CYCLE_TIME_MIN_HOURS..=CYCLE_TIME_MAX_HOURS).contains(&h) {
                v.push(h);
            }
        }
        v
    };

    if durations.is_empty() {
        return Ok(PrMetrics {
            total,
            merged,
            avg_cycle_time_hours: None,
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        });
    }

    durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = durations.len();
    let avg = durations.iter().sum::<f64>() / n as f64;
    let median = durations[n / 2];
    let p95 = if n >= P95_MIN_SAMPLE {
        Some(durations[(n * 95) / 100])
    } else {
        None
    };

    Ok(PrMetrics {
        total,
        merged,
        avg_cycle_time_hours: Some(avg),
        median_cycle_time_hours: Some(median),
        p95_cycle_time_hours: p95,
    })
}

// ─── Commit summary ──────────────────────────────────────────────────────────

/// Basic commit-level summary for a single engineer.
///
/// Why: the drill-down report header needs total commits, repositories touched,
/// first/last commit dates, and ticket coverage — all derived from the `commits`
/// table joined to `authors`.
/// What: runs two queries — one for aggregate counts (total, ticketed, ins,
/// del, first, last timestamp) and one for the distinct repository list.
/// Test: see `tests::commit_summary_basic`.
#[derive(Debug, Clone)]
pub struct CommitSummary {
    /// Total commits in the window.
    pub total_commits: u64,
    /// Commits with `ticketed = 1`.
    pub ticketed_commits: u64,
    /// Distinct repositories touched.
    pub repositories: Vec<String>,
    /// Earliest commit timestamp (ISO 8601), `None` when no commits.
    pub first_commit: Option<String>,
    /// Latest commit timestamp (ISO 8601), `None` when no commits.
    pub last_commit: Option<String>,
    /// Total insertions.
    pub insertions: i64,
    /// Total deletions.
    pub deletions: i64,
}

/// Query a commit-level summary for a single canonical author.
///
/// Why: the drill-down report header (Summary section) needs several commit
/// aggregates in one pass; this function fetches them all from a single SQL
/// query plus a distinct-repository query.
/// What: joins `commits` to `authors` on `author_id`, filters by email and
/// optional date window, returns [`CommitSummary`]. When no commits exist in
/// scope, returns a zero-filled summary with `None` timestamps.
/// Test: see `tests::commit_summary_basic` and `tests::commit_summary_no_commits`.
pub fn query_commit_summary(
    db: &Database,
    email: &str,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<CommitSummary> {
    let conn = db.connection();

    // Aggregate row.
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*), \
                    COUNT(CASE WHEN c.ticketed = 1 THEN 1 END), \
                    MIN(c.timestamp), MAX(c.timestamp), \
                    SUM(c.insertions), SUM(c.deletions) \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND (?2 IS NULL OR c.timestamp >= ?2) \
               AND (?3 IS NULL OR c.timestamp <= ?3)",
        )
        .map_err(crate::core::TgaError::from)?;

    let (total, ticketed, first_commit, last_commit, insertions, deletions) = stmt
        .query_row(params![email, since, until], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            ))
        })
        .map_err(crate::core::TgaError::from)?;

    // Distinct repositories.
    let mut repo_stmt = conn
        .prepare(
            "SELECT DISTINCT c.repository \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND (?2 IS NULL OR c.timestamp >= ?2) \
               AND (?3 IS NULL OR c.timestamp <= ?3) \
             ORDER BY c.repository",
        )
        .map_err(crate::core::TgaError::from)?;

    let repo_rows = repo_stmt
        .query_map(params![email, since, until], |row| row.get::<_, String>(0))
        .map_err(crate::core::TgaError::from)?;

    let mut repositories = Vec::new();
    for r in repo_rows {
        repositories.push(r.map_err(crate::core::TgaError::from)?);
    }

    Ok(CommitSummary {
        total_commits: total,
        ticketed_commits: ticketed,
        repositories,
        first_commit,
        last_commit,
        insertions,
        deletions,
    })
}

/// Extract provider logins from an `authors.aliases` JSON array.
///
/// Why: `pull_requests.author` stores raw provider logins, not canonical
/// emails; to correlate PRs with a canonical identity we need to extract the
/// login entries from the aliases array and supply them to the PR query.
/// What: parses the JSON array, returns entries that do not contain '@'
/// (which distinguishes logins from email aliases). The canonical email
/// itself is never a login, so it is not added automatically.
/// Test: see `tests::extract_logins_from_aliases`.
pub fn extract_provider_logins(aliases_json: &str) -> Vec<String> {
    let aliases: Vec<String> = serde_json::from_str(aliases_json).unwrap_or_default();
    aliases.into_iter().filter(|a| !a.contains('@')).collect()
}

/// Query per-category commit counts for a single canonical author.
///
/// Why: the Category Breakdown section of `tga author` reuses the
/// `classifications` join to show how an engineer's commits are distributed
/// across work types; this query is cheaper than running `Aggregator::build_filtered`.
/// What: joins `commits` → `classifications` → `authors`, groups by category,
/// returns `HashMap<category, count>`. Commits with no classification are excluded.
/// Test: see `tests::category_counts_basic`.
pub fn query_author_categories(
    db: &Database,
    email: &str,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<HashMap<String, usize>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT cl.category, COUNT(*) \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             LEFT JOIN classifications cl ON cl.id = c.classification_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND cl.category IS NOT NULL \
               AND (?2 IS NULL OR c.timestamp >= ?2) \
               AND (?3 IS NULL OR c.timestamp <= ?3) \
             GROUP BY cl.category",
        )
        .map_err(crate::core::TgaError::from)?;

    let rows = stmt
        .query_map(params![email, since, until], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(crate::core::TgaError::from)?;

    let mut map: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let (cat, cnt) = r.map_err(crate::core::TgaError::from)?;
        map.insert(cat, cnt as usize);
    }
    Ok(map)
}

/// Fetch `(id, canonical_name, aliases_json)` for the given canonical email.
///
/// Why: drilldown queries need the aliases JSON (for login extraction) and
/// the id/name for the report header.
/// What: queries `authors` case-insensitively on `canonical_email`; returns
/// `None` when not found.
/// Test: exercised through `query_commit_summary` / `query_effort_histogram`
/// tests via the command integration test.
pub fn lookup_author_for_drilldown(
    db: &Database,
    email: &str,
) -> Result<Option<(i64, String, String, String)>> {
    let conn = db.connection();
    let result: rusqlite::Result<(i64, String, String, String)> = conn.query_row(
        "SELECT id, canonical_name, canonical_email, aliases \
         FROM authors WHERE LOWER(canonical_email) = LOWER(?1) LIMIT 1",
        params![email],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    );
    match result {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(ReportError::Core(crate::core::TgaError::from(e))),
    }
}

// ─── Report model ────────────────────────────────────────────────────────────

/// Fully assembled per-engineer drill-down report.
///
/// Why: formatters (Markdown, JSON) need a single input struct so they can
/// be called independently of the DB; decoupling the data model from both
/// the query layer and the formatters keeps each layer testable in isolation.
/// What: aggregates all drill-down sections — commit summary, effort histogram,
/// PR metrics, category breakdown — plus header metadata for the report.
/// Test: see `tests::format_markdown_contains_headers` and
/// `tests::format_json_parses` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorDrilldownData {
    /// ISO 8601 UTC timestamp at which the report was generated.
    pub generated_at: String,
    /// Canonical email (as stored in `authors.canonical_email`).
    pub email: String,
    /// Canonical display name.
    pub name: String,
    /// Report window.
    pub period: ReportPeriod,
    /// Commit-level aggregate.
    pub commits: CommitSection,
    /// Effort histogram section.
    pub effort: EffortSection,
    /// Pull-request metrics section.
    pub pull_requests: PrSection,
    /// Per-category commit counts.
    pub categories: HashMap<String, usize>,
}

/// Date window for the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPeriod {
    /// Lower bound (ISO 8601 date or timestamp), `None` = all history.
    pub since: Option<String>,
    /// Upper bound (ISO 8601 date or timestamp), `None` = present.
    pub until: Option<String>,
}

/// Commit-level aggregate section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSection {
    /// Total commits in the window.
    pub total: u64,
    /// Commits with `ticketed = 1`.
    pub ticketed: u64,
    /// `ticketed / total`, in `[0.0, 1.0]`. `None` when total == 0.
    pub ticket_coverage: Option<f64>,
    /// Distinct repositories touched.
    pub repositories: Vec<String>,
    /// Earliest commit timestamp (ISO 8601). `None` when total == 0.
    pub first_commit: Option<String>,
    /// Latest commit timestamp (ISO 8601). `None` when total == 0.
    pub last_commit: Option<String>,
    /// Total insertions.
    pub insertions: i64,
    /// Total deletions.
    pub deletions: i64,
}

/// Effort histogram section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffortSection {
    /// Commits that have a row in `fact_commit_effort`.
    pub scored_commits: u64,
    /// Total commits (scored + unscored).
    pub total_commits: u64,
    /// Bucket → commit count (XS/S/M/L/XL).
    pub histogram: HashMap<String, u32>,
}

/// Pull-request metrics section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSection {
    /// Total PRs (all states) matching any of the engineer's provider logins.
    pub total: u64,
    /// Merged PRs.
    pub merged: u64,
    /// Average cycle time (hours). `None` when no merged PRs with valid timestamps.
    pub avg_cycle_time_hours: Option<f64>,
    /// Median (p50) cycle time (hours). `None` when no merged PRs.
    pub median_cycle_time_hours: Option<f64>,
    /// p95 cycle time (hours). `None` when < 20 merged PRs.
    pub p95_cycle_time_hours: Option<f64>,
}

// ─── Formatters ──────────────────────────────────────────────────────────────

/// Render an [`AuthorDrilldownData`] as a Markdown report string.
///
/// Why: `tga author --format markdown` (the default) targets human readers;
/// the output mirrors the structure in spec §6 so automated doc generation
/// can consume it too.
/// What: produces the exact table structure from spec §6, including section
/// headers, the effort histogram with coverage fraction, and the PR metrics
/// table. Cycle-time fields show `—` when `None`; p95 shows `(< 20 PRs)`
/// when omitted due to insufficient sample.
/// Test: see `tests::format_markdown_contains_headers`.
pub fn format_markdown(data: &AuthorDrilldownData) -> String {
    let mut out = String::new();

    // Header.
    let period_str = match (&data.period.since, &data.period.until) {
        (Some(s), Some(u)) => format!("{s} – {u}"),
        (Some(s), None) => format!("{s} – present"),
        (None, Some(u)) => format!("all history – {u}"),
        (None, None) => "all history".to_string(),
    };
    let generated_date = data.generated_at.get(..10).unwrap_or(&data.generated_at);
    out.push_str(&format!(
        "# Engineer Report: {} <{}>\n",
        data.name, data.email
    ));
    out.push_str(&format!(
        "Generated: {generated_date} | Period: {period_str}\n\n"
    ));

    // Summary table.
    out.push_str("## Summary\n");
    out.push_str("| Metric          | Value                     |\n");
    out.push_str("|-----------------|---------------------------|\n");
    out.push_str(&format!(
        "| Total commits   | {:<25} |\n",
        data.commits.total
    ));
    let repos_str = data.commits.repositories.join(", ");
    out.push_str(&format!(
        "| Repositories    | {:<25} |\n",
        if repos_str.is_empty() {
            "—".to_string()
        } else {
            repos_str
        }
    ));
    out.push_str(&format!(
        "| First commit    | {:<25} |\n",
        data.commits
            .first_commit
            .as_deref()
            .and_then(|s| s.get(..10))
            .unwrap_or("—")
    ));
    out.push_str(&format!(
        "| Last commit     | {:<25} |\n",
        data.commits
            .last_commit
            .as_deref()
            .and_then(|s| s.get(..10))
            .unwrap_or("—")
    ));
    let coverage_str = match (data.commits.total, data.commits.ticket_coverage) {
        (0, _) => "no commits in scope".to_string(),
        (total, Some(cov)) => {
            format!(
                "{} / {} ({:.0}%)",
                data.commits.ticketed,
                total,
                cov * 100.0
            )
        }
        (total, None) => format!("{} / {} (0%)", data.commits.ticketed, total),
    };
    out.push_str(&format!("| Ticket coverage | {:<25} |\n", coverage_str));
    out.push('\n');

    // Effort histogram.
    out.push_str(&format!(
        "## Effort Histogram ({} / {} commits scored)\n",
        data.effort.scored_commits, data.effort.total_commits
    ));
    out.push_str("| Size | Count | % scored |\n");
    out.push_str("|------|-------|----------|\n");
    let scored = data.effort.scored_commits as f64;
    for size in &["XS", "S", "M", "L", "XL"] {
        let count = data.effort.histogram.get(*size).copied().unwrap_or(0);
        let pct = if scored > 0.0 {
            format!("{:.0}%", f64::from(count) / scored * 100.0)
        } else {
            "—".to_string()
        };
        out.push_str(&format!("| {:<4} | {:>5} | {:>8} |\n", size, count, pct));
    }
    out.push('\n');

    // PR metrics.
    out.push_str("## Pull Request Metrics\n");
    out.push_str("| Metric             | Value     |\n");
    out.push_str("|--------------------|-----------||\n");
    out.push_str(&format!(
        "| Total PRs          | {:<9} |\n",
        data.pull_requests.total
    ));
    out.push_str(&format!(
        "| Merged PRs         | {:<9} |\n",
        data.pull_requests.merged
    ));
    let fmt_ct = |v: Option<f64>| -> String {
        v.map(|h| format!("{h:.1} h"))
            .unwrap_or_else(|| "—".to_string())
    };
    out.push_str(&format!(
        "| Avg cycle time     | {:<9} |\n",
        fmt_ct(data.pull_requests.avg_cycle_time_hours)
    ));
    out.push_str(&format!(
        "| Median cycle time  | {:<9} |\n",
        fmt_ct(data.pull_requests.median_cycle_time_hours)
    ));
    let p95_str = match data.pull_requests.p95_cycle_time_hours {
        Some(h) => format!("{h:.1} h"),
        None if data.pull_requests.merged < 20 => "(< 20 PRs)".to_string(),
        None => "—".to_string(),
    };
    out.push_str(&format!("| p95 cycle time     | {:<9} |\n", p95_str));

    if data.pull_requests.total == 0 {
        out.push_str(
            "\n> No pull requests found. Ensure provider logins are mapped via \
                      `tga aliases add-login`.\n",
        );
    }
    out.push('\n');

    // Category breakdown.
    if !data.categories.is_empty() {
        out.push_str("## Category Breakdown\n");
        out.push_str("| Category    | Commits | % total |\n");
        out.push_str("|-------------|---------|--------|\n");
        let total_cats: usize = data.categories.values().sum();
        let mut cats: Vec<(&String, &usize)> = data.categories.iter().collect();
        cats.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (cat, count) in cats {
            let pct = if total_cats > 0 {
                format!("{:.0}%", *count as f64 / total_cats as f64 * 100.0)
            } else {
                "—".to_string()
            };
            out.push_str(&format!("| {:<11} | {:>7} | {:>7} |\n", cat, count, pct));
        }
        out.push('\n');
    }

    out
}

/// Render an [`AuthorDrilldownData`] as a JSON string.
///
/// Why: `tga author --format json` targets programmatic consumers (CI
/// dashboards, team tooling) that need structured, machine-readable output.
/// What: serialises the struct to pretty-printed JSON via serde. All
/// `Option<f64>` fields render as JSON `null` when absent.
/// Test: see `tests::format_json_parses`.
pub fn format_json(data: &AuthorDrilldownData) -> crate::report::errors::Result<String> {
    serde_json::to_string_pretty(data).map_err(ReportError::Json)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    fn seed_author(db: &Database, name: &str, email: &str, aliases_json: &str) -> i64 {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) \
                 VALUES (?1, ?2, ?3)",
                params![name, email, aliases_json],
            )
            .expect("insert author");
        db.connection().last_insert_rowid()
    }

    fn seed_commit(db: &Database, sha: &str, author_id: i64, timestamp: &str, ticketed: i64) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, \
                 message, repository, insertions, deletions) \
                 VALUES (?1, ?2, 'n', 'e', ?3, 'm', 'repo-a', 10, 5)",
                params![sha, author_id, timestamp],
            )
            .expect("insert commit");
        if ticketed != 0 {
            db.connection()
                .execute(
                    "UPDATE commits SET ticketed = 1 WHERE sha = ?1",
                    params![sha],
                )
                .expect("set ticketed");
        }
    }

    fn seed_effort(db: &Database, sha: &str, size: &str) {
        db.connection()
            .execute(
                "INSERT INTO fact_commit_effort \
                 (sha, repository, size, score, loc, files, test_loc, tests_factor, computed_at) \
                 VALUES (?1, 'repo-a', ?2, 1.0, 10, 1, 0, 1.0, 0)",
                params![sha, size],
            )
            .expect("insert effort");
    }

    /// Global PR counter so each test gets unique `pr_number` values even when
    /// sharing the same `repository` + `provider` (which would otherwise trip
    /// the UNIQUE constraint added in migration v12).
    static PR_COUNTER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

    fn seed_pr(
        db: &Database,
        author: &str,
        state: &str,
        created_at: &str,
        merged_at: Option<&str>,
    ) {
        let pr_num = PR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        db.connection()
            .execute(
                "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, 'title', ?2, ?3, ?4, ?5, '[]')",
                params![pr_num, author, state, created_at, merged_at],
            )
            .expect("insert pr");
    }

    #[test]
    fn effort_histogram_counts() {
        // Why: verifies the size → count grouping with a mix of bucket sizes.
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
        seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 0);
        seed_commit(&db, "sha2", aid, "2024-01-02T00:00:00Z", 0);
        seed_commit(&db, "sha3", aid, "2024-01-03T00:00:00Z", 0);
        seed_effort(&db, "sha1", "S");
        seed_effort(&db, "sha2", "S");
        seed_effort(&db, "sha3", "L");

        let h = query_effort_histogram(&db, "alice@example.com", None, None).expect("query");
        assert_eq!(h.total_commits, 3);
        assert_eq!(h.scored_commits, 3);
        assert_eq!(h.histogram.get("S").copied(), Some(2));
        assert_eq!(h.histogram.get("L").copied(), Some(1));
    }

    #[test]
    fn effort_histogram_empty_when_no_effort_rows() {
        // Why: if backfill effort has not been run, histogram should show 0 scored.
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
        seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 0);

        let h = query_effort_histogram(&db, "alice@example.com", None, None).expect("query");
        assert_eq!(h.total_commits, 1);
        assert_eq!(h.scored_commits, 0);
        assert!(h.histogram.is_empty());
    }

    #[test]
    fn pr_metrics_basic() {
        // Why: validates total/merged counts and cycle-time computation.
        let db = Database::open_in_memory().expect("open");
        // 2 merged PRs, each ~24h cycle time.
        seed_pr(
            &db,
            "alice-gh",
            "merged",
            "2024-01-01T00:00:00Z",
            Some("2024-01-02T00:00:00Z"),
        );
        seed_pr(
            &db,
            "alice-gh",
            "merged",
            "2024-01-03T00:00:00Z",
            Some("2024-01-04T00:00:00Z"),
        );
        seed_pr(&db, "alice-gh", "open", "2024-01-05T00:00:00Z", None);

        let logins = vec!["alice-gh".to_string()];
        let m = query_pr_metrics(&db, &logins, None, None).expect("query");
        assert_eq!(m.total, 3);
        assert_eq!(m.merged, 2);
        assert!(m.avg_cycle_time_hours.is_some());
        let avg = m.avg_cycle_time_hours.unwrap();
        assert!((avg - 24.0).abs() < 0.01, "avg should be ~24h, got {avg}");
        assert!(m.median_cycle_time_hours.is_some());
        // p95 requires >= 20 merged PRs; should be None here.
        assert!(m.p95_cycle_time_hours.is_none());
    }

    #[test]
    fn pr_metrics_no_prs() {
        // Why: when no logins match, all metrics should be None.
        let db = Database::open_in_memory().expect("open");
        let logins = vec!["nobody".to_string()];
        let m = query_pr_metrics(&db, &logins, None, None).expect("query");
        assert_eq!(m.total, 0);
        assert_eq!(m.merged, 0);
        assert!(m.avg_cycle_time_hours.is_none());
    }

    #[test]
    fn pr_metrics_p95_requires_20_prs() {
        // Why: p95 is a misleading statistic on small samples; gate at 20.
        let db = Database::open_in_memory().expect("open");
        // Seed exactly 20 merged PRs.
        for i in 0..20u32 {
            let created = format!("2024-01-{:02}T00:00:00Z", (i % 28) + 1);
            let merged = format!("2024-01-{:02}T12:00:00Z", (i % 28) + 1);
            seed_pr(&db, "alice-gh", "merged", &created, Some(&merged));
        }
        let logins = vec!["alice-gh".to_string()];
        let m = query_pr_metrics(&db, &logins, None, None).expect("query");
        assert_eq!(m.merged, 20);
        assert!(
            m.p95_cycle_time_hours.is_some(),
            "p95 should appear at n=20"
        );
    }

    #[test]
    fn commit_summary_basic() {
        // Why: validates total, ticketed, repository list, and timestamp fields.
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
        seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 1);
        seed_commit(&db, "sha2", aid, "2024-01-02T00:00:00Z", 0);

        let s = query_commit_summary(&db, "alice@example.com", None, None).expect("query");
        assert_eq!(s.total_commits, 2);
        assert_eq!(s.ticketed_commits, 1);
        assert_eq!(s.repositories, vec!["repo-a"]);
        assert!(s.first_commit.is_some());
        assert!(s.last_commit.is_some());
    }

    #[test]
    fn commit_summary_no_commits() {
        // Why: an author with no commits in scope should return zeros, not panic.
        let db = Database::open_in_memory().expect("open");
        seed_author(&db, "Alice", "alice@example.com", "[]");

        let s = query_commit_summary(&db, "alice@example.com", None, None).expect("query");
        assert_eq!(s.total_commits, 0);
        assert!(s.first_commit.is_none());
        assert!(s.repositories.is_empty());
    }

    #[test]
    fn extract_logins_from_aliases() {
        // Why: the login-extraction logic must skip email aliases and return only logins.
        let json = r#"["alice@example.com","alice-old@example.com","alice-dev","alice-gh"]"#;
        let logins = extract_provider_logins(json);
        assert_eq!(logins, vec!["alice-dev", "alice-gh"]);
    }

    fn make_sample_drilldown() -> AuthorDrilldownData {
        let mut histogram = HashMap::new();
        histogram.insert("XS".to_string(), 5u32);
        histogram.insert("S".to_string(), 10u32);
        histogram.insert("M".to_string(), 3u32);

        let mut categories = HashMap::new();
        categories.insert("feature".to_string(), 8usize);
        categories.insert("bugfix".to_string(), 4usize);

        AuthorDrilldownData {
            generated_at: "2026-05-28T10:00:00Z".to_string(),
            email: "alice@example.com".to_string(),
            name: "Alice Smith".to_string(),
            period: ReportPeriod {
                since: Some("2025-01-01".to_string()),
                until: Some("2026-05-28".to_string()),
            },
            commits: CommitSection {
                total: 18,
                ticketed: 7,
                ticket_coverage: Some(7.0 / 18.0),
                repositories: vec!["acme/api".to_string()],
                first_commit: Some("2025-01-07T09:12:00Z".to_string()),
                last_commit: Some("2026-05-22T16:44:00Z".to_string()),
                insertions: 500,
                deletions: 200,
            },
            effort: EffortSection {
                scored_commits: 18,
                total_commits: 18,
                histogram,
            },
            pull_requests: PrSection {
                total: 10,
                merged: 9,
                avg_cycle_time_hours: Some(14.3),
                median_cycle_time_hours: Some(9.1),
                p95_cycle_time_hours: None,
            },
            categories,
        }
    }

    #[test]
    fn format_markdown_contains_headers() {
        // Why: asserts the mandatory section headers and key values appear in output.
        let data = make_sample_drilldown();
        let md = format_markdown(&data);
        assert!(md.contains("# Engineer Report: Alice Smith <alice@example.com>"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("## Effort Histogram"));
        assert!(md.contains("## Pull Request Metrics"));
        assert!(md.contains("## Category Breakdown"));
        assert!(md.contains("feature"));
        assert!(md.contains("acme/api"));
        assert!(md.contains("18")); // total commits
    }

    #[test]
    fn format_json_parses() {
        // Why: the JSON output must round-trip through serde without loss.
        let data = make_sample_drilldown();
        let json_str = format_json(&data).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid json");
        assert_eq!(parsed["email"].as_str(), Some("alice@example.com"));
        assert_eq!(parsed["commits"]["total"].as_u64(), Some(18));
        assert_eq!(
            parsed["effort"]["histogram"]["XS"].as_u64(),
            Some(5),
            "XS bucket should be 5"
        );
        assert!(
            parsed["pull_requests"]["p95_cycle_time_hours"].is_null(),
            "p95 should be null when None"
        );
    }
}
