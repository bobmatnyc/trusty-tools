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
//! remediation). Each entry point resolves the decision EXACTLY once and passes
//! the answer down — so the operator-facing notice and the filesystem action can
//! never disagree.
//!
//! The two entry points answer DIFFERENT questions, and #5274 separated them.
//! [`provision_for_launch`] decides SESSION PLACEMENT, which since #5274 is the
//! main checkout unless the operator passed `tm launch --worktree`; it takes
//! that request as a parameter and consults no registry at all. Only
//! [`provision_for_fallback`] still reads
//! [`trusty_mpm::project::worktree_enabled_for_origin_at`], because it is
//! answering #1724's question instead — may bare `tm`, with the daemon down,
//! deploy `CLAUDE.md` / `.mcp.json` / `.claude/` into whatever checkout the
//! operator happens to be standing in? — where the registry's `worktree: false`
//! is the project TELLING it yes. The two paths still compose: the fallback ends
//! by calling `launch()` with its already-provisioned worktree as the cwd and
//! [`LaunchDir::CallerResolved`], which is what stops `provision_for_launch` from
//! resolving that placement a second time and overwriting it.
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
/// carries the checkout root the session will run in — since #5274 the default
/// outcome for `tm launch`, and still the #4300 outcome for a project that
/// registered `worktree: false` on the daemon-unreachable fallback.
/// Test: `managed_workspace_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedWorkspace {
    /// A per-session git worktree under the protected base clone (the default).
    Worktree(PathBuf),
    /// The project's own main checkout — no clone, no worktree (#3455 opt-out).
    MainCheckout(PathBuf),
}

