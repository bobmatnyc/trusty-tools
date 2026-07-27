//! `tga jira sync` / `tga jira freshness` — JIRA ingestion ownership
//! (issue #3966).
//!
//! TGA is the owner of the complete JIRA ingestion surface for
//! status-transition and comment data, upstream of the cto-reports fact
//! tables (supersedes cto-reports#404/#405). This module wires the
//! [`tga::collect::jira`] HTTP client and [`tga::core::db::jira_facts`]
//! persistence helpers into two CLI subcommands:
//!
//! - `sync` — incremental (default) or full (`--backfill`/`--since`) ingest
//!   of JIRA changelog status transitions and comments into
//!   `fact_ticket_transitions` / `fact_jira_comment_detail`.
//! - `freshness` — the CRITICAL freshness assertion called out in the
//!   issue: fails loudly (non-zero exit) if either fact table is empty or
//!   its most recently *synced* row is older than a threshold.
//!
//! ## Explicitly out of scope for this slice (see PR description / issue
//! comment for the full breakdown)
//!
//! - Wiring an actual weekly cron trigger: this repository does not have an
//!   existing scheduled-workflow entry point for `tga` subcommands (unlike
//!   `e2e-docker.yml`'s nightly `schedule:`, which runs Docker-based E2E
//!   tests, not `tga` itself). Provisioning that is an infra decision
//!   outside this PR's blast radius.
//! - Retiring the Python discard path
//!   (`scripts/extraction/jira/extract_jira_content.py`,
//!   `scripts/migration/load_jira_to_duckdb.py`) — those scripts live in
//!   the separate `cto-reports` repository (not present here) and the
//!   issue explicitly gates their retirement on 1+ week of live-verified
//!   Rust-path data, which cannot exist yet.
//! - The cto-reports bridge consumer side (cto-reports#404/#405).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Args;
use tracing::{info, warn};

use tga::collect::errors::CollectError;
use tga::collect::jira::sync::{
    build_jql, plan_cursor, resolve_scope, validate_project_key, CursorPlan,
};
use tga::collect::jira::{ChangelogIssue, JiraClient};
use tga::core::config::Config;
use tga::core::db::{
    check_freshness, get_cursor, list_cursor_projects, set_cursor, upsert_comment_detail,
    upsert_ticket_transition, CommentDetailRow, Database, FreshnessStatus, TicketTransitionRow,
};

/// Page size for `search_with_changelog` calls within one sync run. Kept
/// well above the client's internal per-request page size (50) so a single
/// call can walk many pages; bounded by `--max-tickets` when the caller
/// wants a smaller run.
const DEFAULT_MAX_TICKETS: usize = 10_000;

