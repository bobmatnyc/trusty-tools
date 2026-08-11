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
use tga::collect::jira::{ChangelogIssue, JiraClient, JiraTransition};
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

/// Consecutive retry-exhausted per-ticket failures that abort the walk
/// (PR #4067 review round 2).
///
/// Covers BOTH per-ticket network reads — the comment fetch and the truncated
/// changelog repair (issue #4084) — because they fail for the same reasons and
/// a parallel breaker for the second would just be a second thing to get
/// wrong.
///
/// Why a circuit breaker at all: these run once per ticket, so a
/// sustained rate-limit makes *every* ticket fail. Without a bound, a
/// `DEFAULT_MAX_TICKETS` run would issue tens of thousands of requests and
/// spend hours in backoff against a server that is explicitly asking us to
/// stop — risking account or IP throttling on the operator's production JIRA,
/// and reporting all 10,000 tickets failed at the end of it.
///
/// Why aborting loses nothing: the cursor is already held at or below the
/// earliest failure, so an early break resumes from exactly the same place a
/// full walk would have. It converts a multi-hour hammer into a fast, loud
/// failure.
///
/// Why 10 rather than 1: an isolated permanently-broken ticket (deleted
/// mid-run, permission-restricted) should not abort an otherwise healthy
/// backfill. Ten in a row is not an isolated ticket.
const MAX_CONSECUTIVE_TICKET_FAILURES: usize = 10;