/// Who resolved the directory [`provision_for_launch`] was handed.
///
/// Why: `launch()` serves two kinds of caller and they need opposite treatment.
/// `tm launch` passes the operator's cwd, which is not a placement yet —
/// resolving it against the managed checkout is the whole job. The guided
/// daemon-unreachable fallback and `tm run` pass a directory they ALREADY
/// resolved (a per-session worktree, a managed checkout they just established),
/// and re-resolving it discards their answer. Without this distinction the
/// redirect fired on every caller: the fallback's fresh worktree was created,
/// abandoned, and every daemon-unreachable session collapsed onto the one shared
/// base clone.
/// What: `OperatorCwd` runs ADR-0037's placement rule via
/// `managed_checkout::resolve_placement_at`. `CallerResolved` takes the
/// directory as given and prints no placement notice — every such caller prints
/// its own, naming the workspace it chose.
/// Test: `provision_for_launch_keeps_a_caller_resolved_placement`,
/// `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchDir {
    /// Wherever the operator typed `tm launch` — placement is still to resolve.
    OperatorCwd,
    /// A directory the caller already resolved as this session's placement.
    CallerResolved,
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
/// touched nothing on disk. `main_checkout` is a placement its caller has fully
/// resolved by the time it gets here — [`provision_for_launch`] routes an
/// operator cwd through `managed_checkout::resolve_placement_at` first
/// (ADR-0037's 2026-08-17 terminology clarification), while
/// [`provision_for_fallback`] answers #1724's separate question and resolves its
/// own. With `isolate` it ensures the base clone and adds a UUID-named
/// per-session worktree. Errors are the raw `inproject` strings so each caller
/// can wrap them with its own remediation text.
/// Test: `managed_workspace_tests.rs`.
async fn provision(
    isolate: bool,
    origin_url: &str,
    base_path: &Path,
    main_checkout: &Path,
    session_id: &ManagedSessionId,
) -> Result<ManagedWorkspace, String> {
    if !isolate {
        // `main_checkout` is already resolved — this function never re-resolves
        // it. `provision_for_launch` applies the ADR-0037 rule to an operator cwd
        // before calling here; `provision_for_fallback` resolves its own and says
        // so with `LaunchDir::CallerResolved` when it composes onto `launch()`.
        tracing::info!(
            origin = %origin_url,
            path = %main_checkout.display(),
            "session runs in the main checkout; no base clone, no worktree (#5274)"
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

/// Provision the workspace for `tm launch` (#4300 call site 1, #5274).
///
/// Why: `tm launch` runs the whole provisioning flow in-process and never asks
/// the daemon, so it decides placement itself and must reach the SAME answer
/// the daemon's `spawn_managed_routed` would. Since #5274 that answer is the
/// project's main checkout unless the operator asked otherwise, so this takes
/// the request as a parameter (`tm launch --worktree`) rather than reading the
/// registry: the project's `worktree` flag governs AGENT isolation, and letting
/// it decide session placement here would put the CLI and the daemon back on
/// two different rules.
/// What: resolves the REPO ROOT of `cwd`, prints the notice for whichever branch
/// applies, then delegates to [`provision`] with the caller-resolved `base_path`
/// (`inproject::base_clone_path(owner, repo)`). Errors are mapped to `tm
/// launch`'s wording.
///
/// The root resolution matters and is not incidental: `get_origin_url` succeeds
/// at ANY depth, so `cd repo/src && tm launch` would otherwise deploy `.claude`,
/// the project hooks and the tmux cwd into `repo/src` rather than `repo` —
/// landing tm's furniture exactly where the operator did not ask for it.
/// [`super::guided::find_git_root`] is the SAME helper the daemon-unreachable
/// fallback's classifier uses, so both CLI entry points cannot disagree about
/// which directory the project is. A `cwd` that is not inside a working tree (or
/// a machine with no `git` on PATH) falls back to `cwd` itself — the pre-#4300
/// behaviour, never worse.
///
/// The placement rule applies to an operator's cwd and to nothing else, which is
/// what `launch_dir` selects. The guided daemon-unreachable fallback composes
/// onto this function: it provisions a protected worktree via
/// [`provision_for_fallback`] and calls `launch()` with that worktree as `cwd`,
/// already resolved. Running the rule over it again found `find_git_root` ==
/// the worktree, `worktree != base_path`, and redirected the session into the
/// shared base clone — discarding the worktree and its branch, and putting two
/// concurrent fallback sessions in one tree. Such a caller passes
/// [`LaunchDir::CallerResolved`] and the rule is skipped.
/// Test: `provision_for_launch_without_a_request_uses_the_main_checkout`,
/// `provision_for_launch_from_subdirectory_targets_repo_root`,
/// `provision_for_launch_explicit_request_creates_worktree`,
/// `provision_for_launch_ignores_a_registered_worktree_true_project`,
/// `provision_for_launch_keeps_a_caller_resolved_placement`.
pub(crate) async fn provision_for_launch(
    origin_url: &str,
    base_path: &Path,
    cwd: &Path,
    worktree_requested: bool,
    launch_dir: LaunchDir,
    session_id: &ManagedSessionId,
) -> anyhow::Result<ManagedWorkspace> {
    let launch_root = super::guided::find_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    // ADR-0037 (2026-08-17 clarification): "the project's main checkout" is the
    // MANAGED checkout `<workspace-root>/<owner>/<repo>` — already in hand as
    // `base_path` — not the repo root of wherever `tm` was typed. Shared with the
    // daemon's `spawn_managed_routed` through one function so the CLI and the
    // daemon cannot resolve the same phrase two different ways. Only reached on
    // the no-worktree branch; `--worktree` provisions from `base_path` already.
    if worktree_requested {
        eprintln!(
            "note: uncommitted local changes are not carried into the managed clone. \
             Use `tm connect` if you need to work from the live checkout."
        );
        eprintln!("provisioning managed workspace...");
    } else if launch_dir == LaunchDir::CallerResolved {
        // The caller already named the workspace on the terminal; a second
        // notice here would describe a placement decision that is not being
        // made.
    } else if launch_root != base_path {
        // Name the switch on the terminal BEFORE it happens, so the operator is
        // never surprised about which tree the session opened in — and so a
        // first-run clone is announced rather than looking like a hang.
        eprintln!(
            "tm: {} is not the managed checkout — launching in {} instead \
             (provisioning it if absent); your checkout is not modified",
            launch_root.display(),
            base_path.display()
        );
    } else {
        eprintln!(
            "tm: launching in {} (no worktree) — \
             pass `--worktree` to provision an isolated one instead",
            base_path.display()
        );
    }

    let main_checkout = if worktree_requested || launch_dir == LaunchDir::CallerResolved {
        launch_root
    } else {
        trusty_mpm::daemon::managed_routes::managed_checkout::resolve_placement_at(
            &launch_root,
            base_path,
            origin_url,
        )
        .map_err(|e| anyhow::anyhow!("failed to provision managed workspace: {e}"))?
    };

    provision(
        worktree_requested,
        origin_url,
        base_path,
        &main_checkout,
        session_id,
    )
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
///
/// This function's answer is final because [`super::guided`] hands it to
/// `launch()` as [`LaunchDir::CallerResolved`]. The three tests below stop one
/// call short of that composition, so they cannot see it reversed — the two
/// named last cover the composed path instead.
/// Test: `provision_for_fallback_opted_out_creates_no_clone_and_no_worktree`,
/// `provision_for_fallback_unset_creates_worktree_not_live_checkout`,
/// `provision_for_fallback_other_projects_optout_does_not_leak`,
/// `provision_for_launch_keeps_a_caller_resolved_placement`,
/// `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.
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
