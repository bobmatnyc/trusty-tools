//! `tm watch poll|listen <project>` — autonomous label-routed ticket execution.
//!
//! Why: the existing `tm ticket <issue#>` model executes ONE hand-named issue. A
//! board-watch mode complements it: discover every issue carrying a routing label
//! and dispatch each into the SAME managed-session execution path, so a team can
//! drop the label on an issue and have it picked up autonomously. Routing is by
//! LABEL (default `tm-agent`), not assignee — no bot account exists. `poll` is the
//! one-shot, cron-friendly form; `listen` is a long-running poll loop.
//! What: this module owns the two operator entry points — [`poll`] (discover +
//! dispatch once, then exit) and [`listen`] (poll-loop until Ctrl-C) — plus the
//! repo-coordinate resolver ([`resolve_board_repo`]) and the safety-gate mapping
//! ([`dispatch_mode`]). Discovery lives in `github`, settings precedence in
//! `args`, per-issue dispatch in `dispatch`, and the loop in `listen`.
//!
//! SAFETY: both entry points default to DRY-RUN. They only spawn real work when
//! the operator passes `--execute`. `--dry-run` is also accepted (and is the
//! default), so the safe path is the one you get by doing nothing special; mass
//! execution requires the explicit, hard-to-fat-finger `--execute` opt-in.
//!
//! Test: `resolve_board_repo_*`, `dispatch_mode_*`, and the args/github/dispatch/
//! listen unit tests in `tests.rs`.

pub(crate) mod args;
pub(crate) mod dispatch;
pub(crate) mod github;
pub(crate) mod listen;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use trusty_mpm::runtime::RuntimeKind;

use crate::commands::ticket::runner::{CommandRunner, RealCommandRunner};
use trusty_mpm::core::trusty_tools_config::TrustyToolsConfig;

use args::{RawWatchArgs, ResolvedWatch, resolve};
use dispatch::{DispatchMode, dispatch_issue};
use github::{GhIssueLister, IssueLister};
use listen::run_listen_loop;

/// Resolved clone URL + base ref for a board repository.
///
/// Why: the managed-spawn endpoint needs a clone URL AND a base ref to branch
/// from. For watch the board is named as `owner/repo` (possibly remote, not the
/// current checkout), so the clone URL is synthesised and the default branch is
/// asked of `gh` for that specific repo — hard-coding `main` would be wrong for
/// `master`/`trunk` boards.
/// What: the `https://github.com/<owner>/<repo>` clone URL and the repo's actual
/// default branch name.
/// Test: `resolve_board_repo_threads_default_branch`,
/// `resolve_board_repo_falls_back_to_main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardRepo {
    /// HTTPS clone URL for the board repository.
    pub(crate) clone_url: String,
    /// The board repository's default branch (base ref for new branches).
    pub(crate) default_branch: String,
}

/// Map the CLI safety flags to a [`DispatchMode`], defaulting to dry-run.
///
/// Why: the safety model must be "safe unless explicitly told otherwise". This
/// folds the two flags into one decision in a tested place so neither entry point
/// can accidentally execute: `--execute` is required to spawn; absence of it (or
/// an explicit `--dry-run`) is dry-run. `--dry-run` always wins over `--execute`
/// if both are somehow passed, biasing toward safety.
/// What: returns [`DispatchMode::Execute`] only when `execute && !dry_run`;
/// otherwise [`DispatchMode::DryRun`].
/// Test: `dispatch_mode_defaults_to_dry_run`, `dispatch_mode_execute_opts_in`,
/// `dispatch_mode_dry_run_wins_over_execute`.
pub(crate) fn dispatch_mode(execute: bool, dry_run: bool) -> DispatchMode {
    if execute && !dry_run {
        DispatchMode::Execute
    } else {
        DispatchMode::DryRun
    }
}

/// Resolve a board repo's clone URL + default branch via `gh`.
///
/// Why: watch dispatches into the managed-spawn path, which clones a URL and
/// branches off a base ref. The board may be a different repo than the current
/// checkout, so we synthesise the HTTPS clone URL from `owner/repo` and ask `gh`
/// for THAT repo's default branch (so `master`/`trunk` boards branch correctly).
/// What: builds `https://github.com/<repo>` and runs
/// `gh repo view <repo> --json defaultBranchRef --jq …` through the injected
/// [`CommandRunner`]; falls back to `main` only when gh returns an empty branch.
/// Test: `resolve_board_repo_threads_default_branch`,
/// `resolve_board_repo_falls_back_to_main`.
pub(crate) fn resolve_board_repo<R: CommandRunner>(
    runner: &R,
    repo: &str,
) -> anyhow::Result<BoardRepo> {
    let clone_url = format!("https://github.com/{repo}");
    let out = runner.run(
        "gh",
        &[
            "repo",
            "view",
            repo,
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ],
    )?;
    let default_branch = out.ok_or_stderr("gh repo view")?;
    let default_branch = if default_branch.is_empty() {
        "main".to_string()
    } else {
        default_branch
    };
    Ok(BoardRepo {
        clone_url,
        default_branch,
    })
}

