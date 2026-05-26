//! `tga dora` — compute the four DORA metrics and (re)materialise the
//! `deployment_failures` join.
//!
//! Reads from:
//!   * `fact_deployments` (populated by `tga deployments collect`)
//!   * `fact_incidents`   (populated by `tga incidents collect`)
//!   * `commits` / `classifications` (the analysis DB)
//!
//! Writes to:
//!   * `deployment_failures` — derived join used by Change Failure Rate
//!     and Mean Time To Recovery. The join is rebuilt from scratch on
//!     every `tga dora` invocation so the failure-signal config can
//!     change without manual cleanup.
//!
//! Prints to stdout:
//!   * Deployment Frequency (per-repo weekly count)
//!   * Lead Time for Changes (mean hours, commit → production deploy)
//!   * Change Failure Rate (overall %)
//!   * Mean Time To Recovery (mean hours per incident)

use clap::Args;
use regex::Regex;
use rusqlite::params;
use tracing::info;

use tga::core::config::{Config, FailureSignal};
use tga::core::db::Database;

/// Arguments for `tga dora`.
#[derive(Args, Debug)]
pub struct DoraArgs {
    /// Limit metrics to events on or after this ISO8601 date.
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
}

/// Dispatch entry point.
///
/// # Errors
///
/// Propagates DB / regex errors from the underlying analysis.
pub fn run(config: Config, db: &mut Database, args: DoraArgs) -> anyhow::Result<()> {
    rebuild_deployment_failures(db, &config)?;
    print_metrics(db, args.since.as_deref())?;
    Ok(())
}

/// Reconstruct the `deployment_failures` table from current data.
///
/// Why: the failure-signal config (issue #208) can change between runs;
/// keeping `deployment_failures` purely derived means a config edit
/// always produces a consistent CFR/MTTR without manual SQL cleanup.
/// What: deletes all rows, then for every deploy in `fact_deployments`
/// finds the first commit after `triggered_at` whose classification
/// (or message regex) matches a signal within that signal's window;
/// inserts one failure row per match.
/// Test: covered by `rebuild_deployment_failures_*` integration test.
fn rebuild_deployment_failures(db: &mut Database, config: &Config) -> anyhow::Result<usize> {
    let signals: Vec<FailureSignal> = config
        .dora
        .as_ref()
        .map(|d| d.failure_signals.clone())
        .unwrap_or_default();
    if signals.is_empty() {
        info!("No dora.failure_signals configured — leaving deployment_failures empty.");
        let conn = db.connection_mut();
        conn.execute("DELETE FROM deployment_failures", [])?;
        return Ok(0);
    }

    // Pre-compile any message-pattern regexes once.
    let signals_compiled: Vec<(FailureSignal, Option<Regex>)> = signals
        .into_iter()
        .map(|s| {
            let re = s
                .commit_message_pattern
                .as_ref()
                .and_then(|p| Regex::new(p).ok());
            (s, re)
        })
        .collect();

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM deployment_failures", [])?;

    let mut count = 0usize;
    {
        // Pull every deploy ordered by trigger time.
        let mut deploys = tx.prepare(
            "SELECT deploy_id, repo, triggered_at \
             FROM fact_deployments \
             WHERE environment = 'production' AND status = 'success'",
        )?;
        let mut commits = tx.prepare(
            "SELECT c.sha, c.message, c.timestamp, cl.category \
             FROM commits c \
             LEFT JOIN classifications cl ON cl.id = c.classification_id \
             WHERE c.repository = ?1 \
               AND c.timestamp > ?2 \
               AND c.timestamp <= ?3 \
             ORDER BY c.timestamp ASC LIMIT 1",
        )?;
        let mut insert = tx.prepare(
            "INSERT INTO deployment_failures \
             (deploy_id, failure_commit_sha, detected_at) \
             VALUES (?1, ?2, ?3)",
        )?;

        let deploy_rows = deploys.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for d in deploy_rows {
            let (deploy_id, repo, triggered_at) = d?;
            for (signal, re) in &signals_compiled {
                let window_end = window_end_iso(&triggered_at, signal.within_hours);
                let mut rows = commits.query(params![repo, triggered_at, window_end])?;
                while let Some(row) = rows.next()? {
                    let sha: String = row.get(0)?;
                    let msg: String = row.get(1)?;
                    let detected_at: String = row.get(2)?;
                    let cat: Option<String> = row.get(3)?;
                    if signal_matches(signal, re.as_ref(), &msg, cat.as_deref()) {
                        insert.execute(params![deploy_id, sha, detected_at])?;
                        count += 1;
                        break;
                    }
                }
            }
        }
    }
    tx.commit()?;
    info!(failures = count, "rebuilt deployment_failures from signals");
    Ok(count)
}

/// `triggered_at + within_hours` as an RFC3339 string, computed in SQL
/// via `datetime(?, '+N hours')`. We compute it in Rust to keep the
/// commits query parameterised on a string.
fn window_end_iso(triggered_at: &str, hours: u32) -> String {
    use chrono::{DateTime, Duration, Utc};
    let parsed: DateTime<Utc> = DateTime::parse_from_rfc3339(triggered_at)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    (parsed + Duration::hours(i64::from(hours))).to_rfc3339()
}

