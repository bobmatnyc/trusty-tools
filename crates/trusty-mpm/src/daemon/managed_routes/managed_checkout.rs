//! Which tree a directory launch runs in: the managed checkout, always.
//!
//! Why: ADR-0037 decided that a PM session with no explicit worktree request
//! runs on "the project's main checkout". That phrase names the MANAGED
//! checkout — `<workspace-root>/<owner>/<repo>`, managed because tm provisions
//! it for the user in a set location — and never the directory the operator
//! happened to be standing in when they typed `tm`. `spawn_managed_routed`
//! resolved it to the launch directory instead, so a launch from an unmanaged
//! clone ran the session there: no managed checkout was provisioned, and the
//! session inherited whatever configuration that tree already carried. This
//! module is the resolution ADR-0037 always described.
//!
//! **The unmanaged tree is never written to.** Redirecting a launch does not
//! clone over, modify, or migrate the directory the operator launched from —
//! that tree is simply not where the session runs. There is no migration path
//! here because there is no migration.
//!
//! This module also owns [`deny_worktree_fallback`], the other half of the same
//! rule: a placement the operator asked for explicitly is never quietly traded
//! for a different one.
//!
//! Test: `managed_checkout_tests`.

use std::path::{Path, PathBuf};

use tracing::{info, warn};
use trusty_common::github_path::GithubPath;

use crate::session_manager::ManagedSessionId;

/// The managed checkout for a project identity.
///
/// Why: routes through `inproject::base_clone_path` rather than computing the
/// join here, so the redirect target and the base clone the in-project spawn
/// path establishes can never drift onto two different directories.
/// What: `<workspace-root>/<owner>/<repo>`.
/// Test: `managed_checkout_is_the_base_clone_path`.
pub(super) fn managed_checkout_for(gh: &GithubPath) -> PathBuf {
    super::inproject::base_clone_path(&gh.owner, &gh.repo)
}

/// Resolve where a directory launch runs, provisioning the managed checkout
/// when it is absent.
///
/// Why: the one place ADR-0037's "the project's main checkout" is turned into a
/// directory. Keeping it in one function is what stops the daemon spawn path and
/// any future caller from resolving the same phrase two different ways.
///
/// What: compares `launch_dir` to the managed checkout built from `gh`. Equal —
/// the launch is already managed, so it runs there unchanged. Not equal — the
/// session runs in the managed checkout instead, cloned first via
/// `inproject::ensure_base_clone` when it does not yet exist. The comparison is
/// a plain path equality because tm constructs the managed path itself from
/// workspace-root + owner + repo, so both sides are deterministic.
///
/// **A provisioning failure is an error, never a fallback.** The session must
/// not start in the unmanaged tree because the clone failed — that is a failure
/// reported as success, and it is how a session ends up running somewhere the
/// control plane never established control over.
/// Test: `managed_launch_dir_is_left_alone`,
/// `unmanaged_launch_dir_redirects_to_managed`,
/// `provisioning_failure_is_an_error_not_a_fallback`.
pub(super) fn resolve_placement(
    launch_dir: &Path,
    gh: &GithubPath,
    origin_url: &str,
) -> Result<PathBuf, String> {
    resolve_placement_at(launch_dir, &managed_checkout_for(gh), origin_url)
}

/// [`resolve_placement`] against a managed checkout the caller already holds.
///
/// Why: `tm launch` decides placement in-process, without the daemon, and its
/// `managed_workspace::provision` had the same defect this module fixes — it
/// ran the session at `find_git_root(cwd)`. That call site is already handed
/// `inproject::base_clone_path(owner, repo)` as `base_path`, so it needs the
/// RULE, not the derivation. Two implementations of one rule is how the CLI and
/// the daemon end up disagreeing about where a session runs, which is what
/// `provision_for_launch`'s own doc warns against.
/// What: see [`resolve_placement`]. `managed` is trusted to be
/// `<workspace-root>/<owner>/<repo>` — this function does not re-derive it.
/// Test: same cases as [`resolve_placement`], which delegates here.
pub fn resolve_placement_at(
    launch_dir: &Path,
    managed: &Path,
    origin_url: &str,
) -> Result<PathBuf, String> {
    let managed = managed.to_path_buf();
    if launch_dir == managed {
        return Ok(managed);
    }

    // ADR-0037: the launch directory is not the managed checkout, so it is not
    // where the session runs. The unmanaged tree is left exactly as it is.
    info!(
        launch_dir = %launch_dir.display(),
        managed = %managed.display(),
        "spawn_managed: launch directory is not the managed checkout — switching to \
         the managed checkout (provisioning it if absent); the launch directory is \
         not modified"
    );

    super::inproject::ensure_base_clone(origin_url, &managed).map_err(|e| {
        format!(
            "managed checkout could not be established at {}: {e} — refusing to start a \
             session in the unmanaged launch directory {}",
            managed.display(),
            launch_dir.display()
        )
    })?;

    Ok(managed)
}

/// Turn a failed in-project spawn into an error when a worktree was REQUESTED,
/// and into the historical warn-and-fall-back only when it was not.
///
/// Why: `spawn_managed_routed` handled both in-project failures — base-clone
/// establishment and worktree reservation — by logging and continuing into
/// `spawn_managed_local`, which spawns a session with no worktree at all. A
/// launch that asked for an isolated worktree then reported success while
/// running somewhere the operator never chose, and the only trace was a `warn!`
/// nobody reads. An explicit request that cannot be honoured is a failure, and
/// the caller has to see it as one.
///
/// What: `worktree_requested == true` returns `Err`, naming the session and the
/// underlying cause, so the caller's `?` aborts the spawn. `false` keeps the
/// pre-existing behaviour exactly — `warn!` and `Ok(())`, letting the caller
/// fall through to the local-path branch. That branch is still correct there:
/// with no worktree asked for and no usable in-project base, `spawn_managed_local`
/// provisions its own managed clone.
/// Test: `deny_worktree_fallback_errors_when_a_worktree_was_requested`,
/// `deny_worktree_fallback_permits_the_fallback_when_none_was_requested`, and
/// the end-to-end `tests/worktree_request_fail_closed.rs`.
pub(super) fn deny_worktree_fallback(
    session_id: &ManagedSessionId,
    worktree_requested: bool,
    err: &str,
) -> Result<(), String> {
    if !worktree_requested {
        warn!(id = %session_id, "in-project spawn: {err}; falling back to local-path spawn");
        return Ok(());
    }
    Err(format!(
        "spawn failed for session {session_id}: this launch explicitly requested a worktree \
         and one could not be established: {err}. Refusing to fall back to a spawn without a \
         worktree — an explicit placement request that cannot be honoured is a failure, not a \
         reason to run somewhere else."
    ))
}

#[cfg(test)]
#[path = "managed_checkout_tests.rs"]
mod tests;