/// Arguments for `tga jira sync`.
// #5217: `Default` is what lets `audit::run_full_sweep` build this without clap.
#[derive(Args, Debug, Default)]
#[command(
    about = "Sync JIRA status transitions and comments into fact_ticket_transitions / fact_jira_comment_detail.",
    long_about = "Fetch JIRA changelog status transitions and full comment history for the\n\
configured (or --project-overridden) project, and persist them into\n\
`fact_ticket_transitions` and `fact_jira_comment_detail` in tga.db.\n\n\
Incremental by default: resumes from the stored `jira_sync_cursor` for the\n\
project. Pass --backfill for a full historical pull (first-ever sync of a\n\
project is always a full pull automatically, even without --backfill).\n\n\
Requires `jira.url` (and `jira.username`/`jira.token`, which may reference\n\
`${ENV_VAR}` placeholders) configured in config.yaml.\n\n\
JQL date literals carry no timezone and JIRA evaluates them in the querying\n\
account's profile timezone, so the sync window is rendered in that zone. It\n\
is read from `GET /rest/api/3/myself`; set `jira.timezone` (an IANA name such\n\
as `UTC`) to pin it explicitly when that endpoint is not reachable. The sync\n\
refuses to run rather than guessing.",
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
    /// Overrides the stored incremental cursor for this run only: a
    /// successful run never moves the stored cursor backwards, so re-reading
    /// old history cannot rewind an up-to-date cursor. (A run with failed
    /// tickets is the one exception — it holds the cursor at the earliest
    /// failure so the next run re-covers it.)
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
    /// a sync cursor (plus the configured `jira.project_key`) and fail if
    /// ANY of them is stale.
    #[arg(long, value_name = "KEY")]
    pub project: Option<String>,
    /// Also fail when a project's sync cursor is more than this many days
    /// behind — catches a sync that runs on schedule but never catches up
    /// (e.g. truncating at --max-tickets every run). Off by default: a quiet
    /// project legitimately has an old cursor, since the cursor tracks the
    /// newest ticket seen, not the last time the sync looked.
    #[arg(long, value_name = "DAYS")]
    pub max_cursor_lag_days: Option<i64>,
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
/// A ticket's per-ticket network reads — the comment fetch, and the
/// truncated-changelog repair added by issue #4084 — can still fail after
/// their retry budget (see [`tga::collect::jira::retry`]). When one does, two
/// things happen that did not happen before PR #4067's review round 1:
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
/// On a COMMENT failure the ticket's transitions are still persisted, and
/// every write is an upsert, so the re-read on the next run is idempotent.
/// On a CHANGELOG-REPAIR failure they are not: the only copy in hand is the
/// one the server already told us is missing its oldest entries, and a
/// knowingly-short history is indistinguishable from a complete one once it
/// is a row. Partial progress is kept only where it is honest.
///
/// # Errors
///
/// Propagates JIRA HTTP/auth failures and database errors, and returns an
/// error when any ticket could not be fully ingested.
pub async fn run_sync(config: Config, db: &mut Database, args: JiraSyncArgs) -> anyhow::Result<()> {
    let project_key = resolve_project_key(&config, args.project.as_deref())?;
    let client = build_client(&config)?;

    let explicit_since = args.since.as_deref().map(parse_cli_date).transpose()?;
    let stored_cursor = get_cursor(db.connection(), &project_key)?
        .and_then(|c| DateTime::parse_from_rfc3339(&c.last_synced_at).ok())
        .map(|d| d.with_timezone(&Utc));

    let scope = resolve_scope(&project_key, explicit_since, args.backfill, stored_cursor);
    let max_tickets = args.max_tickets.unwrap_or(DEFAULT_MAX_TICKETS);

    // Resolved up front rather than lazily inside the walk so a misconfigured
    // or undiscoverable timezone fails immediately with its remediation hint,
    // before any tickets are read — and so the logged JQL is the query that
    // will actually be sent, not a UTC-shaped approximation of it.
    let tz = client.account_timezone().await?;

    // Rendered up front so a zone that cannot produce a safe bound fails here,
    // with the same "before any tickets are read" guarantee as the timezone
    // resolution above, rather than mid-walk on page 2.
    let logged_jql = build_jql(&scope, tz)?;

    info!(
        project = %project_key,
        jql = %logged_jql,
        timezone = %tz,
        backfill = args.backfill,
        dry_run = args.dry_run,
        "starting tga jira sync"
    );

    let walk = client.search_with_changelog(&scope, max_tickets).await?;

    let mut tickets_scanned = 0usize;
    let mut transitions_written = 0usize;
    let mut comments_ingested = 0usize;
    let mut observed_updated: Vec<DateTime<Utc>> = Vec::new();
    // Ticket keys whose comments could not be ingested, and their `updated`
    // timestamps — the inputs the cursor clamp is computed from.
    let mut failed_tickets: Vec<String> = Vec::new();
    let mut failed_updated: Vec<Option<DateTime<Utc>>> = Vec::new();
    // Circuit breaker: a sustained 429 makes every ticket fail, and walking
    // all 10,000 of them would issue tens of thousands of doomed requests
    // against a server explicitly asking us to stop.
    let mut consecutive_failures = 0usize;
    let mut tripped = false;

    for issue in &walk.issues {
        tickets_scanned += 1;
        if let Some(u) = issue.updated {
            observed_updated.push(u);
        }

        // Repair a truncated embedded changelog HERE rather than inside the
        // search walk (issue #4084, PR #4155 review). Doing it in the walk
        // meant one unreachable ticket aborted everything before a single row
        // was written: no transitions, no comments, no cursor movement, for
        // every ticket in the window, reproducing identically on the next run.
        // Here it lands in the same failure isolation the comment fetch uses,
        // so one bad ticket holds the cursor and the rest still land.
        let repaired = match issue.truncated_history_total {
            Some(expected) => match client.fetch_changelog(&issue.key, Some(expected)).await {
                Ok(full) => Some(full),
                Err(e) => {
                    // Deliberately BEFORE `write_transitions`: the embedded
                    // transitions are known to be missing their oldest
                    // entries, and once written they read exactly like a
                    // complete history. Partial progress is worth keeping
                    // only when it is honest about what it is.
                    warn!(
                        ticket = %issue.key,
                        error = %e,
                        "could not repair this ticket's truncated changelog; skipping its \
                         transitions rather than persisting a knowingly-short history, and \
                         holding the sync cursor at or below it"
                    );
                    record_failure(
                        issue,
                        &mut failed_tickets,
                        &mut failed_updated,
                        &mut consecutive_failures,
                    );
                    if consecutive_failures >= MAX_CONSECUTIVE_TICKET_FAILURES {
                        tripped = true;
                        break;
                    }
                    continue;
                }
            },
            None => None,
        };
        let transitions = repaired.as_deref().unwrap_or(&issue.transitions);

        if !args.dry_run {
            write_transitions(db, issue, transitions)?;
        }
        transitions_written += transitions.len();

        // Comments are fetched even in dry-run: the flag suppresses writes,
        // not reads, and its whole purpose is to report the counts a real
        // run would produce. Skipping the fetch made it structurally report
        // `0 comment(s)` for every preview.
        match client.fetch_comments(&issue.key).await {
            Ok(comments) => {
                consecutive_failures = 0;
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
                record_failure(
                    issue,
                    &mut failed_tickets,
                    &mut failed_updated,
                    &mut consecutive_failures,
                );
                if consecutive_failures >= MAX_CONSECUTIVE_TICKET_FAILURES {
                    tripped = true;
                    break;
                }
            }
        }
    }

    if tripped {
        warn!(
            consecutive_failures,
            tickets_scanned,
            "aborting the walk: too many consecutive per-ticket failures. Continuing \
             would issue thousands more requests against a remote that is already \
             failing every one of them"
        );
    }

    if !args.dry_run {
        match plan_cursor(&observed_updated, &failed_updated, stored_cursor) {
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

    // Two ways this run can be incomplete without being an error. Both used
    // to pass unremarked, which is how a warehouse ends up quietly short of
    // data while every other signal says the sync succeeded.
    if walk.truncated {
        println!(
            "  note: stopped at the --max-tickets limit ({max_tickets}); more tickets match \
             this window. Re-run to continue from the recorded cursor."
        );
        warn!(
            project = %project_key,
            max_tickets,
            "changelog walk truncated at --max-tickets; the window is only partly covered"
        );
    }
    if let Some(minute) = walk.offset_paged_minute {
        println!(
            "  note: {} contained more tickets than one page, so that minute was walked by \
             offset. A ticket edited during that walk could have been missed; re-cover it \
             with `tga jira sync --project {project_key} --since {}` if in doubt.",
            minute.to_rfc3339(),
            minute.date_naive()
        );
        warn!(
            project = %project_key,
            minute = %minute.to_rfc3339(),
            "walked a single minute by offset; see `collect::jira::paging` for the residual"
        );
    }

    if !failed_tickets.is_empty() {
        anyhow::bail!(
            "JIRA sync ({project_key}) could not fully ingest {} of {} ticket(s): {}.{} \
             The sync cursor was held at or below the earliest failure, so the next run \
             re-fetches them — but this run's data is incomplete and must not be treated \
             as a successful sync.",
            failed_tickets.len(),
            tickets_scanned,
            summarize_keys(&failed_tickets),
            if tripped {
                format!(
                    " The walk was ABORTED after {MAX_CONSECUTIVE_TICKET_FAILURES} consecutive \
                     failures rather than continuing through the remaining tickets — the remote \
                     is likely rate-limiting or down, so retry later rather than immediately."
                )
            } else {
                String::new()
            },
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

/// Record one ticket as failed: it joins the cursor clamp's input set and
/// counts toward the circuit breaker.
///
/// Both per-ticket network reads — the truncated-changelog repair and the
/// comment fetch — go through this single function rather than each keeping
/// its own bookkeeping. That is the whole point: the changelog fallback's
/// first cut had no isolation at all and aborted the run, and the fix for
/// that is to REUSE this machinery, not to grow a second copy of it that can
/// drift (PR #4155 review).
///
/// `updated` is pushed even when `None`; [`plan_cursor`] treats a failure it
/// cannot place on the timeline as a reason to hold the cursor entirely,
/// which is the safe reading.
fn record_failure(
    issue: &ChangelogIssue,
    failed_tickets: &mut Vec<String>,
    failed_updated: &mut Vec<Option<DateTime<Utc>>>,
    consecutive_failures: &mut usize,
) {
    failed_tickets.push(issue.key.clone());
    failed_updated.push(issue.updated);
    *consecutive_failures += 1;
}

/// Persist `transitions` for one [`ChangelogIssue`] via a single transaction
/// so a mid-loop failure cannot leave a ticket's transitions half-written.
///
/// The transitions are passed in rather than read off `issue` because a
/// truncated embedded changelog is repaired by a separate walk in `run_sync`;
/// `issue.transitions` may be the knowingly-short copy.
fn write_transitions(
    db: &mut Database,
    issue: &ChangelogIssue,
    transitions: &[JiraTransition],
) -> anyhow::Result<()> {
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    for t in transitions {
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
/// The configured `jira.project_key` is always included in that set, even
/// when it has no cursor row. A project that has *never* completed a sync is
/// the loudest possible version of "the cron was never wired up", and
/// enumerating only `jira_sync_cursor` would make exactly that case
/// invisible — the unscoped fallback fires only when the cursor table is
/// entirely empty.
///
/// A database with no cursors and no configured project falls back to the
/// unscoped, all-projects check, which correctly reports empty tables as
/// stale.
///
/// ## Cursor lag
///
/// The verdict measures `synced_at` — *write* recency, i.e. "did the sync
/// run". That deliberately cannot answer "is the sync keeping up": a run
/// truncated by `--max-tickets` every time writes fresh rows forever while
/// falling further behind. So the report also prints cursor lag
/// (`now − jira_sync_cursor.last_synced_at`). It is informational by default
/// and only fails the check when `--max-cursor-lag-days` is given, because a
/// legitimately quiet project has a legitimately old cursor — the cursor
/// tracks the newest ticket seen, not the last time we looked.
///
/// # Errors
///
/// Returns an error (non-zero exit) if any checked scope is stale/empty (or
/// exceeds an explicit `--max-cursor-lag-days`) and `--report-only` was not
/// passed, or if the underlying DB query fails.
pub fn run_freshness(
    config: &Config,
    db: &Database,
    args: JiraFreshnessArgs,
) -> anyhow::Result<()> {
    let scopes: Vec<Option<String>> = match &args.project {
        Some(p) => {
            validate_project_key(p).map_err(|e| anyhow::anyhow!(e))?;
            vec![Some(p.clone())]
        }
        None => {
            let mut projects = list_cursor_projects(db.connection())?;
            if let Some(configured) = config
                .jira
                .as_ref()
                .and_then(|j| j.project_key.clone())
                .filter(|p| !p.is_empty())
            {
                if !projects.contains(&configured) {
                    projects.push(configured);
                }
            }
            projects.sort();
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

    // Cursor lag: how far behind the ingestion *window* is, as distinct from
    // how recently it wrote. A permanently-truncating sync keeps the verdict
    // above green while this number grows without bound.
    let mut lagging: Vec<String> = Vec::new();
    for project in scopes.iter().flatten() {
        let Some(cursor) = get_cursor(db.connection(), project)? else {
            continue;
        };
        let Ok(parsed) = DateTime::parse_from_rfc3339(&cursor.last_synced_at) else {
            continue;
        };
        let lag_days = (Utc::now() - parsed.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
        let flagged = args
            .max_cursor_lag_days
            .is_some_and(|max| lag_days > max as f64);
        println!(
            "{project:<10} {:<28} cursor={} ({lag_days:.1}d behind){}",
            "jira_sync_cursor",
            cursor.last_synced_at,
            if flagged { " [LAGGING]" } else { "" }
        );
        if flagged {
            lagging.push(project.clone());
        }
    }
    if !lagging.is_empty() {
        any_stale = true;
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
