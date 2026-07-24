//! `#3455` "launch on main" opt-out — spawn a managed session directly in a
//! project's main checkout, with NO per-session git worktree.
//!
//! Why: extracted from `lifecycle.rs` (which is grandfathered at a frozen
//! SLOC budget on `.line-cap-allowlist.tsv`) to keep the opt-out's three
//! pieces — the registry lookup that decides whether isolation is on, the
//! pure concurrent-collision detector, and the `spawn_managed_on_main` flow —
//! in one small sibling module, matching the existing `inproject.rs` split.
//! The functions are `pub(super)` so `lifecycle::spawn_managed_routed` (the
//! only caller) can route to them; the shared spawn helpers they reuse
//! (`write_task_md`/`prepare_inproject_session`/`front_gate_or_escalate`/
//! `resolve_gh_env`) stay owned by `lifecycle` and are called back into.
//! What: [`worktree_enabled_for_origin`] (registry-keyed isolation decision),
//! [`has_concurrent_main_checkout_session`] (pure collision detector), and
//! [`spawn_managed_on_main`] (the no-worktree spawn flow).
//! Test: `worktree_enabled_for_origin_*`,
//! `spawn_managed_on_main_creates_record_without_worktree`, and
//! `spawn_managed_on_main_warns_on_concurrent_main_checkout_session` in
//! `lifecycle_tests.rs` (this module's tests live with `lifecycle`'s, since
//! the opt-out is a branch of the same spawn surface).

use std::sync::Arc;

use tracing::{info, warn};

use super::deployment_check::ensure_deployment_complete;
use super::lifecycle::{
    SpawnParams, front_gate_or_escalate, prepare_inproject_session, resolve_gh_env, write_task_md,
};
use crate::daemon::state::DaemonState;
use crate::project::ProjectRegistry;
use crate::runtime::RuntimeKind;
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Resolve whether per-session worktree isolation is enabled for the
/// registered project whose `repo_url` matches `origin` (#3455).
///
/// Why: mirrors `core::gh_account::find_pinned_gh_account` — the registry
/// (not the static `config.yaml`, which only ever SEEDS the registry via
/// `seed_from_config`) is the source of truth consulted at spawn time, keyed
/// by `repo_url` identity so lookup works regardless of the registry's
/// `name` key or how the project got registered (explicit `tm projects
/// register`, config-seeded, or auto-registered from session history).
/// What: `true` (worktree isolation ON, the default — no regression) when no
/// registered project matches `origin`, the registry is unreachable, or the
/// matched project's `worktree` field is unset/`Some(true)`; `false` only
/// when a matched project has `worktree == Some(false)`.
/// Test: `worktree_enabled_for_origin_defaults_true_when_unregistered`,
/// `worktree_enabled_for_origin_honors_registered_false`.
pub(super) async fn worktree_enabled_for_origin(registry: &ProjectRegistry, origin: &str) -> bool {
    let Ok(projects) = registry.list().await else {
        return true;
    };
    projects
        .iter()
        .find(|p| crate::project::record::repo_url_matches(&p.repo_url, origin))
        .map(|p| p.worktree_enabled())
        .unwrap_or(true)
}

/// Return the FIRST already-`Active` session whose cwd is EXACTLY
/// `local_path`, if any (#3455 collision detector).
///
/// Why: pure, testable core of the "two sessions on one main checkout"
/// warning. Extracted from `spawn_managed_on_main`'s inline `.any(...)` so
/// the WARN path has direct unit coverage and so the caller can name the
/// colliding session (its id + tmux name) in the log for diagnosability.
/// What: returns `Some(&record)` for the first Active record sharing
/// `local_path` as its cwd, else `None` — worktree sessions never match
/// (their cwd is a `.worktrees/<name>` slice, never the bare main checkout).
/// Test: `spawn_managed_on_main_warns_on_concurrent_main_checkout_session`.
pub(super) fn has_concurrent_main_checkout_session<'a>(
    existing: &'a [SessionRecord],
    local_path: &std::path::Path,
) -> Option<&'a SessionRecord> {
    existing
        .iter()
        .find(|r| r.state == ManagedSessionState::Active && r.cwd == local_path)
}

