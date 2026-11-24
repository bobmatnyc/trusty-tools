//! `tm pr watch add|ls|rm` — the daemon PR watch's table verbs (#11673 AC1).
//!
//! Why: the daemon's PR watch loop wakes a PM once per CI transition on a
//! tracked PR, and a PM needs a command to start and stop tracking one.
//! What: [`register`] reads the PR's head and base, the base's required
//! contexts (branch protection plus rulesets, `check_gate::read_required`)
//! and the owner (`--owner`, else this pane's tmux session, #11741), then
//! upserts the row. `tm wait --for check --watch` calls the same function.
//! `ls` prints the rows; `rm` drops one. Only this module, `tm wait` and the
//! daemon loop touch the table; the merge and review gates never read it.
//! Test: `watch_tests.rs`.

use std::path::Path;

use trusty_mpm::core::pr_watch::gh::{PR_VIEW_FIELDS, parse_pr_view};
use trusty_mpm::core::pr_watch::table::{Table, Upsert, WatchRow};

use super::{EXIT_OK, GhRunner, argv, check_gate, repo_slug};
use crate::cli::{PrWatchArgs, PrWatchCmd};

/// The `--repo` a watch verb was given, if any.
pub(crate) fn repo_arg(args: &PrWatchArgs) -> Option<&str> {
    match &args.cmd {
        PrWatchCmd::Add(a) => a.repo.as_deref(),
        PrWatchCmd::Rm(a) => a.repo.as_deref(),
        PrWatchCmd::Ls(_) => None,
    }
}

/// Run one `tm pr watch` verb against the production table.
pub(crate) fn run<R: GhRunner>(gh: &R, args: &PrWatchArgs) -> anyhow::Result<i32> {
    let table = Table::production()?;
    let cwd = std::env::current_dir()?;
    match &args.cmd {
        PrWatchCmd::Add(a) => {
            let line = register(gh, &table, a.pr, a.repo.as_deref(), a.owner.clone(), &cwd)?;
            println!("{line}");
        }
        PrWatchCmd::Ls(a) => println!("{}", list(&table, a.json)?),
        PrWatchCmd::Rm(a) => {
            let slug = repo_slug(gh, a.repo.as_deref())?;
            let gone = table.remove(&slug, a.pr)?;
            println!(
                "tm pr watch: {slug}#{} {}",
                a.pr,
                if gone { "removed" } else { "was not watched" }
            );
        }
    }
    Ok(EXIT_OK)
}

/// Add or update the row for PR `pr`; returns the one-line report.
///
/// Why: `tm pr watch add` and `tm wait --for check --watch` must record the
/// same row the same way.
/// What: refuses a merged or closed PR, and an owner that is neither given
/// nor this pane's tmux session. The owner's ledger launch dir is `cwd`'s,
/// when it resolves. The verdict starts unjudged, so a PR already settled
/// wakes its owner once on the daemon's first tick.
///
/// # Errors
/// A `gh` read failed, the PR is done, or no owner is known.
///
/// Test: `register_records_head_required_and_owner`,
/// `register_refuses_a_merged_pr_and_an_unknown_owner`.
pub(crate) fn register<R: GhRunner>(
    gh: &R,
    table: &Table,
    pr: u64,
    repo: Option<&str>,
    owner: Option<String>,
    cwd: &Path,
) -> anyhow::Result<String> {
    register_with(
        gh,
        table,
        pr,
        repo,
        owner,
        cwd,
        trusty_mpm::core::dbatg::own_session,
    )
}

/// [`register`] with the own-session lookup passed in, for tests.
pub(crate) fn register_with<R: GhRunner>(
    gh: &R,
    table: &Table,
    pr: u64,
    repo: Option<&str>,
    owner: Option<String>,
    cwd: &Path,
    own_session: impl FnOnce() -> Option<String>,
) -> anyhow::Result<String> {
    let owner = owner
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty())
        .or_else(own_session)
        .ok_or_else(|| {
            anyhow::anyhow!("no owner session: run inside a tmux pane or pass --owner <session>")
        })?;
    let slug = repo_slug(gh, repo)?;
    let a = argv(&[
        "pr",
        "view",
        &pr.to_string(),
        "--repo",
        &slug,
        "--json",
        PR_VIEW_FIELDS,
    ]);
    let snap = parse_pr_view(&gh.run(&a)?.stdout_ok(&a)?)?;
    anyhow::ensure!(
        !snap.is_done(),
        "{slug}#{pr} is {}; nothing to watch",
        snap.state
    );
    let required = check_gate::read_required(gh, &slug, &snap.base)?.unwrap_or_default();
    let project = trusty_architect::ledger::store::resolve_project(cwd).ok();
    let row = WatchRow {
        repo: slug.clone(),
        pr,
        head: snap.head.clone(),
        base: snap.base,
        required: required.clone(),
        owner: owner.clone(),
        project,
        verdict: None,
        emitted_key: None,
        ledger_key: None,
        last_woken_key: None,
        added_at: chrono::Utc::now().to_rfc3339(),
    };
    let done = match table.upsert(row)? {
        Upsert::Added => "added",
        Upsert::Updated => "updated",
    };
    let required = if required.is_empty() {
        "none (all checks judged)".to_string()
    } else {
        required.join(", ")
    };
    Ok(format!(
        "tm pr watch: {done} {slug}#{pr} head={} owner={owner} required={required}",
        snap.head.chars().take(10).collect::<String>()
    ))
}

/// The rows as lines, or as JSON.
fn list(table: &Table, json: bool) -> anyhow::Result<String> {
    let rows = table.list()?;
    if json {
        return Ok(serde_json::to_string_pretty(&rows)?);
    }
    if rows.is_empty() {
        return Ok("tm pr watch: no PRs watched".to_string());
    }
    Ok(rows
        .iter()
        .map(|r| {
            format!(
                "{}#{} head={} verdict={} owner={} woken={}",
                r.repo,
                r.pr,
                r.head.chars().take(10).collect::<String>(),
                r.verdict.map_or("unjudged", |v| v.as_str()),
                r.owner,
                r.last_woken_key.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
