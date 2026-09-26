//! `tm pr cleanup <n>` — the CLI wrapper around the post-merge cleanup engine
//! (#7275).
//!
//! Why: the engine itself lives in `trusty_mpm::core::pr_cleanup` because three
//! callers share it — this command, `tm pr merge`'s final step, and the
//! daemon's periodic merged-PR sweep. What is CLI-specific is only the wiring:
//! which checkout the git commands run in, and how a session claim is ended
//! from a process that is not the daemon. Both live here and nowhere else.
//!
//! What: [`run`] resolves the main checkout, builds a [`DaemonClaims`] over the
//! running daemon, calls the engine, prints one line per step, and exits 0 only
//! when every step succeeded.
//!
//! Test: the engine's decisions are covered in `core::pr_cleanup::tests`;
//! `cli_parses_pr_cleanup` pins the argv wiring.

use std::path::{Path, PathBuf};

use trusty_mpm::client::DaemonClient;
use trusty_mpm::core::pr_cleanup::{
    ClaimEnder, CleanupRegistry, CleanupRequest, CleanupScope, RealGit, RealLanding,
};
use trusty_mpm::session_manager::worktree_ignored_output::inspect_dirt_with_ignored_output;

use super::{EXIT_BLOCKED, EXIT_OK, RealGhRunner, repo_slug};
use crate::cli::PrCleanupArgs;

/// The session claims cleanup can see and end, over the running daemon.
///
/// Why: the CLI is not the daemon, so it cannot call
/// `SessionManager::decommission_record_only` directly. Routing both halves
/// through [`DaemonClient`] keeps the CLI on the crate's single managed-session
/// HTTP client rather than hand-building requests.
/// What: `claims_on` keeps each managed session whose `workspace_path` names
/// the same tree (`identifies_same_path`); `end_claim` posts the record-only
/// decommission. A daemon that cannot be reached surfaces as an `Err`, which
/// the engine reports as a FAILED worktree step — an unanswerable claim
/// question never advances toward a delete.
/// Test: `daemon_claims_match_a_workspace_spelled_through_a_symlink`; the
/// engine-side behaviour is
/// `cleanup_fails_the_worktree_step_when_claims_cannot_be_read`.
struct DaemonClaims {
    /// The managed-session client, bound to the daemon's base URL.
    client: DaemonClient,
}

#[async_trait::async_trait]
impl ClaimEnder for DaemonClaims {
    async fn claims_on(&self, path: &Path) -> anyhow::Result<Vec<String>> {
        let sessions = self.client.list_managed_sessions().await?;
        // #8301 critic: a symlinked or `/private`-prefixed spelling is still
        // this tree's claim.
        Ok(sessions
            .into_iter()
            .filter(|s| {
                s.workspace_path
                    .as_deref()
                    .is_some_and(|w| trusty_common::identifies_same_path(Path::new(w), path))
            })
            .map(|s| s.id)
            .collect())
    }

    async fn end_claim(&self, id: &str) -> anyhow::Result<()> {
        let outcome = self
            .client
            .decommission_managed_session_record_only(id)
            .await?;
        // A daemon too old to read `record_only` falls through to the full
        // teardown, which removes the workspace. Cleanup removes worktrees
        // itself, under its own dirty gate; it must never let a stale daemon do
        // it unguarded. A daemon that reports nothing is undeterminable, which
        // fails the same way.
        anyhow::ensure!(
            outcome.workspace_removed == Some(false),
            "the daemon did not confirm it only tombstoned session {id}'s record (it reported \
             workspace_removed = {:?}) — it may predate the record-only decommission; restart it \
             before cleaning up",
            outcome.workspace_removed
        );
        Ok(())
    }
}

/// The checkout every cleanup git command runs in.
///
/// Why: cleanup removes worktrees, and `git worktree remove` must be issued
/// from a checkout that is not the one being removed. The main checkout is
/// where `version-control` runs (ADR-0056), so resolving git's own toplevel
/// from the cwd is both correct and the only place the operator can be.
/// What: `git rev-parse --show-toplevel`, through this workspace's single git
/// entry point.
/// Test: exercised live; the engine takes the resolved path as an input.
fn repo_root() -> anyhow::Result<PathBuf> {
    let out = trusty_common::git::command()
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "not inside a git checkout: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    anyhow::ensure!(
        !path.is_empty(),
        "`git rev-parse --show-toplevel` named no directory"
    );
    Ok(PathBuf::from(path))
}