/// Arguments for `tga jira sync`.
#[derive(Args, Debug)]
#[command(
    about = "Sync JIRA status transitions and comments into fact_ticket_transitions / fact_jira_comment_detail.",
    long_about = "Fetch JIRA changelog status transitions and full comment history for the\n\
configured (or --project-overridden) project, and persist them into\n\
`fact_ticket_transitions` and `fact_jira_comment_detail` in tga.db.\n\n\
Incremental by default: resumes from the stored `jira_sync_cursor` for the\n\
project. Pass --backfill for a full historical pull (first-ever sync of a\n\
project is always a full pull automatically, even without --backfill).\n\n\
Requires `jira.url` (and `jira.username`/`jira.token`, which may reference\n\
`${ENV_VAR}` placeholders) configured in config.yaml.",
    after_help = "EXAMPLES:\n\
  # Incremental sync using the stored cursor (or full history on first run)\n\
  tga jira sync\n\n\
  # Full historical backfill, ignoring any stored cursor\n\
  tga jira sync --backfill\n\n\
  # Sync only tickets updated on/after a specific date\n\
  tga jira sync --since 2026-01-01\n\n\
  # Preview without writing to the database\n\
  tga jira sync --dry-run\n\n\
TIPS:\n\
  - Run `tga jira freshness` after a sync (or on a schedule) to catch a\n\
    silently-stopped sync before downstream reports serve stale data."
)]
pub struct JiraSyncArgs {
    /// Restrict sync to a single JIRA project key. Overrides `jira.project_key`
    /// in config.yaml.
    #[arg(long, value_name = "KEY")]
    pub project: Option<String>,
    /// Only sync tickets updated on/after this date (ISO8601 YYYY-MM-DD).
    /// Overrides the stored incremental cursor for this run (the cursor is
    /// still advanced afterwards based on what was actually observed).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Full historical backfill: ignore the stored cursor and (unless
    /// --since is also given) sync the entire project history.
    #[arg(long, default_value_t = false)]
    pub backfill: bool,
    /// Cap the number of tickets processed in this run (safety valve for a
    /// first-time backfill against a large project). [default: 10000]
    #[arg(long, value_name = "N")]
    pub max_tickets: Option<usize>,
    /// Fetch from JIRA (including comments, so the counts are real) and
    /// report them without writing to the database or advancing the cursor.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

/// Arguments for `tga jira freshness`.
#[derive(Args, Debug)]
#[command(
    about = "Check freshness of the JIRA-derived fact tables (fails loudly if stale/empty).",
    long_about = "Report row counts and sync recency for `fact_ticket_transitions` and\n\
`fact_jira_comment_detail`. Exits non-zero (unless --report-only) if either\n\
table is empty or has not been written to within --max-age-days.\n\n\
This is the CRITICAL freshness guard from issue #3966: intended to be run\n\
as a health check (e.g. from the same cron slot as `tga jira sync`, or a\n\
separate monitoring job) so a sync that silently stopped running is caught\n\
loudly instead of downstream reports serving stale data with no alarm.",
    after_help = "EXAMPLES:\n\
  # Standard health check: every project with a sync cursor, checked\n\
  # individually (fails the process if ANY project's table is stale)\n\
  tga jira freshness\n\n\
  # Check one project only\n\
  tga jira freshness --project PROJ\n\n\
  # Report only, never fail the process (e.g. informational dashboard use)\n\
  tga jira freshness --report-only --max-age-days 7"
)]
pub struct JiraFreshnessArgs {
    /// Maximum allowed age (days) since a fact table was last written to
    /// before it is considered stale.
    #[arg(long, default_value_t = 2)]
    pub max_age_days: i64,
    /// Always exit 0, even when a table is stale or empty (report-only mode).
    #[arg(long, default_value_t = false)]
    pub report_only: bool,
    /// Check only this JIRA project. Default: check every project that has
    /// a sync cursor and fail if ANY of them is stale.
    #[arg(long, value_name = "KEY")]
    pub project: Option<String>,
}

/// Parse a `YYYY-MM-DD` CLI date into a UTC midnight `DateTime`.
fn parse_cli_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid --since date '{s}' (expected YYYY-MM-DD): {e}"))?;
    let ndt = d
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid time-of-day for date '{s}'"))?;
    Ok(Utc.from_utc_datetime(&ndt))
}

/// Resolve the effective JIRA project key from `--project` or
/// `config.jira.project_key`.
///
/// # Errors
///
/// Returns an error if neither is set — `tga jira sync` cannot run without
/// a project scope.
/// The key is additionally validated against JIRA's key grammar before it
/// can reach JQL interpolation — see
/// [`tga::collect::jira::sync::validate_project_key`].
fn resolve_project_key(config: &Config, cli_project: Option<&str>) -> anyhow::Result<String> {
    let key = match cli_project {
        Some(p) => p.to_string(),
        None => config
            .jira
            .as_ref()
            .and_then(|j| j.project_key.clone())
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                "no JIRA project scope: pass --project <KEY> or set jira.project_key in config.yaml"
            )
            })?,
    };
    validate_project_key(&key).map_err(|e| anyhow::anyhow!(e))?;
    Ok(key)
}

/// Build a [`JiraClient`] from config, surfacing a clear error when
/// `jira.url` is unset (rather than a raw HTTP failure later).
fn build_client(config: &Config) -> anyhow::Result<JiraClient> {
    let jira_config = config
        .jira
        .clone()
        .ok_or_else(|| anyhow::anyhow!("`jira:` section is missing from config.yaml"))?;
    JiraClient::new(&jira_config).map_err(|e| match e {
        CollectError::Config(msg) => anyhow::anyhow!("{msg}"),
        other => anyhow::anyhow!(other),
    })
}

