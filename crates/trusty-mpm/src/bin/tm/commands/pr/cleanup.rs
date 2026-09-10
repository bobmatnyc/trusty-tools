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
    ClaimEnder, CleanupRegistry, CleanupRequest, RealGit, RealLanding,
};
use trusty_mpm::session_manager::worktree_safety::inspect_dirt;

use super::{EXIT_BLOCKED, EXIT_OK, RealGhRunner, repo_slug};
use crate::cli::PrCleanupArgs;

/// The session claims cleanup can see and end, over the running daemon.
///
/// Why: the CLI is not the daemon, so it cannot call
/// `SessionManager::decommission_record_only` directly. Routing both halves
/// through [`DaemonClient`] keeps the CLI on the crate's single managed-session
/// HTTP client rather than hand-building requests.
/// What: `claims_on` filters the managed-session list by `workspace_path`;
/// `end_claim` posts the record-only decommission. A daemon that cannot be
/// reached surfaces as an `Err`, which the engine reports as a FAILED worktree
/// step — an unanswerable claim question never advances toward a delete.
/// Test: the engine-side behaviour is
/// `cleanup_fails_the_worktree_step_when_claims_cannot_be_read`.
struct DaemonClaims {
    /// The managed-session client, bound to the daemon's base URL.
    client: DaemonClient,
}

#[async_trait::async_trait]
impl ClaimEnder for DaemonClaims {
    async fn claims_on(&self, path: &Path) -> anyhow::Result<Vec<String>> {
        let sessions = self.client.list_managed_sessions().await?;
        Ok(sessions
            .into_iter()
            .filter(|s| {
                s.workspace_path
                    .as_deref()
                    .is_some_and(|w| Path::new(w) == path)
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
    };

    // #7275: `RealLanding` is what decides a squash-merged branch is landed;
    // `inspect_dirt`'s ahead-of-upstream count no longer refuses on its own.
    let report = trusty_mpm::core::pr_cleanup::run(
        &gh,
        &RealGit,
        &claims,
        &RealLanding,
        &inspect_dirt,
        &req,
    )
    .await;
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

/// Run cleanup as `tm pr merge`'s final step (#7275, owner amendment).
///
/// Why: the owner's ruling is that cleanup "should be run as the final step
/// after merge confirmation", and a merge is "the most likely determiner of the
/// obsolescence" of the worktree, branches and claim. Chaining it here is what
/// makes the common path need no second command.
/// What: forwards to [`run`] with the merged PR's number and `--repo`. The
/// merge itself has already reported success, so a cleanup failure is reported
/// on its own terms and returns its own nonzero code — the merge is not undone
/// and is not re-reported as failed.
/// Test: engine decisions in `core::pr_cleanup::tests`;
/// `cli_parses_pr_merge` pins the args this forwards.
pub(crate) async fn after_merge(
    args: &crate::cli::PrMergeArgs,
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<i32> {
    let cleanup = PrCleanupArgs {
        pr: args.pr,
        repo: args.repo.clone(),
        dry_run: false,
    };
    run(&cleanup, client, url).await
}