/// Spawn a managed session directly in the project's main checkout, with NO
/// per-session git worktree (#3455 "launch on main" opt-out).
///
/// Why: some projects have a direct-main workflow (no PR flow, no isolation
/// requirement) where the standard per-session worktree is pure friction — a
/// worktree sitting unused at a stale commit, a cwd the agent must `cd` out
/// of on every command, and a stale-state misdiagnosis risk (issue #3455).
/// `spawn_managed_routed` calls this INSTEAD of the worktree-provisioning
/// branch when the matched registered project has `worktree ==
/// Some(false)`. Otherwise this is a normal managed session in every other
/// respect — agents/skills deployed, project hooks written, tracked in the
/// session manager, front-gated, reconnectable — it just never clones a base
/// checkout or adds a worktree; the session's cwd/workspace IS `local_path`.
/// What: (1) logs (never refuses), naming the colliding session's id + tmux
/// name via [`has_concurrent_main_checkout_session`], when a second Active
/// session already targets this EXACT `local_path` — the caller only reaches
/// this function after its own reconnect check found no live session for the
/// repo, so this can only fire when `force_new` deliberately bypassed that
/// check; running two sessions against ONE main checkout is not isolated the
/// way two worktrees are (uncommitted-change races, concurrent git
/// operations), so this is the collision caveat #3455 asks to surface; (2)
/// resolves the semantic tmux name via `SessionManager::resolve_session_name`
/// with no worktree-collision predicate (there is no worktree to collide
/// with); (3) writes `TASK.md`; (4) runs `prepare_inproject_session` directly
/// against `local_path` (mirrors `spawn_managed_inproject`'s #1913 fix — no
/// clone step wraps this path either); (5) creates the session record with
/// `workspace_owned = false` — the operator's own checkout must NEVER be
/// auto-deleted by decommission (verified safe: `decommission_with_root`
/// only ever removes a `workspace_owned = false` path when
/// `is_session_worktree` recognises it as living under `.worktrees/`, which
/// `local_path` never does); (6) sets `source_id`; (7) front gates; (8)
/// marks `Active`; (9) spawns the runtime.
/// Test: `spawn_managed_on_main_creates_record_without_worktree`,
/// `spawn_managed_on_main_warns_on_concurrent_main_checkout_session` in
/// `lifecycle_tests.rs`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_managed_on_main(
    state: &Arc<DaemonState>,
    session_id: &ManagedSessionId,
    params: &SpawnParams,
    runtime: RuntimeKind,
    local_path: &std::path::Path,
    owner: &str,
    repo: &str,
) -> Result<SessionRecord, String> {
    use crate::core::provisioning_stage::{ProvisioningStage, emit};

    let mgr = state.session_manager().await;

    // #3455 collision caveat: warn (never refuse) when a second Active
    // session is already running against this EXACT main checkout — the
    // caller's reconnect check already ruled out "same repo, some live
    // session"; reaching here with a collision means `force_new` explicitly
    // asked for a second session on the identical directory. Naming the
    // colliding session lets an operator identify which other session shares
    // the checkout.
    {
        let existing = mgr.list().await;
        if let Some(other) = has_concurrent_main_checkout_session(&existing, local_path) {
            warn!(
                path = %local_path.display(),
                colliding_session_id = %other.id,
                colliding_session_name = %other.tmux_name,
                "spawn_managed (launch-on-main): a second Active session is launching in \
                 the SAME main checkout (#3455) — there is no worktree isolating them from \
                 the colliding session; concurrent git operations or uncommitted-change \
                 races are possible"
            );
        }
    }

    let reserved_name = mgr
        .resolve_session_name(params.name_hint.as_deref(), Some(repo), local_path, |_| {
            false
        })
        .await
        .map_err(|e| format!("name resolution failed for session {session_id}: {e}"))?;

    write_task_md(local_path, &params.task, session_id);

    let synthetic_repo_url = format!("https://github.com/{owner}/{repo}");
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(local_path);
    prepare_inproject_session(&fw, session_id, local_path, &synthetic_repo_url);

    emit(ProvisioningStage::CreatingTmuxSession);
    let record = mgr
        .create_with_reserved_name(
            *session_id,
            reserved_name,
            params.task.clone(),
            Some(local_path.to_path_buf()),
            Some(local_path.to_path_buf()),
            Some(synthetic_repo_url),
            None,
            runtime,
            params.ephemeral.unwrap_or(false),
            false, // workspace_owned: the operator's own main checkout, never auto-deletable
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (launch-on-main): create failed: {e}");
            e.to_string()
        })?;

    let source_id = format!("{owner}/{repo}");
    if let Err(e) = mgr.set_source_id(session_id, &source_id).await {
        warn!(id = %session_id, "spawn_managed (launch-on-main): set_source_id failed: {e}");
    }

    if let Some(record) =
        front_gate_or_escalate(&mgr, &record, &params.repo_url, &params.task).await?
    {
        return Ok(record);
    }

    if let Err(e) = mgr
        .set_workspace(
            session_id,
            local_path.to_path_buf(),
            ManagedSessionState::Active,
        )
        .await
    {
        warn!(id = %session_id, "spawn_managed (launch-on-main): set_workspace failed: {e}");
    }

    if let Err(reason) =
        ensure_deployment_complete(&fw, local_path, record.repo_url.as_deref(), session_id)
    {
        warn!(
            id = %session_id,
            "spawn_managed (launch-on-main): deployment incomplete after auto-repair \
             (non-blocking, launch proceeds): {reason}"
        );
    }

    crate::core::session_launch::spawn_workstream_label_ensure(
        record.repo_url.clone(),
        local_path.to_path_buf(),
        record.tmux_name.clone(),
    );

    emit(ProvisioningStage::LaunchingRuntime);
    let tmux_arc = mgr.tmux_driver();
    let adapter = crate::runtime::build_adapter(record.runtime, tmux_arc);
    let gh_env = resolve_gh_env(state, local_path).await;
    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        local_path,
        &params.task,
        &record.id.to_string(),
        &gh_env,
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_managed (launch-on-main): runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            path = %local_path.display(),
            "managed session spawned successfully (launch-on-main, no worktree)"
        );
    }

    emit(ProvisioningStage::Complete);
    Ok(mgr.get(&record.id).await.unwrap_or(record))
}