/// Dispatch entry point for `tga jira sync`.
///
/// ## Partial failure is never silent, and never permanent
///
/// A per-ticket comment fetch can still fail after its retry budget (see
/// [`tga::collect::jira::retry`]). When it does, two things happen that did
/// not happen before PR #4067's review round 1:
///
/// 1. The ticket is recorded as failed, and the incremental cursor is
///    clamped so it never rises above the *earliest* failure — the ticket
///    stays inside the next run's `updated >=` window. See
///    [`plan_cursor`] for the full invariant. Previously the cursor advanced
///    to the batch maximum, putting the failed ticket permanently below
///    every future window: its comments were lost forever, silently.
/// 2. The run reports the failure count and returns an error, so the process
///    exits non-zero and a cron schedule surfaces it. Previously it exited 0
///    with a `warn!` nobody reads.
///
/// The ticket's transitions are still persisted, and every write is an
/// upsert, so the re-read on the next run is idempotent.
///
/// # Errors
///
/// Propagates JIRA HTTP/auth failures and database errors, and returns an
/// error when any ticket's comments could not be ingested.
pub async fn run_sync(config: Config, db: &mut Database, args: JiraSyncArgs) -> anyhow::Result<()> {
    let project_key = resolve_project_key(&config, args.project.as_deref())?;
    let client = build_client(&config)?;

    let explicit_since = args.since.as_deref().map(parse_cli_date).transpose()?;
    let stored_cursor = get_cursor(db.connection(), &project_key)?
        .and_then(|c| DateTime::parse_from_rfc3339(&c.last_synced_at).ok())
        .map(|d| d.with_timezone(&Utc));

    let scope = resolve_scope(&project_key, explicit_since, args.backfill, stored_cursor);
    let max_tickets = args.max_tickets.unwrap_or(DEFAULT_MAX_TICKETS);

    info!(
        project = %project_key,
        jql = %build_jql(&scope),
        backfill = args.backfill,
        dry_run = args.dry_run,
        "starting tga jira sync"
    );

    let issues = client.search_with_changelog(&scope, max_tickets).await?;

    let mut tickets_scanned = 0usize;
    let mut transitions_written = 0usize;
    let mut comments_ingested = 0usize;
    let mut observed_updated: Vec<DateTime<Utc>> = Vec::new();
    // Ticket keys whose comments could not be ingested, and their `updated`
    // timestamps — the inputs the cursor clamp is computed from.
    let mut failed_tickets: Vec<String> = Vec::new();
    let mut failed_updated: Vec<Option<DateTime<Utc>>> = Vec::new();

    for issue in &issues {
        tickets_scanned += 1;
        if let Some(u) = issue.updated {
            observed_updated.push(u);
        }

        if !args.dry_run {
            write_transitions(db, issue)?;
        }
        transitions_written += issue.transitions.len();

        // Comments are fetched even in dry-run: the flag suppresses writes,
        // not reads, and its whole purpose is to report the counts a real
        // run would produce. Skipping the fetch made it structurally report
        // `0 comment(s)` for every preview.
        match client.fetch_comments(&issue.key).await {
            Ok(comments) => {
                comments_ingested += comments.len();
                if args.dry_run {
                    continue;
                }
                for c in &comments {
                    let row = CommentDetailRow {
                        ticket_key: issue.key.clone(),
                        comment_id: c.id.clone(),
                        project_key: issue.project_key.clone(),
                        author: c.author.clone(),
                        created_at: c.created.to_rfc3339(),
                        body_len: c.body_len,
                    };
                    upsert_comment_detail(db.connection(), &row)?;
                }
            }
            Err(e) => {
                warn!(
                    ticket = %issue.key,
                    error = %e,
                    "failed to fetch comments for this ticket; holding the sync \
                     cursor at or below it so the next run re-fetches it"
                );
                failed_tickets.push(issue.key.clone());
                failed_updated.push(issue.updated);
            }
        }
    }

    if !args.dry_run {
        match plan_cursor(&observed_updated, &failed_updated) {
            CursorPlan::Advance(next) => set_cursor(
                db.connection(),
                &project_key,
                &next.to_rfc3339(),
                tickets_scanned as i64,
            )?,
            CursorPlan::Hold => info!(
                project = %project_key,
                tickets_scanned,
                failures = failed_tickets.len(),
                "cursor left unchanged (no usable `updated` timestamps, or a \
                 failed ticket could not be placed on the timeline)"
            ),
        }
    }

    println!(
        "JIRA sync ({project_key}): {tickets_scanned} ticket(s) scanned, \
         {transitions_written} transition(s), {comments_ingested} comment(s), \
         {} failed ticket(s){}.",
        failed_tickets.len(),
        if args.dry_run {
            " [dry-run: no writes]"
        } else {
            ""
        }
    );

    if !failed_tickets.is_empty() {
        anyhow::bail!(
            "JIRA sync ({project_key}) could not ingest comments for {} of {} ticket(s): {}. \
             The sync cursor was held at or below the earliest failure, so the next run \
             re-fetches them — but this run's data is incomplete and must not be treated \
             as a successful sync.",
            failed_tickets.len(),
            tickets_scanned,
            summarize_keys(&failed_tickets)
        );
    }
    Ok(())
}

