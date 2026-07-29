//! CLI-side managed-workspace provisioning that honours the #3455 worktree
//! opt-out (#4300).
//!
//! Why: two `tm` entry points create base clones and per-session worktrees
//! entirely IN THE CLI PROCESS — `tm launch` (which never asks the daemon)
//! and the daemon-unreachable guided fallback (which runs precisely because
//! the daemon is down). Both predate #3455's per-project `worktree: false`
//! opt-out and neither consulted it, so the setting was silently conditional
//! on the daemon being up: a project registered with `worktree: false` still
//! got a clone and a worktree from either path. Concentrating the decision
//! and the provisioning here means the opt-out is read once per launch, in
//! the SAME order the daemon uses (`managed_routes::lifecycle`, ahead of
//! `try_inproject_spawn`) — BEFORE `ensure_base_clone`, so an opted-out
//! project creates neither a worktree nor a base clone.
//! What: [`ManagedWorkspace`] names the two outcomes; [`provision`] is the
//! shared dispatcher that acts on an already-resolved decision;
//! [`provision_for_launch`] serves `tm launch` (which has already resolved
//! the base-clone path) and [`provision_for_fallback`] serves the guided
//! fallback (which parses `owner/repo` itself and reports daemon-specific
//! remediation). Each entry point reads the registry EXACTLY once, via
//! [`trusty_mpm::project::worktree_enabled_for_origin_at`], and passes the
//! answer down — so the operator-facing notice and the filesystem action can
//! never disagree.
//! Known redundancy, accepted: the daemon-unreachable fallback ends by
//! calling `launch()`, so on that composed path the registry is read twice —
//! once by [`provision_for_fallback`] and once by [`provision_for_launch`].
//! Collapsing it would mean threading a pre-resolved `ManagedWorkspace`
//! through `launch()`'s signature, a restructure that buys one avoided
//! `read_to_string` of an atomically-published file whose read path takes no
//! lock and fails open to isolation-ON. Both reads see the same bytes; the
//! only operator-visible symptom was a duplicated notice, and that is fixed
//! in [`provision_for_fallback`] directly.
//! Scope note: this deliberately does NOT change #3455's concurrency
//! behaviour. `spawn_managed_on_main` WARNS (never refuses) on a second
//! session against one main checkout, and that stays the rule here — nothing
//! in this module refuses a launch.
//! Test: `managed_workspace_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::daemon::managed_routes::inproject;
use trusty_mpm::session_manager::ManagedSessionId;

/// Where a CLI-provisioned managed session will actually run.
///
/// Why: the two outcomes have materially different lifecycles — a worktree is
/// framework-owned and disposable, the operator's main checkout is neither —
/// so callers are forced to distinguish them rather than receiving a bare
/// `PathBuf` that hides which one they got.
/// What: `Worktree` carries `<base>/.worktrees/<session-id>`; `MainCheckout`
/// carries the operator's own checkout root, returned only when the project is
/// registered with `worktree: false`.
/// Test: `managed_workspace_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedWorkspace {
    /// A per-session git worktree under the protected base clone (the default).
    Worktree(PathBuf),
    /// The project's own main checkout — no clone, no worktree (#3455 opt-out).
    MainCheckout(PathBuf),
}

impl ManagedWorkspace {
    /// The directory the session should run in, whichever variant this is.
    ///
    /// Why: callers that only need the cwd should not have to re-match.
    /// What: the inner path.
    /// Test: `managed_workspace_tests.rs`.
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Worktree(p) | Self::MainCheckout(p) => p,
        }
    }

    /// True when this workspace is a per-session worktree.
    ///
    /// Why: callers tailor operator-facing notices — "the live checkout is
    /// untouched" is a lie when the session IS the live checkout.
    /// What: `matches!(self, Self::Worktree(_))`.
    /// Test: `managed_workspace_tests.rs`.
    pub(crate) fn is_worktree(&self) -> bool {
        matches!(self, Self::Worktree(_))
    }
}

/// Act on an already-resolved isolation decision.
///
/// Why: the opt-out check MUST run before [`inproject::ensure_base_clone`].
/// Checking afterwards would still leave a full base clone on disk for a
/// project that asked for no isolation at all — the wasted-disk complaint
/// #3455 was filed about; the daemon's `spawn_managed_routed` documents the
/// same ordering for the same reason. Taking `isolate` as a parameter (rather
/// than re-reading the registry here) guarantees the notice each caller
/// printed describes the action actually taken.
/// What: `isolate == false` returns [`ManagedWorkspace::MainCheckout`] having
/// touched nothing on disk; otherwise ensures the base clone and adds a
/// UUID-named per-session worktree. Errors are the raw `inproject` strings so
/// each caller can wrap them with its own remediation text.
/// Test: `managed_workspace_tests.rs`.
async fn provision(
    isolate: bool,
    origin_url: &str,
    base_path: &Path,
    main_checkout: &Path,
    session_id: &ManagedSessionId,
) -> Result<ManagedWorkspace, String> {
    if !isolate {
        tracing::info!(
            origin = %origin_url,
            path = %main_checkout.display(),
            "project has worktree isolation disabled (#3455); no base clone, no worktree"
        );
        return Ok(ManagedWorkspace::MainCheckout(main_checkout.to_path_buf()));
    }

    inproject::ensure_base_clone(origin_url, base_path)?;
    // NOTE (#2032): the CLI flows have no `SessionManager` to resolve a
    // semantic tmux name from, so they keep the pre-#2032 UUID-named
    // worktree; only the daemon's `spawn_managed_inproject` uses the
    // semantic-name layout.
    let worktree =
        inproject::create_session_worktree(base_path, &session_id.to_string(), session_id)?;
    Ok(ManagedWorkspace::Worktree(worktree))
}

