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

use crate::core::pr_cleanup::{ClaimEnder, CleanupRegistry, RealGh, RealGit, sweep};
use crate::session_manager::SessionManager;
use crate::session_manager::worktree_safety::inspect_dirt;

/// The session claims the daemon can see and end, read straight from the store.
///
/// Why: the daemon owns the session store, so it answers the claim question
/// without an HTTP hop — and, crucially, ends a claim through the record-only
/// path rather than the full teardown, which would remove the workspace outside
/// cleanup's own dirty gate.
/// What: `claims_on` filters records by `workspace_path`; `end_claim` calls
/// [`SessionManager::decommission_record_only`].
/// Test: the engine-side behaviour is
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
            .filter(|r| r.workspace_path.as_deref() == Some(path))
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
/// requests it cleaned up.
/// Test: the loop is `sweep_stamps_only_a_fully_successful_run`.
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
    sweep::run_sweep(
        &gh,
        &RealGit,
        &claims,
        &inspect_dirt,
        &CleanupRegistry::production(),
    )
    .await
}
