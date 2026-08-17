//! HTTP handlers for the #1508 ephemeral bulk-teardown + by-state prune routes.
//!
//! Why: the managed-session API needs two new teardown endpoints — one to tear
//! down every ephemeral (test) session, and a general by-state prune that can also
//! purge the legacy 239 stale records. They live in their own file so the route
//! module (`mod.rs`) stays under the 500-SLOC production cap.
//! What: [`decommission_ephemeral_route`] (POST …/managed/decommission-ephemeral)
//! and [`prune_managed_route`] (POST …/managed/prune) handlers, plus the
//! [`PruneRequest`] body. Both delegate to the shared
//! [`crate::session_manager::SessionManager`] prune engine so HTTP, CLI, and MCP
//! share ONE implementation.
//! Test: `prune_route_*` in tests/session_manager_mvp.rs.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_reclaim::ReclaimMode;
use crate::session_manager::worktree_reclaim_sweep::reclaim_merged_pr_worktrees;
use crate::session_manager::{DirtyWorktreePolicy, PruneFilter};

/// Request body for POST /api/v1/sessions/managed/prune (#1508).
///
/// Why: the by-state prune must let an operator choose WHICH records to target
/// (`ephemeral`/`stopped`/`decommissioned`/`all`), preview with `dry_run`, and —
/// only when they really mean it — also include RUNNING sessions via
/// `include_active`. Modelling it as a typed body keeps the safety defaults
/// explicit (running sessions are spared unless `include_active` is `true`).
/// What: `state` (the [`PruneFilter`] spelling), `dry_run` (default false), and
/// `include_active` (default false).
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state`.
#[derive(Debug, Deserialize)]
pub struct PruneRequest {
    /// Which records to target: `ephemeral` | `stopped` | `decommissioned` | `all`.
    pub state: String,
    /// When true, report what WOULD be pruned without mutating anything.
    #[serde(default)]
    pub dry_run: bool,
    /// When true, also tear down RUNNING (`Active`/`Provisioning`) sessions.
    /// Defaults to false — the fail-closed safety default.
    #[serde(default)]
    pub include_active: bool,
}

/// POST /api/v1/sessions/managed/decommission-ephemeral — bulk-tear-down ephemeral (#1508).
///
/// Why: the one-shot "clean up all my throwaway test sessions" verb for e2e
/// harnesses and operators. REAL sessions default `ephemeral=false` and are
/// unreachable, so this can never harm durable work.
/// What: delegates to
/// [`SessionManager::decommission_all_ephemeral`](crate::session_manager::SessionManager::decommission_all_ephemeral)
/// and returns `{ decommissioned: <count> }`.
/// Test: `decommission_ephemeral_route_tears_down_only_ephemeral` in
/// tests/session_manager_mvp.rs.
pub async fn decommission_ephemeral_route(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    match mgr.decommission_all_ephemeral().await {
        Ok(count) => Json(serde_json::json!({ "decommissioned": count })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Request body for POST /api/v1/sessions/managed/prune-worktrees (#1840).
///
/// Why: the orphaned-worktree sweep should default to dry-run so operators can
/// preview what will be removed before committing.
/// What: `dry_run` (default true — safe preview default) and `discard_dirty`
/// (#4091, default FALSE — the only way to reach
/// [`DirtyWorktreePolicy::ForceDiscard`], i.e. to delete a worktree that still
/// holds uncommitted or unpushed work; omitting the field, or sending `{}`,
/// always yields the safe skip-and-report behaviour).
/// Test: `prune_worktrees_request_defaults_to_skip_dirty`.
#[derive(Debug, Deserialize)]
pub struct PruneWorktreesRequest {
    /// When true (the default), report orphans without deleting anything.
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    /// When true, ALSO remove worktrees holding uncommitted/unpushed work
    /// (#4091). Defaults to false — the fail-safe default.
    #[serde(default)]
    pub discard_dirty: bool,
    /// When true, ALSO run the merged-pull-request reclaim pass (#2919).
    ///
    /// Defaults to false. This is the ONLY way to reach
    /// [`ReclaimMode::Remove`]; no timer, hook, or daemon sweep sets it, so
    /// merged-PR reclamation is never unattended. It respects `dry_run` and is
    /// independent of `discard_dirty` — the merged-PR pass never destroys
    /// unsaved work under any combination of flags.
    #[serde(default)]
    pub merged_prs: bool,
}

fn default_dry_run() -> bool {
    true
}

/// POST /api/v1/sessions/managed/prune-worktrees — remove orphaned worktree dirs (#1840).
///
/// Why: sessions decommissioned before Fix 1a (#1840), or where
/// `git worktree remove` failed, may leave stale `.worktrees/<session-id>/`
/// directories. This endpoint removes them safely, never touching a directory
/// that belongs to an active session.
/// What: collects active workspace paths and the managed workspace root, then
/// delegates to [`SessionManager::prune_orphaned_worktrees`](crate::session_manager::SessionManager::prune_orphaned_worktrees). Returns
/// `{ dry_run, paths, owner_unknown_paths, agent_owned_paths, skipped_dirty }`
/// (#3649 added the second field: worktrees conservatively skipped because
/// their ownership sentinel had no resolvable owner — never auto-deleted,
/// surfaced here for operator review; #4091 adds the third: worktrees skipped
/// because they still hold uncommitted or unpushed work, each an object with
/// `path`, `reason`, `dirty_files`, and `unpushed_commits`). Dry-run is the
/// default, and so is skipping dirty worktrees — `discard_dirty: true` is the
/// only path to a destructive removal of unsaved work.
/// Test: `prune_worktrees_route_dry_run`,
/// `prune_worktrees_request_defaults_to_skip_dirty`,
/// `prune_spares_a_stopped_records_workspace` (#4288).
pub async fn prune_worktrees_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PruneWorktreesRequest>,
) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    // #4288 (item 4 of #4207): DELIBERATELY UNFILTERED. Do NOT "tidy this up"
    // by adding `.filter(|r| r.state == ManagedSessionState::Active)` — every
    // record with a `workspace_path` belongs in this set, whatever its state.
    //
    // Why: a `SessionRecord`'s state is bookkeeping, NOT a liveness signal. It
    // is written by reconcile/stop/hook paths that can miss, race, or be
    // skipped entirely, so live sessions are routinely observed carrying a
    // terminal state. Measured on this repo 2026-07-28: session
    // `2eb72dca-de08-481b-8dfa-22ab7f81b1f9` was RUNNING (tmux pane `%981`,
    // `pane_current_path` inside its own worktree) while `sessions.json`
    // recorded it as `state: "stopped"`, holding 12 modified tracked files,
    // 31 untracked files, and 1 unpushed commit.
    //
    // What narrowing this set costs, measured rather than assumed (#4288):
    // such a worktree stops being spared and becomes an orphan CANDIDATE, so
    // this route's DRY-RUN preview reports a live worktree as reclaimable —
    // a false report an operator may then act on. It does NOT by itself delete
    // anything: the real (`dry_run: false`) path re-reads the store for
    // `prune_orphaned_worktrees`'s Phase 2 `fresh_active` snapshot, itself
    // deliberately unfiltered, and that second read still spares the candidate
    // immediately before deletion. The two reads are defense-in-depth; data
    // loss needs BOTH narrowed. Do not read that as permission to narrow one
    // "because the other covers it" — that argument applied twice is exactly
    // how a pair of independent boundaries collapses into none.
    //
    // Pinned by `prune_spares_a_stopped_records_workspace` in this file's test
    // module (which fails on the dry-run preview alone), and end-to-end on the
    // automatic GC path by `reap_spares_a_stopped_records_workspace` in
    // `session_manager::reap_orphaned_worktrees_tests`.
    let in_use_workspace_paths: Vec<std::path::PathBuf> = records
        .iter()
        .filter_map(|r| r.workspace_path.clone())
        .collect();
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let repos_root = crate::core::trusty_tools_config::workspace_root(&config);
    // Item 7 (#1845): propagate scan failures as HTTP 500 instead of silently
    // returning an empty path list (which could mask the underlying error).
    // #4091: the ONLY place `ForceDiscard` can be requested, and only via an
    // explicit `discard_dirty: true` in the request body.
    let policy = if req.discard_dirty {
        DirtyWorktreePolicy::ForceDiscard
    } else {
        DirtyWorktreePolicy::Skip
    };
    match mgr
        .prune_orphaned_worktrees(&repos_root, &in_use_workspace_paths, req.dry_run, policy)
        .await
    {
        Ok(outcome) => {
            let paths: Vec<String> = outcome
                .removed
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let owner_unknown_paths: Vec<String> = outcome
                .owner_unknown
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            // #4311: attributed to a dispatched agent — owned, and reclaimed by
            // that agent's exit rather than this sweep. Reported because these
            // arrived in `owner_unknown_paths` before they carried a
            // sentinel, and losing them from the response would trade a
            // "cannot reclaim" line for silence.
            let agent_owned_paths: Vec<String> = outcome
                .agent_owned
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            // #2919: the merged-PR pass runs only on the explicit opt-in, and
            // only ever in Report mode under `dry_run`. It re-reads the
            // in-use set itself via `in_use_workspace_paths` above, which was
            // captured moments ago from the same unfiltered store read.
            let merged = if req.merged_prs {
                let mode = if req.dry_run {
                    ReclaimMode::Report
                } else {
                    ReclaimMode::Remove
                };
                let root = repos_root.clone();
                // #2919: a HANDLE to the manager, not a captured path list. The
                // delete loop calls this closure per candidate and needs the
                // CURRENT set, not one snapshotted before a survey that takes
                // minutes. `None` (the store could not be read) refuses.
                let mgr_for_probe = state.session_manager().await.clone();
                // #5661: the sweep's other gates read SESSION records, and a
                // dispatched agent has none — which is how this path deleted
                // three live agents' worktrees. The delegation registry is the
                // only place an agent's liveness is resolved from real
                // `SubagentStop` signals, so it is read here and handed to the
                // classifier as a probe rather than as a captured list.
                let state_for_agents = Arc::clone(&state);
                match tokio::task::spawn_blocking(move || {
                    let in_use_now = move || -> Option<Vec<std::path::PathBuf>> {
                        // `None` means "could not be determined", which REFUSES
                        // the delete. `SessionManager::list` is itself
                        // infallible, so the only way to fail here is to have no
                        // runtime to block on — which happens if this closure is
                        // ever invoked off the blocking pool. `Handle::current`
                        // would PANIC in that case, unwinding through a delete
                        // loop mid-sweep; `try_current` turns it into the
                        // fail-closed refusal the contract already specifies.
                        let handle = tokio::runtime::Handle::try_current().ok()?;
                        // Blocking on the runtime is legal from a blocking-pool
                        // thread (not a runtime worker), and is the only way to
                        // re-read an async store from the synchronous loop.
                        Some(
                            handle
                                .block_on(mgr_for_probe.list())
                                .into_iter()
                                .filter_map(|r| r.workspace_path)
                                .collect(),
                        )
                    };
                    let agent_state = move |owner: &crate::session_manager::worktree_ownership::AgentWorktreeOwner| {
                        crate::daemon::services::agent_worktree_reap::delegation_state_for_agent(
                            &state_for_agents,
                            &owner.agent_id,
                        )
                    };
                    reclaim_merged_pr_worktrees(&root, &in_use_now, &agent_state, mode)
                })
                .await
                {
                    Ok(o) => serde_json::json!({
                        "removed": o.removed,
                        "removed_bytes": o.removed_bytes,
                        "refused_at_recheck": o.refused_at_recheck,
                        "removal_failed": o.removal_failed,
                        "reclaimable": o.survey.reclaimable,
                        "reclaimable_measured": o.survey.reclaimable_measured,
                        "reclaimable_bytes": o.survey.reclaimable_bytes,
                        "total_bytes": o.survey.total_bytes,
                        "pr_state_unknown": o.survey.pr_state_unknown,
                    }),
                    Err(e) => {
                        // A panicked pass reclaimed nothing; say so rather than
                        // omitting the key, which would read as "not requested".
                        warn!("prune-worktrees route: merged-PR pass panicked: {e}");
                        serde_json::json!({ "error": e.to_string() })
                    }
                }
            } else {
                serde_json::Value::Null
            };
            Json(serde_json::json!({
                "dry_run": req.dry_run,
                "paths": paths,
                "owner_unknown_paths": owner_unknown_paths,
                "agent_owned_paths": agent_owned_paths,
                "skipped_dirty": outcome.skipped_dirty,
                "merged_prs": merged,
            }))
            .into_response()
        }
        Err(e) => {
            warn!("prune-worktrees route: orphan scan failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("orphan worktree scan failed: {e}"),
            )
                .into_response()
        }
    }
}

/// POST /api/v1/sessions/managed/prune — by-state prune + compaction (#1508).
///
/// Why: the general teardown tool, exposed so the legacy 239 stale records can be
/// purged over HTTP with the SAME engine that cleans up ephemeral sessions.
/// What: parses the `state` filter (400 on an unknown value), then delegates to
/// [`SessionManager::prune_managed`](crate::session_manager::SessionManager::prune_managed)
/// with the request's `dry_run`/`include_active`. Returns the
/// [`crate::session_manager::PruneOutcome`] JSON (its `dry_run`, `filter`, and
/// per-session `action` list).
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state` in
/// tests/session_manager_mvp.rs.
pub async fn prune_managed_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PruneRequest>,
) -> impl IntoResponse {
    let filter = match PruneFilter::parse(&req.state) {
        Ok(f) => f,
        Err(e) => {
            warn!("prune route: {e}");
            return (StatusCode::BAD_REQUEST, e).into_response();
        }
    };
    let mgr = state.session_manager().await;
    // `caller: None` — the HTTP route is an operator surface, never a session
    // acting on its own behalf; the #3649 owner gate does not apply here.
    match mgr
        .prune_managed(filter, req.dry_run, req.include_active, None)
        .await
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #4091: a request body that says nothing about dirty worktrees must
    /// deserialize to the SAFE behaviour. An omitted field is the overwhelming
    /// majority of real calls (the CLI's non-`--discard-dirty` path, and any
    /// third-party caller), so the serde default is the actual guard here.
    #[test]
    fn prune_worktrees_request_defaults_to_skip_dirty() {
        let req: PruneWorktreesRequest = serde_json::from_str("{}").expect("empty body parses");
        assert!(req.dry_run, "an unspecified prune must preview, not delete");
        assert!(
            !req.discard_dirty,
            "an unspecified prune must NEVER discard uncommitted work"
        );

        // #2919: the same guarantee for the merged-PR pass. An omitted field —
        // every pre-#2919 caller — must never enable a GitHub-state-driven
        // deletion.
        assert!(
            !req.merged_prs,
            "an unspecified prune must NEVER run the merged-PR reclaim pass"
        );

        let explicit: PruneWorktreesRequest =
            serde_json::from_str(r#"{"dry_run":false,"discard_dirty":true,"merged_prs":true}"#)
                .expect("explicit body parses");
        assert!(explicit.discard_dirty, "the opt-in must round-trip");
        assert!(explicit.merged_prs, "the #2919 opt-in must round-trip");
    }

    /// Read a route response's JSON body.
    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("route body must be readable");
        serde_json::from_slice(&bytes).expect("route body must be JSON")
    }

    /// #4288: a `Stopped` record's workspace is spared by the orphan sweep —
    /// the active-set construction above must stay UNFILTERED by record state.
    ///
    /// Why: session state is not a liveness signal (see the comment at the
    /// construction site — a live session was measured recorded as `stopped`
    /// while holding 12 modified files and an unpushed commit). The obvious
    /// tidy-up, `.filter(|r| r.state == Active)`, would drop that record's
    /// workspace out of the spared set and hand a LIVE worktree to the
    /// reclaim path. This test exists to turn that tidy-up red.
    ///
    /// Non-vacuity: the fixture is stamped with an aged, well-formed ownership
    /// sentinel naming a never-registered owner, so EVERY downstream gate
    /// (#3649 ownership, the `git worktree list` cross-check, the #4091 dirty
    /// gate) already votes "reclaim". The CONTROL below asserts exactly that
    /// against an EMPTY active set — so if a future change makes the fixture
    /// unreclaimable for some unrelated reason, the control fails loudly
    /// instead of letting the real assertions pass for the wrong reason.
    /// Membership in `in_use_workspace_paths` is therefore the ONLY thing
    /// under test here.
    /// Test: this function IS the test.
    ///
    /// `await_holding_lock` is the point, not an oversight: the route reads the
    /// workspace root from the process environment, so the crate-wide
    /// `env_test_lock` must span the awaited route calls or a sibling env test
    /// can clobber the override mid-request. Each `#[tokio::test]` gets its own
    /// thread and current-thread runtime, so a blocking sync guard serialises
    /// those threads without any chance of deadlocking the executor.
    #[allow(clippy::await_holding_lock)]
    #[serial_test::serial]
    #[tokio::test]
    async fn prune_spares_a_stopped_records_workspace() {
        use crate::session_manager::record::ManagedSessionId;
        use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
        use crate::session_manager::{ManagedSessionState, SessionRecord};

        let fx = GitWorktreeFixture::new();
        let wt = fx.add_worktree("stopped-but-live");
        GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
        let wt_str = wt.to_string_lossy().into_owned();

        let root = crate::test_support::hermetic_temp_dir();
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let mgr = state.session_manager().await;

        // CONTROL: with an EMPTY active set the fixture IS reclaimable. Dry-run
        // so nothing is deleted before the real assertions run.
        let control = mgr
            .prune_orphaned_worktrees(&fx.repos_root, &[], true, DirtyWorktreePolicy::Skip)
            .await
            .expect("control sweep must not error");
        assert!(
            control.removed.contains(&wt),
            "CONTROL: {wt_str} must be reclaimable with an empty active set, \
             otherwise this test proves nothing; removed={:?} owner_unknown={:?} \
             skipped_dirty={:?}",
            control.removed,
            control.owner_unknown,
            control.skipped_dirty
        );

        // Register the worktree to a record and drive it to `Stopped` — the
        // exact shape observed on a session that was actually still running.
        let record = mgr
            .create_with_id(
                ManagedSessionId::new(),
                "pinned by #4288".into(),
                Some(wt.clone()),
                None,
                Some(wt.clone()),
                None,
                None,
                crate::runtime::RuntimeKind::default(),
                false,
                false,
            )
            .await
            .expect("seed session record");
        let stopped = SessionRecord {
            state: ManagedSessionState::Stopped,
            ..record
        };
        mgr.store
            .write()
            .await
            .upsert(stopped)
            .await
            .expect("persist the Stopped record");

        // Point the route's repos-root resolver at the fixture.
        //
        // This test issues a DRY-RUN sweep only — it must never run a deleting
        // sweep whose target root comes from a process-global env var. If a
        // concurrent test restored that var mid-call, a destructive sweep would
        // target the operator's real `~/trusty-mpm-projects`, and the
        // three-read defense would NOT save it: the caller set and the Phase 2
        // re-read both draw from this empty isolated store, so real worktrees
        // are absent from both. The deleting half also bought zero mutation
        // coverage (its assertions pass under the M1 mutation), so it is gone.
        //
        // BOTH guards are required and they do not interoperate:
        // `env_test_lock` serialises the env-precedence tests, while
        // `#[serial_test::serial]` serialises `connectors::tm_tests`, which
        // mutates this same var in this same binary under `serial` alone.
        let _env = crate::core::trusty_tools_config::env_test_lock();
        // SAFETY: guarded by env_test_lock; removed below before the asserts.
        unsafe {
            std::env::set_var(
                crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV,
                &fx.repos_root,
            )
        };

        let preview = body_json(
            prune_worktrees_route(
                State(state.clone()),
                Json(PruneWorktreesRequest {
                    dry_run: true,
                    discard_dirty: false,
                    merged_prs: false,
                }),
            )
            .await
            .into_response(),
        )
        .await;
        // SAFETY: guarded by env_test_lock, still held.
        unsafe { std::env::remove_var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV) };

        let strings = |v: &serde_json::Value, key: &str| -> Vec<String> {
            v.get(key)
                .and_then(|x| x.as_array())
                .unwrap_or_else(|| panic!("route body must carry `{key}`: {v}"))
                .iter()
                .map(|s| {
                    s.as_str()
                        .unwrap_or_else(|| panic!("`{key}` must hold strings: {v}"))
                        .to_owned()
                })
                .collect()
        };

        // Dry-run `paths` IS the reclaimable set (see `prune_orphaned_worktrees`).
        let preview_paths = strings(&preview, "paths");
        assert!(
            !preview_paths.contains(&wt_str),
            "a Stopped record's workspace must NOT be reclaimable; \
             {wt_str} appeared in the dry-run set {preview_paths:?}"
        );
        let preview_unknown = strings(&preview, "owner_unknown_paths");
        assert!(
            !preview_unknown.contains(&wt_str),
            "a spared workspace is never even a candidate, so it must not be \
             reported as owner-unknown either; got {preview_unknown:?}"
        );
    }
}
