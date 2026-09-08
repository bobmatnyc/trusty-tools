//! `tga linear sync` / `tga linear freshness` — Linear bulk team ingestion
//! (issue #7139).
//!
//! Why: `linear.fetch_on_reference` (`collect::linear_pipeline`) only ever
//! resolves issues a commit message names, so an engagement registered with
//! `[boards.linear]` and no JIRA board had no path to a team's full issue set
//! and no ticket-linked metrics. This module is the Linear counterpart to
//! `commands::jira` (issue #3966): a paginated bulk sync scoped to one team
//! key, and a freshness check reading the same `linear_sync_cursor`
//! bookkeeping the sync writes.
//!
//! ## Differences from `tga jira sync`, deliberately
//!
//! JIRA's sync walks tickets one at a time (a changelog fetch, then a
//! separate comment fetch per ticket), so a single bad ticket needs
//! isolation — the per-ticket circuit breaker and cursor clamp in
//! `commands::jira::run_sync`. Linear's `issues` query is already a bulk
//! paginated read: one page IS the unit of work, and a page either succeeds
//! or the whole run fails and propagates the error before any cursor
//! decision is made. There is no per-issue partial-failure state to isolate,
//! so this module carries none of that machinery.
//!
//! ## Scope
//!
//! `--team` overrides; otherwise the sync requires exactly one configured
//! `linear.team_keys` entry (multiple configured teams need an explicit
//! `--team` per run, mirroring JIRA's single `project_key`).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Args;
use std::collections::HashMap;

use tga::collect::errors::CollectError;
use tga::collect::linear::sync::{next_cursor, resolve_scope, validate_team_key};
use tga::collect::linear::LinearClient;
use tga::collect::linear_pipeline::persist_work_items;
use tga::core::config::Config;
use tga::core::db::{get_linear_cursor, list_linear_cursor_teams, set_linear_cursor, Database};

/// Safety valve for a first-time backfill against a large team, mirroring
/// JIRA's `DEFAULT_MAX_TICKETS`.
const DEFAULT_MAX_ISSUES: usize = 10_000;

/// Arguments for `tga linear sync`.
#[derive(Args, Debug, Default)]
#[command(
    about = "Bulk-sync a Linear team's full issue set into linear_issues / work_items.",
    long_about = "Fetch every issue for the configured (or --team-overridden) Linear team\n\
and persist it into `linear_issues` (with lifecycle timestamps) and the\n\
source-agnostic `work_items` corpus.\n\n\
Incremental by default: resumes from the stored `linear_sync_cursor` for the\n\
team. Pass --backfill for a full historical pull (first-ever sync of a team\n\
is always a full pull automatically, even without --backfill).\n\n\
Requires `linear.api_key` (or a shared-credential fallback) configured, and\n\
either --team or exactly one entry in `linear.team_keys`.",
    after_help = "EXAMPLES:\n\
  # Incremental sync using the stored cursor (or full history on first run)\n\
  tga linear sync --team ENG\n\n\
  # Full historical backfill, ignoring any stored cursor\n\
  tga linear sync --team ENG --backfill\n\n\
  # Preview without writing to the database\n\
  tga linear sync --team ENG --dry-run"
)]
pub struct LinearSyncArgs {
    /// Restrict sync to a single Linear team key. Overrides
    /// `linear.team_keys` in config.yaml.
    #[arg(long, value_name = "KEY")]
    pub team: Option<String>,
    /// Only sync issues updated on/after this date (ISO8601 YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Full historical backfill: ignore the stored cursor and (unless
    /// --since is also given) sync the entire team history.
    #[arg(long, default_value_t = false)]
    pub backfill: bool,
    /// Cap the number of issues processed in this run (safety valve for a
    /// first-time backfill against a large team). [default: 10000]
    #[arg(long, value_name = "N")]
    pub max_issues: Option<usize>,
    /// Fetch from Linear and report counts without writing to the database
    /// or advancing the cursor.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

/// Arguments for `tga linear freshness`.
#[derive(Args, Debug)]
#[command(
    about = "Check freshness of the Linear bulk-sync cursor (fails loudly if stale/never run).",
    long_about = "Report the last successful `tga linear sync` run per team, reading\n\
`linear_sync_cursor.last_run_at` — deliberately NOT `linear_issues.fetched_at`,\n\
which is also written by the unrelated per-commit-reference lookup and so\n\
cannot tell \"the bulk sync ran\" apart from \"a commit happened to mention a\n\
ticket\". Exits non-zero (unless --report-only) if any checked team has never\n\
synced or is older than --max-age-days.",
    after_help = "EXAMPLES:\n\
  # Every team with a sync cursor, plus every configured team_keys entry\n\
  tga linear freshness\n\n\
  # Check one team only\n\
  tga linear freshness --team ENG\n\n\
  # Report only, never fail the process\n\
  tga linear freshness --report-only --max-age-days 7"
)]
pub struct LinearFreshnessArgs {
    /// Maximum allowed age (days) since the last successful sync before a
    /// team is considered stale.
    #[arg(long, default_value_t = 2)]
    pub max_age_days: i64,
    /// Always exit 0, even when a team is stale or has never synced.
    #[arg(long, default_value_t = false)]
    pub report_only: bool,
    /// Check only this Linear team. Default: every team with a sync cursor,
    /// plus every configured `linear.team_keys` entry.
    #[arg(long, value_name = "KEY")]
    pub team: Option<String>,
}