/// Render a failed-ticket list for an error message, capped so a mass
/// failure does not produce an unreadable wall of keys.
fn summarize_keys(keys: &[String]) -> String {
    const MAX_LISTED: usize = 10;
    if keys.len() <= MAX_LISTED {
        return keys.join(", ");
    }
    format!(
        "{}, … and {} more",
        keys[..MAX_LISTED].join(", "),
        keys.len() - MAX_LISTED
    )
}

/// Persist every transition on one [`ChangelogIssue`] via a single
/// transaction so a mid-loop failure cannot leave a ticket's transitions
/// half-written.
fn write_transitions(db: &mut Database, issue: &ChangelogIssue) -> anyhow::Result<()> {
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    for t in &issue.transitions {
        let row = TicketTransitionRow {
            ticket_key: issue.key.clone(),
            project_key: issue.project_key.clone(),
            from_status: t.from_status.clone(),
            to_status: t.to_status.clone(),
            transitioned_at: t.created.to_rfc3339(),
            author: t.author.clone(),
        };
        upsert_ticket_transition(&tx, &row)?;
    }
    tx.commit()?;
    Ok(())
}

/// Dispatch entry point for `tga jira freshness`.
///
/// ## Scoping
///
/// `--project KEY` checks one project. With no `--project`, every project
/// carrying a sync cursor is checked *individually* and the command fails if
/// any one of them is stale. That default is deliberate: the freshness
/// aggregate is table-wide, so on a multi-project install one healthy
/// project's writes keep `MAX(synced_at)` recent and mask another project's
/// dead sync entirely — the guard would print OK for an ingestion path that
/// has not run in weeks. Enumerating the cursors makes the guard correct by
/// default instead of correct only when invoked carefully.
///
/// A database with no cursors at all (nothing has ever synced) falls back to
/// the unscoped, all-projects check, which correctly reports the empty
/// tables as stale.
///
/// # Errors
///
/// Returns an error (non-zero exit) if any checked scope is stale/empty and
/// `--report-only` was not passed, or if the underlying DB query fails.
pub fn run_freshness(db: &Database, args: JiraFreshnessArgs) -> anyhow::Result<()> {
    let scopes: Vec<Option<String>> = match &args.project {
        Some(p) => {
            validate_project_key(p).map_err(|e| anyhow::anyhow!(e))?;
            vec![Some(p.clone())]
        }
        None => {
            let projects = list_cursor_projects(db.connection())?;
            if projects.is_empty() {
                vec![None]
            } else {
                projects.into_iter().map(Some).collect()
            }
        }
    };

    let mut statuses: Vec<FreshnessStatus> = Vec::new();
    for scope in &scopes {
        statuses.extend(check_freshness(
            db.connection(),
            args.max_age_days,
            scope.as_deref(),
        )?);
    }

    let mut any_stale = false;
    for s in &statuses {
        let age_desc = match s.age_seconds {
            Some(age) => format!("{:.1}d old", age as f64 / 86_400.0),
            None => "no rows".to_string(),
        };
        let verdict = if s.stale { "STALE" } else { "OK" };
        println!(
            "{:<10} {:<28} rows={:<8} last_synced={:<14} [{}]",
            s.project.as_deref().unwrap_or("<all>"),
            s.table,
            s.row_count,
            age_desc,
            verdict
        );
        if s.stale {
            any_stale = true;
        }
    }

    if any_stale {
        let msg = format!(
            "one or more JIRA fact tables are stale or empty (threshold: {} day(s), \
             scopes checked: {}); see rows above",
            args.max_age_days,
            scopes
                .iter()
                .map(|s| s.as_deref().unwrap_or("<all>"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if args.report_only {
            warn!("{msg}");
            return Ok(());
        }
        anyhow::bail!(msg);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