/// Run `tm pr cleanup`.
///
/// Why: the owner's ruling is that this runs as the final step after merge
/// confirmation and is deterministic unless it errors, so it reports every step
/// and its exit code is the whole contract: 0 when all five succeeded,
/// [`EXIT_BLOCKED`] when any did not.
/// What: resolves the checkout, runs the engine with the real `gh`, `git`,
/// daemon-claim and unsaved-work probes, prints the report, and stamps the
/// cleanup registry on a fully successful non-dry run so the daemon's periodic
/// sweep does not repeat it.
/// Test: `cli_parses_pr_cleanup`; engine decisions in `core::pr_cleanup::tests`.
pub(crate) async fn run(
    args: &PrCleanupArgs,
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<i32> {
    run_scoped(args, client, url, false).await
}

/// One engine pass with the production `git`, landing and dirt probes.
async fn run_engine(
    gh: &RealGhRunner,
    claims: &DaemonClaims,
    req: &CleanupRequest,
) -> trusty_mpm::core::pr_cleanup::CleanupReport {
    // #7275: `RealLanding` is what decides a squash-merged branch is landed;
    // `inspect_dirt`'s ahead-of-upstream count no longer refuses on its own.
    // #8534: `git worktree remove` deletes gitignored output; the probe counts it.
    let probe = &inspect_dirt_with_ignored_output;
    trusty_mpm::core::pr_cleanup::run(gh, &RealGit, claims, &RealLanding, probe, req).await
}

/// [`run`], with the #8301 scope choice: `head_only` limits removal to the PR's
/// own head worktree and branch, and prints the plan before removing anything.
async fn run_scoped(
    args: &PrCleanupArgs,
    client: &reqwest::Client,
    url: &str,
    head_only: bool,
) -> anyhow::Result<i32> {
    let gh = RealGhRunner::new()?;
    // Resolved, never left as the caller's `None`: the registry stamp below is
    // keyed by (repo, number), so a run that could not name its repository
    // would clean up and still leave the entry pending for the daemon's sweep.
    let repo = repo_slug(&gh, args.repo.as_deref())?;
    let claims = DaemonClaims {
        client: DaemonClient::with_client(client.clone(), url.to_string()),
    };
    let req = CleanupRequest {
        pr: args.pr,
        repo: Some(repo.clone()),
        repo_root: repo_root()?,
        dry_run: args.dry_run,
        head_only,
    };

    // #8301: a merge-chained cleanup prints its plan before it removes anything.
    if head_only && !req.dry_run {
        let plan_req = CleanupRequest {
            dry_run: true,
            ..req.clone()
        };
        let plan = run_engine(&gh, &claims, &plan_req).await;
        println!("cleanup plan for #{}:\n{}", req.pr, plan.render());
        if plan.failed() {
            return Ok(EXIT_BLOCKED);
        }
    }
    let report = run_engine(&gh, &claims, &req).await;
    println!("{}", report.render());
    if report.failed() {
        return Ok(EXIT_BLOCKED);
    }
    if !req.dry_run {
        // Best-effort: a failed stamp costs one repeated sweep, which reports
        // its own failures, so it must not turn a successful cleanup red.
        if let Err(e) =
            CleanupRegistry::production().mark_cleaned(&repo, req.pr, chrono::Utc::now())
        {
            eprintln!("tm pr cleanup: could not stamp the cleanup registry: {e:#}");
        }
    }
    Ok(EXIT_OK)
}

/// What `tm pr merge` does after `gh` answered the merge (#8301).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostMerge {
    /// The merge did not succeed; exit with this code.
    NotMerged(i32),
    /// Run the head-only cleanup now, against `repo`.
    Cleanup { repo: String },
    /// The operator deferred cleanup; the registry entry says so.
    Deferred,
    /// `--auto`: nothing has merged yet, so the daemon's sweep acts later —
    /// head-only, as recorded.
    AwaitSweep,
}

/// The scope a `tm pr merge` invocation binds the sweep to (#8301).
///
/// What: `--no-cleanup`/`--no-delete-branch` → [`CleanupScope::Deferred`];
/// anything else, `--auto` included, → [`CleanupScope::HeadOnly`], because a
/// merge names exactly one PR.
fn merge_scope(args: &crate::cli::PrMergeArgs) -> CleanupScope {
    if args.no_cleanup || args.no_delete_branch {
        CleanupScope::Deferred
    } else {
        CleanupScope::HeadOnly
    }
}