/// `tm watch poll <project>` — discover label-matched issues and dispatch once.
///
/// Why: the one-shot, stateless, cron-friendly entry point: list every issue
/// carrying the routing label, dispatch each (dry-run by default, execute only
/// with `--execute`), then exit. No loop, no state — a scheduler re-runs it.
/// What: loads config, resolves settings (CLI > config > defaults), resolves the
/// board repo coords, lists matched issues via `gh`, and dispatches each through
/// the shared [`dispatch_issue`]. Prints a summary and returns.
/// Test: settings/repo/mode resolution are unit-tested; the gh + HTTP path is
/// exercised manually (and mirrors `tm ticket`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn poll(
    client: &reqwest::Client,
    url: &str,
    raw: RawWatchArgs,
    execute: bool,
    dry_run: bool,
    runtime: RuntimeKind,
) -> anyhow::Result<()> {
    let config = TrustyToolsConfig::load();
    let settings = resolve(&raw, &config.watch.unwrap_or_default())?;
    let mode = dispatch_mode(execute, dry_run);

    let runner = RealCommandRunner;
    let lister = GhIssueLister::new(RealCommandRunner);
    run_poll_once(client, url, &runner, &lister, &settings, mode, runtime).await
}

/// One-shot poll body, generic over the runner + lister seams for testing.
///
/// Why: factoring the body out from [`poll`] (which constructs the real `gh`
/// seams) lets the discover→dispatch flow be driven by fakes in tests without a
/// live `gh`, while the public entry point stays a thin wiring shim.
/// What: resolves the board repo, lists matched issues, dispatches each via
/// [`dispatch_issue`], and prints a count summary.
/// Test: covered indirectly via the dispatch/github unit tests; the dry-run no-op
/// invariant is asserted on the underlying `dispatch_issue`.
async fn run_poll_once<R: CommandRunner, L: IssueLister>(
    client: &reqwest::Client,
    url: &str,
    runner: &R,
    lister: &L,
    settings: &ResolvedWatch,
    mode: DispatchMode,
    runtime: RuntimeKind,
) -> anyhow::Result<()> {
    let board = resolve_board_repo(runner, &settings.repo)?;
    let issues = lister.list(&settings.repo, &settings.label, settings.state)?;
    eprintln!(
        "tm watch poll: {} issue(s) on {} carry label `{}` ({})",
        issues.len(),
        settings.repo,
        settings.label,
        match mode {
            DispatchMode::DryRun => "dry-run",
            DispatchMode::Execute => "EXECUTE",
        }
    );
    let mut dispatched = 0usize;
    for issue in &issues {
        if dispatch_issue(
            client,
            url,
            &board.clone_url,
            &board.default_branch,
            issue,
            mode,
            runtime,
        )
        .await?
        {
            dispatched += 1;
        }
    }
    eprintln!(
        "tm watch poll: done — {dispatched} dispatched, {} considered",
        issues.len()
    );
    Ok(())
}

/// `tm watch listen <project>` — poll the board on an interval until Ctrl-C.
///
/// Why: the long-running entry point that processes newly-matched issues each
/// cycle, de-duplicating already-seen issue numbers so the same issue is not
/// re-dispatched every poll. Defaults to dry-run; `--execute` opts into real
/// spawning.
/// What: loads config, resolves settings, resolves the board repo coords, then
/// hands off to [`run_listen_loop`] (poll → dedup → dispatch → sleep), which
/// traps SIGINT for a clean stop.
/// Test: the dedup core ([`select_new_issues`]) is unit-tested; the loop is
/// exercised manually (it sleeps + traps signals).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn listen(
    client: &reqwest::Client,
    url: &str,
    raw: RawWatchArgs,
    execute: bool,
    dry_run: bool,
    runtime: RuntimeKind,
) -> anyhow::Result<()> {
    let config = TrustyToolsConfig::load();
    let settings = resolve(&raw, &config.watch.unwrap_or_default())?;
    let mode = dispatch_mode(execute, dry_run);

    let runner = RealCommandRunner;
    let board = resolve_board_repo(&runner, &settings.repo)?;
    let lister = GhIssueLister::new(RealCommandRunner);
    run_listen_loop(
        client,
        url,
        &lister,
        &settings,
        &board.clone_url,
        &board.default_branch,
        mode,
        runtime,
    )
    .await
}