fn parse_cli_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid --since date '{s}' (expected YYYY-MM-DD): {e}"))?;
    let ndt = d
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid time-of-day for date '{s}'"))?;
    Ok(Utc.from_utc_datetime(&ndt))
}

/// Resolve the effective Linear team key from `--team` or a single
/// `linear.team_keys` entry.
///
/// # Errors
///
/// Returns an error when neither yields exactly one team: `tga linear sync`
/// needs one unambiguous scope per run, the same way `tga jira sync` needs
/// one `project_key`.
fn resolve_team_key(config: &Config, cli_team: Option<&str>) -> anyhow::Result<String> {
    let key = match cli_team {
        Some(t) => t.to_string(),
        None => {
            let configured = config
                .linear
                .as_ref()
                .map(|l| l.team_keys.clone())
                .unwrap_or_default();
            match configured.as_slice() {
                [one] => one.clone(),
                [] => anyhow::bail!(
                    "no Linear team scope: pass --team <KEY> or set exactly one entry in \
                     linear.team_keys in config.yaml"
                ),
                many => anyhow::bail!(
                    "ambiguous Linear team scope: linear.team_keys has {} entries ({}); \
                     pass --team <KEY> to pick one",
                    many.len(),
                    many.join(", ")
                ),
            }
        }
    };
    validate_team_key(&key).map_err(|e| anyhow::anyhow!(e))?;
    Ok(key)
}

fn build_client(config: &Config) -> anyhow::Result<LinearClient> {
    let linear_config = config
        .linear
        .clone()
        .ok_or_else(|| anyhow::anyhow!("`linear:` section is missing from config.yaml"))?;
    LinearClient::new(&linear_config).map_err(|e| match e {
        CollectError::Config(msg) => anyhow::anyhow!("{msg}"),
        other => anyhow::anyhow!(other),
    })
}

