//! Database query functions for per-engineer drill-down reports.
//!
//! Why: isolating the SQL layer from the report model and formatters keeps
//! each independently testable with a seeded in-memory SQLite database.
//! What: free functions for effort histograms, PR metrics, commit summaries,
//! category counts, and author lookups.
//! Test: each function has a dedicated unit test in `drilldown::tests`.

use std::collections::HashMap;

use rusqlite::params;

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
/// Test: see `tests::effort_histogram_counts`.
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
pub(super) const P95_MIN_SAMPLE: usize = 20;

/// Cycle-time filter: exclude same-minute merges (< 0.5 h) and stale PRs (> 720 h).
pub(super) const CYCLE_TIME_MIN_HOURS: f64 = 0.5;
pub(super) const CYCLE_TIME_MAX_HOURS: f64 = 720.0;

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

/// Fetch `(id, canonical_name, canonical_email, aliases_json)` for the given canonical email.
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