/// Provision the workspace for `tm launch` (#4300 call site 1).
///
/// Why: `tm launch` runs the whole provisioning flow in-process and never
/// asks the daemon, so it is the CLI path that most visibly ignored the
/// opt-out before this.
/// What: resolves the REPO ROOT of `cwd` and reads the opt-out from the
/// registry at `registry_dir`, prints the notice for whichever branch applies,
/// then delegates to [`provision`] with the caller-resolved `base_path`
/// (`inproject::base_clone_path(owner, repo)`). Errors are mapped to `tm
/// launch`'s wording.
///
/// The root resolution matters and is not incidental: `get_origin_url`
/// succeeds at ANY depth, so `cd repo/src && tm launch` on an opted-out
/// project would otherwise deploy `.claude`, the project hooks and the tmux
/// cwd into `repo/src` rather than `repo` — landing tm's furniture exactly
/// where the operator did not ask for it, which is the failure mode the
/// opt-out exists to prevent. [`super::guided::find_git_root`] is the SAME
/// helper the daemon-unreachable fallback's classifier uses, so both CLI
/// entry points cannot disagree about which directory the project is. A `cwd`
/// that is not inside a working tree (or a machine with no `git` on PATH)
/// falls back to `cwd` itself — the pre-#4300 behaviour, never worse.
/// Test: `provision_for_launch_opted_out_creates_no_clone_and_no_worktree`,
/// `provision_for_launch_from_subdirectory_targets_repo_root`,
/// `provision_for_launch_unset_creates_worktree`,
/// `provision_for_launch_unregistered_origin_creates_worktree`.
pub(crate) async fn provision_for_launch(
    registry_dir: &Path,
    origin_url: &str,
    base_path: &Path,
    cwd: &Path,
    session_id: &ManagedSessionId,
) -> anyhow::Result<ManagedWorkspace> {
    let main_checkout = super::guided::find_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let isolate =
        trusty_mpm::project::worktree_enabled_for_origin_at(registry_dir, origin_url).await;
    if isolate {
        eprintln!(
            "note: uncommitted local changes are not carried into the managed clone. \
             Use `tm connect` if you need to work from the live checkout."
        );
        eprintln!("provisioning managed workspace...");
    } else {
        eprintln!(
            "tm: worktree isolation is disabled for this project (#3455) — \
             launching directly in {} (no managed clone, no worktree)",
            main_checkout.display()
        );
    }

    provision(isolate, origin_url, base_path, &main_checkout, session_id)
        .await
        .map_err(|e| anyhow::anyhow!("failed to provision managed workspace: {e}"))
}

/// Provision the workspace for the daemon-unreachable guided fallback
/// (#4300 call site 2).
///
/// Why: bare `tm` with the daemon down must still never write framework files
/// into a live checkout it was not told it could (#1724) — but a project
/// registered with `worktree: false` HAS told us exactly that, and the daemon
/// would have honoured it via `spawn_managed_on_main`. Consulting the same
/// registry here is what makes the setting independent of daemon liveness.
/// What: parses `owner/repo` from `origin_url`, reads the opt-out, and
/// delegates to [`provision`] with `git_root` (the repo ROOT, not the
/// possibly-nested cwd) as the opt-out target; errors carry the fallback's
/// "start the daemon" remediation.
/// Test: `provision_for_fallback_opted_out_creates_no_clone_and_no_worktree`,
/// `provision_for_fallback_unset_creates_worktree_not_live_checkout`,
/// `provision_for_fallback_other_projects_optout_does_not_leak`.
pub(crate) async fn provision_for_fallback(
    registry_dir: &Path,
    origin_url: &str,
    git_root: &Path,
    session_id: &ManagedSessionId,
) -> anyhow::Result<ManagedWorkspace> {
    let Some(gh) = trusty_common::github_path::parse_github_path(origin_url) else {
        eprintln!(
            "tm: cannot determine GitHub project from remote URL '{origin_url}'.\n\
             Start the daemon first with `tm start`, then run `tm` again."
        );
        anyhow::bail!(
            "daemon unreachable: cannot parse GitHub remote URL as owner/repo — run `tm start` first"
        );
    };

    // #4300: ask BEFORE naming (let alone creating) the base clone, so an
    // opted-out project never sees a clone directory appear.
    let isolate =
        trusty_mpm::project::worktree_enabled_for_origin_at(registry_dir, origin_url).await;
    let base = inproject::base_clone_path(&gh.owner, &gh.repo);
    if isolate {
        eprintln!(
            "tm: daemon unreachable — redirecting to protected managed clone\n\
             tm: base clone: {}",
            base.display()
        );
    } else {
        // Deliberately terse: this path ends in `launch()`, which calls
        // `provision_for_launch` and prints the full "worktree isolation is
        // disabled … no managed clone, no worktree" line itself. Repeating it
        // here showed the operator the same sentence twice. What is NOT
        // redundant is the daemon-unreachable context, which only this path
        // knows.
        eprintln!(
            "tm: daemon unreachable — this project opted out of worktrees (#3455), \
             so there is nothing to redirect to."
        );
    }

    let workspace = match provision(isolate, origin_url, &base, git_root, session_id).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "tm: could not set up managed workspace for {}/{}: {e}\n\
                 Start the daemon first with `tm start`, then run `tm` again.",
                gh.owner, gh.repo
            );
            anyhow::bail!("failed to set up managed workspace: {e}");
        }
    };

    if workspace.is_worktree() {
        eprintln!(
            "tm: launching in protected workspace (live checkout at {} is untouched)\n\
             tm: session worktree: {}",
            git_root.display(),
            workspace.path().display()
        );
    }
    Ok(workspace)
}

#[cfg(test)]
#[path = "managed_workspace_tests.rs"]
mod tests;
