//! The supervisor's periodic post-merge cleanup sweep (#7275, owner scope
//! amendment 2026-09-09).
//!
//! Why: `tm pr merge` cleans up after the merge it performs, but a PR merged
//! any other way — the GitHub UI, `--auto` firing hours later, another
//! contributor — leaves its worktree, branches and session claim behind with
//! nothing scheduled to notice. The owner's ruling is that the merge is "the
//! most likely determiner of the obsolescence of those items", so the daemon
//! watches the pull requests this harness opened and acts on the transition.
//!
//! What: [`SessionClaims`] is the daemon-side [`ClaimEnder`] — it reads the
//! session store directly and tombstones a record through
//! [`SessionManager::decommission_record_only`], the one path that is
//! record-only by construction and cannot kill a process. [`due`] is the
//! cadence gate the supervisor's tick calls.
//!
//! Test: the sweep's decisions are covered in
//! `core::pr_cleanup::sweep::sweep_tests`; the cadence gate is
//! `pr_cleanup_is_due_only_after_the_interval` in `super::tests`.

use std::path::Path;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::core::pr_cleanup::{
    ClaimEnder, ClaimOwnership, CleanupRegistry, HolderState, RealGh, RealGit, RealLanding,
    auth_backoff, holder_state_of, host_tree_gate, sweep,
};
use crate::session_manager::SessionManager;
use crate::session_manager::worktree_ignored_output::inspect_dirt_with_ignored_output;

/// The session claims the daemon can see and end, read straight from the store.
///
/// Why: the daemon owns the session store, so it answers the claim question
/// without an HTTP hop — and, crucially, ends a claim through the record-only
/// path rather than the full teardown, which would remove the workspace outside
/// cleanup's own dirty gate.
/// What: `claims_on` keeps each record whose `workspace_path` names the same
/// tree (`identifies_same_path`); `end_claim` calls
/// [`SessionManager::decommission_record_only`].
/// Test: `session_claims_match_a_workspace_spelled_through_a_symlink`; the
/// engine-side behaviour is
/// `cleanup_ends_a_session_claim_before_removing_the_worktree`.
pub struct SessionClaims<'a> {
    /// The store the claims are read from and tombstoned in.
    mgr: &'a SessionManager,
}

impl<'a> SessionClaims<'a> {
    /// Bind a claim ender to one session manager.
    pub fn new(mgr: &'a SessionManager) -> Self {
        Self { mgr }
    }
}

#[async_trait::async_trait]
impl ClaimEnder for SessionClaims<'_> {
    async fn claims_on(&self, path: &Path) -> anyhow::Result<Vec<String>> {
        Ok(self
            .mgr
            .list()
            .await
            .into_iter()
            // #8301 critic: a symlinked or `/private`-prefixed spelling is
            // still this tree's claim.
            .filter(|r| {
                r.workspace_path
                    .as_deref()
                    .is_some_and(|w| trusty_common::identifies_same_path(w, path))
            })
            .map(|r| r.id.to_string())
            .collect())
    }

    async fn end_claim(&self, id: &str) -> anyhow::Result<()> {
        let parsed = crate::session_manager::ManagedSessionId(
            id.parse::<uuid::Uuid>()
                .map_err(|e| anyhow::anyhow!("`{id}` is not a managed session id: {e}"))?,
        );
        self.mgr
            .decommission_record_only(&parsed)
            .await
            .map_err(|e| anyhow::anyhow!("cannot tombstone session {id}: {e}"))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ClaimOwnership for SessionClaims<'_> {
    // #8301: the supervisor runs for no session, so only the #7652 evidence
    // that a holder ENDED lets its claim go.
    async fn holder_state(&self, id: &str) -> HolderState {
        let owners = self.mgr.workspace_claims(None).await.owners;
        holder_state_of(owners.session_end(id))
    }

    // #7771: the prune sweep's ownership rule, condition (d) from the store.
    async fn tree_gate(&self, path: &Path) -> Result<(), String> {
        let owners = self.mgr.workspace_claims(None).await.owners;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            host_tree_gate(&path, &|id: &str| owners.session_end(id))
        })
        .await
        .map_err(|e| format!("the ownership gate did not complete: {e}"))?
    }

    // #7771: the lock is judged again at the moment it would be released.
    async fn release_stale_lock(&self, path: &Path) -> Result<(), String> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            crate::session_manager::worktree_owner_gate::release_stale_lock(&path)
        })
        .await
        .map_err(|e| format!("the lock judgement did not complete: {e}"))?
    }
}

/// Run the cleanup sweep when its own cadence says it is due.
///
/// Why: the fleet sweep runs every 30 seconds by default and the cleanup sweep
/// costs a `gh` call per pending pull request, so the two cadences have to be
/// independent. Keeping the gate here — rather than a second timer — means the
/// sweep can never run concurrently with itself.
/// What: with `interval` `None` the sweep is off. Otherwise it runs when `last`
/// is `None` (the first tick, so a daemon restart cleans up promptly) or when
/// at least `interval` has elapsed, and returns the new `last` to store.
/// Test: `pr_cleanup_is_due_only_after_the_interval`.
pub fn due(interval: Option<Duration>, last: Option<Instant>, now: Instant) -> bool {
    let Some(interval) = interval else {
        return false;
    };
    match last {
        None => true,
        Some(prev) => now.duration_since(prev) >= interval,
    }
}

/// One cleanup sweep, wired to production `gh`, `git` and session store.
///
/// Why: this is the only place the daemon assembles the real drivers, so the
/// sweep's own logic stays testable against fakes.
/// What: builds a [`RealGh`] with the ambient GitHub identity, runs
/// [`sweep::run_sweep`] over the production registry, and returns how many pull
/// requests it cleaned up. The backoff gate is the process-wide
/// [`auth_backoff::shared`] one (#8058) — the strike count has to outlive a
/// single tick, and `/health` reads that same instance.
/// Test: the loop is `sweep_stamps_only_a_fully_successful_run`; the gate is
/// `sweep_stops_calling_gh_after_repeated_auth_failures`.
pub async fn run_sweep(mgr: &SessionManager) -> usize {
    let (env, unset) = match crate::core::gh_identity::resolve_gh_env(None) {
        Ok(e) => (e.vars().to_vec(), e.unset_vars().to_vec()),
        Err(e) => {
            // No identity overlay is not a reason to skip: `gh` still has the
            // host's own auth, and `gh pr view` is a read.
            warn!(
                "pr cleanup sweep: no GitHub identity overlay resolved ({e}); using ambient auth"
            );
            (Vec::new(), Vec::new())
        }
    };
    let gh = RealGh::new(env, unset);
    let claims = SessionClaims::new(mgr);
    sweep::run_sweep_with(
        &gh,
        &RealGit,
        &claims,
        &claims,
        // #7275: the merged-pull-request matcher decides "landed", never an
        // ahead-of-upstream count.
        &RealLanding,
        // #8534: `git worktree remove` deletes gitignored output; the probe counts it.
        &inspect_dirt_with_ignored_output,
        &CleanupRegistry::production(),
        auth_backoff::shared(),
    )
    .await
}