/// Dispatch entry point for `tga linear sync`.
///
/// # Errors
///
/// Propagates Linear HTTP/auth failures and database errors.
pub async fn run_sync(
    config: Config,
    db: &mut Database,
    args: LinearSyncArgs,
) -> anyhow::Result<()> {
    let team_key = resolve_team_key(&config, args.team.as_deref())?;
    let client = build_client(&config)?;

    let explicit_since = args.since.as_deref().map(parse_cli_date).transpose()?;
    let stored_cursor = get_linear_cursor(db.connection(), &team_key)?
        .and_then(|c| DateTime::parse_from_rfc3339(&c.last_synced_at).ok())
        .map(|d| d.with_timezone(&Utc));

    let scope = resolve_scope(&team_key, explicit_since, args.backfill, stored_cursor);
    let max_issues = args.max_issues.unwrap_or(DEFAULT_MAX_ISSUES);

    tracing::info!(
        team = %team_key,
        since = ?scope.since,
        backfill = args.backfill,
        dry_run = args.dry_run,
        "starting tga linear sync"
    );

    let (issues, truncated) = client
        .fetch_team_issues(&team_key, scope.since, max_issues)
        .await?;
    let issues_synced = issues.len();

    if !args.dry_run {
        client.store_issues(db, &issues)?;
        // #7139: the same issues land in the source-agnostic `work_items`
        // corpus, extending the existing commit-reference linkage
        // (`linear_pipeline::persist_work_items`) to cover backfilled
        // issues too. No commit correlation is attempted here — a bulk sync
        // has no message context — so `commit_refs` is empty; the per-commit
        // path still owns that linkage.
        persist_work_items(db, &issues, &HashMap::new())?;

        if let Some(next) = next_cursor(
            &issues
                .iter()
                .filter_map(|i| i.updated_at)
                .collect::<Vec<_>>(),
        ) {
            let advance = stored_cursor.map_or(next, |s| s.max(next));
            set_linear_cursor(
                db.connection(),
                &team_key,
                &advance.to_rfc3339(),
                issues_synced as i64,
            )?;
        }
    }

    println!(
        "Linear sync ({team_key}): {issues_synced} issue(s) synced{}.",
        if args.dry_run {
            " [dry-run: no writes]"
        } else {
            ""
        }
    );
    if truncated {
        println!(
            "  note: stopped at the --max-issues limit ({max_issues}); more issues match this \
             window. Re-run to continue from the recorded cursor."
        );
    }
    Ok(())
}

/// Dispatch entry point for `tga linear freshness`.
///
/// Scoping mirrors `tga jira freshness`: with no `--team`, every team
/// carrying a sync cursor is checked individually, plus every configured
/// `linear.team_keys` entry — so a team that has *never* synced (no cursor
/// row at all) is still reported, not silently skipped.
///
/// # Errors
///
/// Returns an error (non-zero exit) if any checked team has never synced or
/// is older than `--max-age-days`, unless `--report-only` was passed.
pub fn run_freshness(
    config: &Config,
    db: &Database,
    args: LinearFreshnessArgs,
) -> anyhow::Result<()> {
    let scopes: Vec<String> = match &args.team {
        Some(t) => {
            validate_team_key(t).map_err(|e| anyhow::anyhow!(e))?;
            vec![t.clone()]
        }
        None => {
            let mut teams = list_linear_cursor_teams(db.connection())?;
            if let Some(cfg) = &config.linear {
                for key in &cfg.team_keys {
                    if !teams.contains(key) {
                        teams.push(key.clone());
                    }
                }
            }
            teams.sort();
            teams
        }
    };

    if scopes.is_empty() {
        anyhow::bail!(
            "no Linear team to check: pass --team <KEY> or configure linear.team_keys / run \
             `tga linear sync` at least once"
        );
    }

    let now = Utc::now();
    let mut any_stale = false;
    for team in &scopes {
        let cursor = get_linear_cursor(db.connection(), team)?;
        let (age_desc, stale) = match &cursor {
            Some(c) => match DateTime::parse_from_rfc3339(&c.last_run_at) {
                Ok(parsed) => {
                    let age_days =
                        (now - parsed.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
                    (
                        format!("{age_days:.1}d old"),
                        age_days > args.max_age_days as f64,
                    )
                }
                Err(_) => ("unparseable last_run_at".to_string(), true),
            },
            None => ("never synced".to_string(), true),
        };
        let verdict = if stale { "STALE" } else { "OK" };
        println!(
            "{team:<10} linear_sync_cursor       issues={:<8} last_run={:<20} [{verdict}]",
            cursor.as_ref().map_or(0, |c| c.issues_synced),
            age_desc,
        );
        if stale {
            any_stale = true;
        }
    }

    if any_stale {
        let msg = format!(
            "one or more Linear teams have never synced or are stale (threshold: {} day(s), \
             teams checked: {}); see rows above",
            args.max_age_days,
            scopes.join(", ")
        );
        if args.report_only {
            tracing::warn!("{msg}");
            return Ok(());
        }
        anyhow::bail!(msg);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