/// Decide whether a `(message, category)` pair matches a failure signal.
fn signal_matches(
    signal: &FailureSignal,
    pattern: Option<&Regex>,
    message: &str,
    category: Option<&str>,
) -> bool {
    if let Some(wt) = &signal.work_type {
        let cat_ok = category.is_some_and(|c| c.eq_ignore_ascii_case(wt));
        if !cat_ok {
            return false;
        }
    }
    if let Some(re) = pattern {
        if !re.is_match(message) {
            return false;
        }
    } else if signal.commit_message_pattern.is_some() {
        // Pattern configured but failed to compile — refuse to match
        // so a bad regex never silently widens the failure set.
        return false;
    }
    // Both filters absent OR all configured filters passed.
    true
}

/// Render the four DORA metrics to stdout.
fn print_metrics(db: &Database, since: Option<&str>) -> anyhow::Result<()> {
    let since_pred = since.map(|s| format!(" AND triggered_at >= '{s}'"));
    let since_clause = since_pred.as_deref().unwrap_or("");

    // 1. Deployment Frequency (count + per-week average)
    let (total_deploys, weeks_active): (i64, i64) = db
        .connection()
        .query_row(
            &format!(
                "SELECT COUNT(*), COUNT(DISTINCT strftime('%Y-W%W', triggered_at)) \
                 FROM fact_deployments \
                 WHERE environment = 'production' AND status = 'success'{since_clause}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let per_week = if weeks_active == 0 {
        0.0
    } else {
        (total_deploys as f64) / (weeks_active as f64)
    };
    println!("Deployment Frequency");
    println!(
        "  Total production deploys : {total_deploys} \
         (across {weeks_active} active week(s), ~{per_week:.2}/week)"
    );

    // 2. Lead Time for Changes
    let lead_time_hours: Option<f64> = db
        .connection()
        .query_row("SELECT AVG(lead_time_hours) FROM v_lead_time", [], |r| {
            r.get(0)
        })
        .ok()
        .flatten();
    println!("\nLead Time for Changes");
    match lead_time_hours {
        Some(h) => println!("  Mean hours (commit → deploy): {h:.2}"),
        None => println!("  (no commits joined to deploys via git_sha)"),
    }

    // 3. Change Failure Rate
    let (cfr_total, cfr_failed): (i64, i64) = db
        .connection()
        .query_row(
            &format!(
                "SELECT COUNT(DISTINCT d.deploy_id), COUNT(DISTINCT df.deploy_id) \
                 FROM fact_deployments d \
                 LEFT JOIN deployment_failures df ON df.deploy_id = d.deploy_id \
                 WHERE d.environment = 'production'{since_clause}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let cfr = if cfr_total == 0 {
        0.0
    } else {
        (cfr_failed as f64) / (cfr_total as f64)
    };
    println!("\nChange Failure Rate");
    println!(
        "  {} failure(s) across {} deploy(s) → {:.1}% CFR",
        cfr_failed,
        cfr_total,
        cfr * 100.0,
    );

    // 4. Mean Time To Recovery
    let mttr_hours: Option<f64> = db
        .connection()
        .query_row("SELECT AVG(mttr_hours) FROM v_mttr", [], |r| r.get(0))
        .ok()
        .flatten();
    println!("\nMean Time To Recovery");
    match mttr_hours {
        Some(h) => println!("  Mean hours (incident detected → resolved): {h:.2}"),
        None => println!("  (no incidents with both detected_at and resolved_at)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the failure-signal matcher is the single decision point for
    /// CFR; regressions here would either under- or over-count failures.
    /// What: probe each branch (work-type only, pattern only, both,
    /// neither).
    /// Test: pure-function table.
    #[test]
    fn signal_matches_branches_individually() {
        let work_type_only = FailureSignal {
            work_type: Some("bug_fix".into()),
            ..Default::default()
        };
        assert!(signal_matches(
            &work_type_only,
            None,
            "any",
            Some("bug_fix")
        ));
        assert!(!signal_matches(
            &work_type_only,
            None,
            "any",
            Some("feature")
        ));

        let pat_only = FailureSignal {
            commit_message_pattern: Some(r"(?i)hotfix".into()),
            ..Default::default()
        };
        let re = Regex::new(r"(?i)hotfix").unwrap();
        assert!(signal_matches(&pat_only, Some(&re), "Hotfix prod", None));
        assert!(!signal_matches(&pat_only, Some(&re), "feat: thing", None));

        let combined = FailureSignal {
            work_type: Some("bug_fix".into()),
            commit_message_pattern: Some(r"(?i)hotfix".into()),
            ..Default::default()
        };
        let re = Regex::new(r"(?i)hotfix").unwrap();
        assert!(signal_matches(
            &combined,
            Some(&re),
            "Hotfix x",
            Some("bug_fix")
        ));
        assert!(!signal_matches(
            &combined,
            Some(&re),
            "Hotfix x",
            Some("feature")
        ));

        let empty = FailureSignal::default();
        // No filters configured → match everything.
        assert!(signal_matches(&empty, None, "anything", None));
    }

    /// Why: empty `failure_signals` must yield zero failures and not
    /// error (e.g. a fresh install with no dora config block).
    /// What: open an empty DB and call rebuild; assert zero rows.
    /// Test: smoke-level integration.
    #[test]
    fn rebuild_deployment_failures_with_no_signals_is_a_clean_noop() {
        let mut db = Database::open_in_memory().expect("db");
        let n = rebuild_deployment_failures(&mut db, &Config::default()).expect("rebuild");
        assert_eq!(n, 0);
    }
}