/// Persist the merge's scope in the cleanup registry (#8301).
///
/// Why: the sweep runs in the daemon, possibly minutes later, so the choice
/// has to be on disk before anything can merge.
/// What: writes [`merge_scope`] for (`repo`, PR) and returns the warnings the
/// caller prints. A failed write is an error that names the registry and how to
/// recover. Two cases warn: a registry an older writer rewrote (checked before
/// this write re-stamps it), and a deferral that matched no entry — nothing for
/// the sweep to skip, but a mismatched repo slug would look the same.
/// Test: `post_merge_no_cleanup_defers_the_registry_entry`,
/// `post_merge_cleanup_records_a_head_only_scope`,
/// `post_merge_auto_leaves_the_entry_to_the_sweep`,
/// `record_merge_scope_warns_on_a_registry_an_older_writer_rewrote`.
pub(crate) fn record_merge_scope(
    args: &crate::cli::PrMergeArgs,
    repo: &str,
    registry: &CleanupRegistry,
) -> anyhow::Result<Vec<String>> {
    let scope = merge_scope(args);
    let path = registry.path().display();
    let mut warnings = Vec::new();
    // #8301: checked BEFORE the write below, which re-stamps the file and
    // would hide the downgrade. The merge still proceeds: this call records
    // the scope, and a later downgrade only restores the pre-#8301 behaviour.
    if registry.rewritten_by_older_writer() {
        let daemon = trusty_mpm::core::daemon_identity::read_lock()
            .map(|l| format!("the running daemon (pid {} at {})", l.pid, l.addr))
            .unwrap_or_else(|| "a daemon or tm older than this one".to_string());
        warnings.push(format!(
            "tm pr merge: {path} was rewritten by {daemon}, which predates recorded cleanup \
             scopes and drops them; run `tm restart` so the daemon matches this tm (#8301)"
        ));
    }
    let hit = registry.record_scope(repo, args.pr, scope).map_err(|e| {
        // #8301: the sweep consequence is stated only when it is true — the
        // sweep can read this entry, and it is still pending wide.
        let sweep = registry
            .entry(repo, args.pr)
            .filter(|e| e.pending() && e.scope == CleanupScope::Wide)
            .map(|_| "; until it is recorded, the daemon's sweep would run the full cleanup")
            .unwrap_or_default();
        anyhow::anyhow!(
            "not merging #{}: its post-merge cleanup choice could not be recorded in {path} \
             ({e:#}). Repair the file or move {path} aside to proceed{sweep}",
            args.pr
        )
    })?;
    // #8301: `Ok(false)` is not silent for a deferral.
    if !hit && scope == CleanupScope::Deferred {
        warnings.push(format!(
            "tm pr merge: no cleanup-registry entry for {repo}#{} in {path}; nothing to defer \
             (the daemon's sweep only visits PRs `tm pr open` recorded)",
            args.pr
        ));
    }
    Ok(warnings)
}

/// The step after a successful merge (#8301).
///
/// Test: `post_merge_no_cleanup_never_reaches_after_merge`.
pub(crate) fn post_merge_step(args: &crate::cli::PrMergeArgs, repo: String) -> PostMerge {
    if merge_scope(args) == CleanupScope::Deferred {
        PostMerge::Deferred
    } else if args.auto {
        PostMerge::AwaitSweep
    } else {
        PostMerge::Cleanup { repo }
    }
}

/// Record the merge's cleanup scope, THEN merge (#8301).
///
/// Why: recording after the merge left a window — and, on a failed write, a
/// permanent state — in which the registry said "wide" for a PR that had
/// merged, and the sweep acted on it.
/// What: resolves `repo`, runs [`record_merge_scope`] and stops with its error
/// before `gh` is asked to merge; then runs [`super::merge::run`] and maps the
/// outcome through [`post_merge_step`].
/// Test: `merge_aborts_before_merging_when_the_scope_cannot_be_recorded`,
/// `merge_aborts_when_the_scope_marker_cannot_be_written`.
pub(crate) fn merge_with_recorded_scope<R: super::GhRunner>(
    gh: &R,
    args: &crate::cli::PrMergeArgs,
    repo: impl FnOnce() -> anyhow::Result<String>,
    registry: &CleanupRegistry,
) -> anyhow::Result<PostMerge> {
    let repo = repo()?;
    for warning in record_merge_scope(args, &repo, registry)? {
        eprintln!("{warning}");
    }
    let code = super::merge::run(gh, args)?;
    Ok(if code == EXIT_OK {
        post_merge_step(args, repo)
    } else {
        PostMerge::NotMerged(code)
    })
}

/// Run cleanup as `tm pr merge`'s final step (#7275, owner amendment).
///
/// Why: the owner's ruling is that cleanup "should be run as the final step
/// after merge confirmation", and a merge is "the most likely determiner of the
/// obsolescence" of the worktree, branches and claim. Chaining it here is what
/// makes the common path need no second command.
/// What: forwards to [`run_scoped`] with the merged PR's number and resolved `repo`,
/// scoped to the PR's own head worktree and branch (#8301) — other trees and
/// branches the merge made obsolete are reported and left for `tm pr cleanup`.
/// The merge itself has already reported success, so a cleanup failure is
/// reported on its own terms and returns its own nonzero code — the merge is
/// not undone and is not re-reported as failed.
/// Test: `cleanup_8301_head_only_leaves_an_unnamed_tree_at_the_head_commit`;
/// `cli_parses_pr_merge` pins the args this forwards.
pub(crate) async fn after_merge(
    args: &crate::cli::PrMergeArgs,
    repo: String,
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<i32> {
    let cleanup = PrCleanupArgs {
        pr: args.pr,
        // #8301: the slug `record_merge_scope` recorded the scope under.
        repo: Some(repo),
        dry_run: false,
    };
    // #8301: a merge names one PR, so its cleanup removes only that PR's tree.
    run_scoped(&cleanup, client, url, true).await
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod cleanup_tests;
