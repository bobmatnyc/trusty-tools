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

use axum::{Json, extract::State, response::IntoResponse};
use serde::Deserialize;
use tracing::warn;

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::prune::OrphanSweepOutcome;
use crate::session_manager::prune_progress::PruneCancel;
use crate::session_manager::worktree_reclaim::ReclaimMode;
use crate::session_manager::worktree_reclaim_preview::preview_rows;
use crate::session_manager::worktree_scope::PruneScope;
use crate::session_manager::{DirtyWorktreePolicy, PruneFilter};

/// Request body for POST /api/v1/sessions/managed/prune (#1508).
///
/// Why: the by-state prune must let an operator choose WHICH records to target
/// (`ephemeral`/`stopped`/`decommissioned`/`all`), preview with `dry_run`, and —
/// only when they really mean it — also include RUNNING sessions via
/// `include_active`. Modelling it as a typed body keeps the safety defaults
/// explicit (running sessions are spared unless `include_active` is `true`).
/// What: `state` (the [`PruneFilter`] spelling), `dry_run` (default false),
/// `include_active` (default false), and `invoking_session` (#6118, default
/// absent).
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state`,
/// `prune_route_rejects_an_unparseable_invoking_session`.
#[derive(Debug, Deserialize)]
pub struct PruneRequest {
    /// Which records to target: `ephemeral` | `stopped` | `decommissioned` |
    /// `deleted` | `all` | `unresolvable`.
    pub state: String,
    /// When true, report what WOULD be pruned without mutating anything.
    #[serde(default)]
    pub dry_run: bool,
    /// When true, also tear down RUNNING (`Active`/`Provisioning`) sessions.
    /// Defaults to false — the fail-closed safety default.
    #[serde(default)]
    pub include_active: bool,
    /// The managed session id this prune was invoked FROM, which is then never
    /// a target (#6118).
    ///
    /// Why: the daemon cannot discover this — it occupies no pane — so the
    /// caller supplies it. The CLI sends `$TM_MANAGED_SESSION_ID` when the
    /// operator is inside a managed session. Absent means "the caller is not a
    /// session", which is correct for the MCP tool and for an ad-hoc shell.
    /// A value present but unparseable is REJECTED rather than dropped: silently
    /// ignoring it would run the sweep with the protection the caller asked for
    /// switched off.
    #[serde(default)]
    pub invoking_session: Option<String>,
}

/// Query string for POST …/managed/decommission-ephemeral (#6118).
///
/// Why: the verb had no preview mode, so an operator could only learn its
/// selection by running it. A query param rather than a body keeps every
/// existing bodyless caller working — axum's `Json` extractor rejects an empty
/// body, so adding a body would have broken the CLI, the MCP path, and any
/// script that POSTs nothing.
/// What: `dry_run` (default false — the pre-#6118 behaviour).
/// Test: `ephemeral_route_dry_run_reports_without_tearing_down`.
#[derive(Debug, Deserialize)]
pub struct EphemeralQuery {
    /// When true, report what WOULD be torn down and mutate nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// POST /api/v1/sessions/managed/decommission-ephemeral — bulk-tear-down ephemeral (#1508).
///
/// Why: the one-shot "clean up all my throwaway test sessions" verb for e2e
/// harnesses and operators. REAL sessions default `ephemeral=false` and are
/// unreachable, so this can never harm durable work.
/// What: delegates to
/// [`SessionManager::sweep_all_ephemeral`](crate::session_manager::SessionManager::sweep_all_ephemeral)
/// and returns `{ decommissioned: <count>, dry_run: <bool> }`.
///
/// 🔴 #6118: `dry_run` is ECHOED, and that echo is load-bearing, not cosmetic.
/// This daemon is long-lived by design — a CLI upgrade never bounces it — and
/// axum silently drops a query param the handler does not declare, so a daemon
/// predating this change accepts `?dry_run=true` and performs a REAL teardown,
/// returning 200. The status alone cannot tell that apart from an honoured
/// preview; only the echoed field can, which is why the CLI refuses to report a
/// dry run the daemon did not confirm. Same failure shape, and the same remedy,
/// as `session_picker_prune::decommission_dead_record`'s `workspace_removed`
/// check.
/// Test: `decommission_ephemeral_route_tears_down_only_ephemeral` in
/// tests/session_manager_mvp.rs;
/// `ephemeral_route_dry_run_reports_without_tearing_down` in this file;
/// the CLI's refusal by `decommission_ephemeral_refuses_a_dry_run_a_stale_daemon_ignored`.
pub async fn decommission_ephemeral_route(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Query(q): axum::extract::Query<EphemeralQuery>,
) -> impl IntoResponse {
    decommission_ephemeral_core(&state, q.dry_run).await // #6288
}

/// The transport-neutral body of `POST .../managed/decommission-ephemeral`
/// (#6288), served over the socket as `mpm.managed.decommission_ephemeral`.
///
/// Test: `managed_decommission_ephemeral_parity` in
/// `daemon::rpc::managed_tests`.
pub(crate) async fn decommission_ephemeral_core(
    state: &Arc<DaemonState>,
    dry_run: bool,
) -> RouteOutcome {
    let mgr = state.session_manager().await;
    match mgr.sweep_all_ephemeral(dry_run).await {
        Ok(outcome) => RouteOutcome::ok(&serde_json::json!({
            "decommissioned": outcome.count(),
            "dry_run": dry_run,
        })),
        Err(e) => RouteOutcome::text(500, e.to_string()),
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
    /// The managed session id this sweep was invoked FROM (#6806).
    ///
    /// Why: gate 2 refuses a worktree any live session claims, and a session
    /// pruning worktrees it created inside its own workspace was blocked by
    /// its OWN claim — 31 clean, pushed, merged worktrees in one observed run.
    /// The daemon cannot discover the caller (it occupies no pane), so the
    /// caller supplies it, exactly as `PruneRequest::invoking_session` does.
    /// The CLI sends `$TM_MANAGED_SESSION_ID`. Absent means "the caller is not
    /// a session", which is correct for the MCP tool and an ad-hoc shell, and
    /// leaves every claim foreign — the pre-#6806 behaviour.
    ///
    /// A value present but unparseable is REJECTED rather than dropped:
    /// silently ignoring it would run the sweep with the self-attribution the
    /// caller asked for switched off, and report every one of its own
    /// worktrees as blocked by a stranger.
    #[serde(default)]
    pub invoking_session: Option<String>,
    /// The checkout the caller is working in; when set, both passes touch only
    /// worktrees that checkout's registry lists (#8782).
    ///
    /// Absent means every registered project — the automatic sweep's and the
    /// MCP tool's scope. A value that does not resolve is REJECTED, never read
    /// as "absent", so a typo cannot widen a prune to every project.
    #[serde(default)]
    pub project_root: Option<String>,
    /// The orphan paths a `--force` run's own preview listed (#8782). When
    /// set, the orphan sweep surveys and removes nothing outside it.
    #[serde(default)]
    pub only_orphan_paths: Option<Vec<String>>,
    /// The merged-PR paths that preview listed (#8782); the same bound for the
    /// merged-PR pass.
    #[serde(default)]
    pub only_merged_paths: Option<Vec<String>>,
    /// The orphan paths that preview said `--discard-dirty` would discard
    /// unsaved work from (#8782). When set, a dirty tree it does not list is
    /// kept, even under `discard_dirty`.
    #[serde(default)]
    pub only_discard_paths: Option<Vec<String>>,
    /// Keep running after the client leaves (#7619, `--detach`). Read by the
    /// streaming method only: without it a departed client cancels the pass
    /// at its next candidate. The unary method ignores it.
    #[serde(default)]
    pub detach: bool,
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
/// `prune_worktrees_route_rejects_an_unparseable_invoking_session` (#6806),
/// `prune_spares_a_stopped_records_workspace` (#4288).
pub async fn prune_worktrees_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PruneWorktreesRequest>,
) -> impl IntoResponse {
    prune_worktrees_core(&state, req).await // #6288
}

/// The transport-neutral body of `POST .../managed/prune-worktrees` (#6288),
/// served over the socket as `mpm.managed.prune_worktrees`.
///
/// Test: `managed_prune_worktrees_parity_defaults_to_a_preview` in
/// `daemon::rpc::managed_tests`.
pub(crate) async fn prune_worktrees_core(
    state: &Arc<DaemonState>,
    req: PruneWorktreesRequest,
) -> RouteOutcome {
    // #8994: the daemon's one maintenance lane, which the timer sweep also takes.
    prune_worktrees_core_in(state, req, crate::daemon::services::sweep_status::lane()).await
}

/// [`prune_worktrees_core`] with the maintenance lane as a parameter (#8994).
///
/// Why: a `--merged-prs` pass and the timer reclaim sweep must never run at
/// once. The lane is a parameter so a test can hold or close its own lane
/// instead of the process-global one other tests share.
/// What: when `merged_prs` is set, takes the permit with
/// [`try_manual_permit`](crate::daemon::services::merged_pr_reclaim::try_manual_permit)
/// before any work and moves it into the task that runs the pass, so the lane
/// stays held until the pass ends even when the caller's future is dropped.
/// Busy is a 409 and a closed lane a 500; neither runs a pass. #7619: dropping
/// this future cancels the pass at its next candidate boundary; a removal in
/// flight finishes and the unstarted candidates are left untouched.
/// Test: `a_merged_prs_prune_is_refused_while_the_timer_sweep_holds_the_lane`,
/// `a_merged_prs_prune_fails_closed_on_a_closed_lane`,
/// `a_dropped_prune_request_keeps_the_lane_until_the_pass_ends`,
/// `worktree_7619_a_dropped_prune_request_removes_nothing_further`.
pub(crate) async fn prune_worktrees_core_in(
    state: &Arc<DaemonState>,
    req: PruneWorktreesRequest,
    lane: &'static tokio::sync::Semaphore,
) -> RouteOutcome {
    let (scope, permit) = match admit_prune(&req, lane) {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    // See #8994: the pass runs in its own task that OWNS the permit. The
    // reclaim and orphan loops run in `spawn_blocking`, which a dropped
    // handler future (client disconnect) does not stop; a permit held by the
    // handler would free the lane while those loops still remove worktrees,
    // letting the timer sweep start a second pass. A dropped handler now only
    // detaches the task, and the permit drops when the pass ends (#8451).
    // #7619: a dropped handler future also cancels the pass. The guard lives in
    // this future, so its drop — client gone, deadline, or unwind alike, never
    // told apart — stops both loops at their next candidate; the permit then
    // drops with the task.
    let token = tokio_util::sync::CancellationToken::new();
    let _cancel_on_drop = token.clone().drop_guard();
    let cancel = PruneCancel::from_probe(move || Some(token.is_cancelled()));
    let state = Arc::clone(state);
    match tokio::spawn(async move {
        let _permit = permit;
        prune_worktrees_pass(&state, req, scope, cancel).await
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!("prune-worktrees route: the prune task failed: {e}");
            RouteOutcome::text(500, format!("the prune task failed: {e}"))
        }
    }
}

/// Every refusal a prune-worktrees request can meet before any work (#6806,
/// #8782, #8994), shared by the unary and streaming methods (#7619).
///
/// What: the caller id must parse, the scope must resolve, and a
/// `merged_prs` pass must take the maintenance permit. `Err` is the reply.
/// Test: `prune_route_rejects_an_unparseable_invoking_session`,
/// `a_merged_prs_prune_is_refused_while_the_timer_sweep_holds_the_lane`.
pub(crate) fn admit_prune(
    req: &PruneWorktreesRequest,
    lane: &'static tokio::sync::Semaphore,
) -> Result<(PruneScope, Option<tokio::sync::SemaphorePermit<'static>>), RouteOutcome> {
    // #6806: reject a present-but-unparseable caller id before any work, for
    // the reason #6118 gives — dropping it would run the sweep with the
    // self-attribution the caller asked for silently switched off, and report
    // every one of the caller's own worktrees as blocked by a stranger.
    if let Some(raw) = req.invoking_session.as_deref()
        && raw.parse::<uuid::Uuid>().is_err()
    {
        let msg = "invalid invoking_session: not a UUID — refusing the prune rather than \
                   running it without the requested caller attribution (#6806)"
            .to_string();
        warn!("prune-worktrees route: {msg}");
        return Err(RouteOutcome::text(400, msg));
    }
    // #8782: resolved before any work, and a project root that does not
    // resolve is a 400 rather than a daemon-global sweep.
    let scope = match PruneScope::from_request(
        req.project_root.as_deref(),
        req.only_orphan_paths.as_deref(),
        req.only_merged_paths.as_deref(),
        req.only_discard_paths.as_deref(),
    ) {
        Ok(scope) => scope,
        Err(msg) => {
            warn!("prune-worktrees route: {msg}");
            return Err(RouteOutcome::text(400, msg));
        }
    };
    // #8994: one maintenance permit for manual and timer reclaim, taken before
    // any work. Every refusal returns here.
    let permit = if req.merged_prs {
        match crate::daemon::services::merged_pr_reclaim::try_manual_permit(lane) {
            Ok(permit) => Some(permit),
            Err(refusal) => return Err(merged_prs_refusal(refusal)),
        }
    } else {
        None
    };
    Ok((scope, permit))
}

/// The prune pass itself, run by [`prune_worktrees_core_in`] in a task that holds the permit (#8994).
///
/// Test: `prune_spares_a_stopped_records_workspace`,
/// `a_dropped_prune_request_keeps_the_lane_until_the_pass_ends`.
pub(crate) async fn prune_worktrees_pass(
    state: &Arc<DaemonState>,
    req: PruneWorktreesRequest,
    scope: PruneScope,
    cancel: PruneCancel,
) -> RouteOutcome {
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
    // #7357: the route is the entry point, so it resolves the adopted anchors
    // under the daemon's own framework root — the root `project_register`
    // wrote them to. Every scan below takes them as a parameter.
    let adopted = crate::project::adopted_anchors_under(state.framework_root());
    // #8782: a checkout the scan never reaches finds nothing; say so.
    let project_known = scope.orphan.project_known(&repos_root, &adopted);
    match mgr
        .prune_orphaned_worktrees_in_with_cancel(
            &repos_root,
            &in_use_workspace_paths,
            req.dry_run,
            policy,
            &adopted,
            &scope.orphan,
            &cancel,
        )
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
                // #7504: through the ONE entry point that binds the production
                // probes. The assembly used to live inline here; a second copy
                // for the daemon's automatic sweep is exactly the divergence the
                // common-entry-point rule forbids, so both callers now ask
                // `merged_pr_reclaim::reclaim`. The only thing this route still
                // decides is Report-vs-Remove, from its own `dry_run`.
                // #6806: the caller's own id, so gate 2 can tell its claim from
                // a stranger's. Validated ABOVE, before any survey work.
                match crate::daemon::services::merged_pr_reclaim::reclaim(
                    state,
                    &repos_root,
                    mode,
                    req.invoking_session.clone(),
                    scope.merged.clone(),
                    cancel,
                )
                .await
                {
                    Ok(o) => {
                        // #8782: every path the pass would remove, and every path
                        // it keeps because its PR state is unknown, each with its
                        // project and reason.
                        let preview = preview_rows(&o.survey);
                        serde_json::json!({
                            "reclaimable_paths": preview.reclaim,
                            "unknown_paths": preview.unknown,
                            "removed": o.removed,
                            "removed_bytes": o.removed_bytes,
                            "refused_at_recheck": o.refused_at_recheck,
                            "removal_failed": o.removal_failed,
                            // #8782: git failed after deleting content.
                            "partially_removed": o.partially_removed,
                            // #7619: approved, never started, untouched.
                            "cancelled_unstarted": o.cancelled_unstarted,
                            // #5829: the agent-ownership gate has spared these
                            // since #5661, but reported nothing — so a run that
                            // protected a live agent's tree was indistinguishable
                            // from one that found nothing to reclaim.
                            "spared_agent_owned": o.survey.agent_owned,
                            // #6507: one line per non-reclaimable candidate naming
                            // the GATE that refused it. Without it a plain
                            // `Blocked` — every gate but 4 — appears in no field of
                            // this reply and in no log line, which is how a merged,
                            // clean worktree sat unreclaimed with nothing saying
                            // why.
                            "blocked_reasons": o.survey.blocked_reasons,
                            "reclaimable": o.survey.reclaimable,
                            "reclaimable_measured": o.survey.reclaimable_measured,
                            "reclaimable_bytes": o.survey.reclaimable_bytes,
                            "total_bytes": o.survey.total_bytes,
                            "pr_state_unknown": o.survey.pr_state_unknown,
                            // #6561: without these three, `0 reclaimable` and
                            // "the `gh` call failed for every worktree" are the
                            // same reply. The renderer prints them beside the
                            // reclaimable count.
                            "not_inspected": o.survey.not_inspected,
                            "lookup_failed": o.survey.lookup_failed,
                            "lookup_failure": o.survey.lookup_failure,
                        })
                    }
                    Err(e) => {
                        // A panicked pass reclaimed nothing; say so rather than
                        // omitting the key, which would read as "not requested".
                        warn!("prune-worktrees route: merged-PR pass panicked: {e}");
                        serde_json::json!({ "error": e })
                    }
                }
            } else {
                serde_json::Value::Null
            };
            RouteOutcome::ok(&serde_json::json!({
                "dry_run": req.dry_run,
                // #8782: echoed so a client can refuse a daemon that predates
                // the scope and would have run daemon-global.
                "scope": scope.echo(project_known),
                "orphan_rows": orphan_rows(&outcome),
                "paths": paths,
                "owner_unknown_paths": owner_unknown_paths,
                "agent_owned_paths": agent_owned_paths,
                "skipped_dirty": outcome.skipped_dirty,
                // #8782: git failed after deleting content.
                "partially_removed": outcome.partially_removed,
                // #7619: never started by a cancelled pass, untouched.
                "cancelled_unstarted": outcome.cancelled_unstarted,
                "merged_prs": merged,
            }))
        }
        Err(e) => {
            warn!("prune-worktrees route: orphan scan failed: {e}");
            RouteOutcome::text(500, format!("orphan worktree scan failed: {e}"))
        }
    }
}

/// The reply for a `--merged-prs` prune that did not get the permit (#8994).
///
/// Test: `a_merged_prs_prune_is_refused_while_the_timer_sweep_holds_the_lane`,
/// `a_merged_prs_prune_fails_closed_on_a_closed_lane`.
fn merged_prs_refusal(
    refusal: crate::daemon::services::merged_pr_reclaim::ManualPermitRefusal,
) -> RouteOutcome {
    use crate::daemon::services::merged_pr_reclaim::ManualPermitRefusal;
    let (status, msg) = match refusal {
        ManualPermitRefusal::Busy => (
            409,
            "a worktree reclaim or other maintenance sweep is in progress in the daemon; \
             nothing was pruned — retry when it finishes (#8994)",
        ),
        ManualPermitRefusal::Closed => (
            500,
            "the daemon's maintenance lane is closed, so the reclaim cannot be serialised \
             against the timer sweep; nothing was pruned (#8994)",
        ),
    };
    warn!("prune-worktrees route: {msg}");
    RouteOutcome::text(status, msg.to_string())
}

/// The orphan pass's paths as preview rows: path, owning checkout, reason (#8782).
///
/// What: one row per `removed` path. The project is the registry root the
/// scan carried, so this runs no `git`. A path whose unsaved work
/// `--discard-dirty` destroys says so, with what the dirty check found, and
/// carries `discards_unsaved_work: true` — the CLI's discard allowlist.
/// Test: `orphan_rows_name_the_owning_checkout`,
/// `the_orphan_preview_names_unsaved_work_that_discard_dirty_destroys`.
pub(crate) fn orphan_rows(outcome: &OrphanSweepOutcome) -> Vec<serde_json::Value> {
    outcome
        .removed
        .iter()
        .map(|path| {
            let project = outcome.registry_roots.get(path).map_or_else(
                || "(owning checkout unresolved)".to_string(),
                |r| r.to_string_lossy().into_owned(),
            );
            let dirt = outcome.discarded_dirty.iter().find(|d| d.path == *path);
            let reason = match dirt {
                Some(dirt) => format!(
                    "orphaned — its owning session has ended; it holds unsaved work ({}) — \
                     discarded (--discard-dirty)",
                    dirt.reason
                ),
                None => {
                    "orphaned — its owning session has ended and it holds no unsaved work".into()
                }
            };
            serde_json::json!({
                "path": path.to_string_lossy(),
                "project": project,
                "reason": reason,
                "discards_unsaved_work": dirt.is_some(),
            })
        })
        .collect()
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
///
/// #6118: a present-but-unparseable `invoking_session` is a 400. Dropping it
/// would run the sweep with the self-exclusion the caller explicitly asked for
/// silently disabled — the failure arm has to be louder than the success arm it
/// protects.
/// Test: `prune_route_dry_run_reports`, `prune_route_rejects_bad_state` in
/// tests/session_manager_mvp.rs;
/// `prune_route_rejects_an_unparseable_invoking_session` in this file.
pub async fn prune_managed_route(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PruneRequest>,
) -> impl IntoResponse {
    prune_managed_core(&state, req).await // #6288
}

/// The transport-neutral body of `POST .../managed/prune` (#6288), served over
/// the socket as `mpm.managed.prune`.
///
/// Test: `managed_prune_parity`,
/// `managed_prune_rejects_an_unparseable_invoking_session` in
/// `daemon::rpc::managed_tests`.
pub(crate) async fn prune_managed_core(
    state: &Arc<DaemonState>,
    req: PruneRequest,
) -> RouteOutcome {
    let filter = match PruneFilter::parse(&req.state) {
        Ok(f) => f,
        Err(e) => {
            warn!("prune route: {e}");
            return RouteOutcome::text(400, e);
        }
    };
    let invoker = match req
        .invoking_session
        .as_deref()
        .map(str::parse::<uuid::Uuid>)
    {
        None => None,
        Some(Ok(u)) => Some(crate::session_manager::record::ManagedSessionId::from(u)),
        Some(Err(_)) => {
            let msg = "invalid invoking_session: not a UUID — refusing the prune rather than \
                       running it without the requested self-exclusion (#6118)"
                .to_string();
            warn!("prune route: {msg}");
            return RouteOutcome::text(400, msg);
        }
    };
    let mgr = state.session_manager().await;
    // `caller: None` — the HTTP route is an operator surface, never a session
    // acting on its own behalf; the #3649 owner gate does not apply here.
    match mgr
        .prune_managed(filter, req.dry_run, req.include_active, None, invoker)
        .await
    {
        Ok(outcome) => RouteOutcome::ok(&outcome),
        Err(e) => RouteOutcome::text(500, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

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

    /// 🔴 #6118 FAIL-OPEN GUARD: an unparseable `invoking_session` REFUSES the
    /// prune rather than running it without the self-exclusion.
    ///
    /// Why: the field's whole purpose is keeping the sweep off the caller's own
    /// session. Dropping a malformed value and proceeding would run exactly the
    /// sweep the caller asked to be protected from — the measured `--state all
    /// --include-active` case that selected the operator's own running session.
    /// A 400 costs one retry; the open arm costs the terminal the command was
    /// typed in.
    /// What: posts a well-formed body whose `invoking_session` is not a UUID and
    /// asserts 400. The CONTROL sends the same body with the field ABSENT and
    /// asserts it is accepted, so the test cannot pass merely because the route
    /// rejects everything.
    /// #6806: the same refusal for the WORKTREE sweep's caller id.
    ///
    /// Why: dropping a malformed value here leaves every claim foreign, so the
    /// caller's own worktrees come back blocked by a stranger — the shipped
    /// defect, silently reinstated. The failure arm has to be louder than the
    /// success arm it protects.
    /// What: posts a body whose `invoking_session` is not a UUID and asserts
    /// 400. The CONTROL sends the field ABSENT and asserts acceptance, so the
    /// test cannot pass merely because the route rejects everything.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn prune_worktrees_route_rejects_an_unparseable_invoking_session() {
        let root = crate::test_support::hermetic_temp_dir();
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

        let bad = prune_worktrees_route(
            State(state.clone()),
            Json(PruneWorktreesRequest {
                dry_run: true,
                discard_dirty: false,
                merged_prs: false,
                invoking_session: Some("not-a-uuid".into()),
                project_root: None,
                only_orphan_paths: None,
                only_merged_paths: None,
                only_discard_paths: None,
                detach: false,
            }),
        )
        .await
        .into_response();
        assert_eq!(
            bad.status(),
            StatusCode::BAD_REQUEST,
            "a malformed caller id must stop the sweep, not be dropped"
        );

        // CONTROL: absent is legitimate (an ad-hoc shell, the MCP tool).
        let ok = prune_worktrees_route(
            State(state),
            Json(PruneWorktreesRequest {
                dry_run: true,
                discard_dirty: false,
                merged_prs: false,
                invoking_session: None,
                project_root: None,
                only_orphan_paths: None,
                only_merged_paths: None,
                only_discard_paths: None,
                detach: false,
            }),
        )
        .await
        .into_response();
        assert_ne!(
            ok.status(),
            StatusCode::BAD_REQUEST,
            "an absent caller id is legitimate and must be accepted"
        );
    }

    /// 🔴 #8782: a `project_root` that does not resolve is a 400, never a
    /// daemon-global sweep.
    ///
    /// Fails when the route reads the unresolvable root as "no scope"
    /// (`WorktreeScope::all()` in the `Err` arm): it then answers 200 over
    /// every project. A dry run, so even that arm deletes nothing.
    #[tokio::test]
    async fn prune_worktrees_route_rejects_an_unresolvable_project_root() {
        let root = crate::test_support::hermetic_temp_dir();
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let resp = prune_worktrees_route(
            State(state),
            Json(PruneWorktreesRequest {
                dry_run: true,
                discard_dirty: false,
                merged_prs: false,
                invoking_session: None,
                project_root: Some("/nonexistent/8782/project".into()),
                only_orphan_paths: None,
                only_merged_paths: None,
                only_discard_paths: None,
                detach: false,
            }),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// A `--merged-prs` preview request, the shape the CLI's first POST sends.
    fn merged_prs_preview() -> PruneWorktreesRequest {
        PruneWorktreesRequest {
            dry_run: true,
            discard_dirty: false,
            merged_prs: true,
            invoking_session: None,
            project_root: None,
            only_orphan_paths: None,
            only_merged_paths: None,
            only_discard_paths: None,
            detach: false,
        }
    }

    /// A lane that outlives the test: the route moves the permit into a task, so the
    /// lane must be `'static` (#8994).
    fn leaked_lane() -> &'static tokio::sync::Semaphore {
        Box::leak(Box::new(tokio::sync::Semaphore::new(1)))
    }

    /// The status and text body of a route outcome.
    async fn status_and_text(outcome: RouteOutcome) -> (StatusCode, String) {
        let resp = outcome.into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("route body must be readable");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 🔴 #8994: while the timer sweep holds the maintenance lane, a
    /// `--merged-prs` prune does not start a pass; it answers 409 naming why.
    ///
    /// Fails when the route skips the permit: it then runs both passes and
    /// answers 200. A dry run on a local lane, so even that arm deletes nothing
    /// and no other test's use of the global lane can interfere.
    #[allow(clippy::await_holding_lock)]
    #[serial_test::serial]
    #[tokio::test]
    async fn a_merged_prs_prune_is_refused_while_the_timer_sweep_holds_the_lane() {
        let root = crate::test_support::hermetic_temp_dir();
        // #8994: a regressed route must never scan the operator's real fleet or
        // call a real `gh`; the pinned root names an empty directory.
        let _env = crate::core::trusty_tools_config::WorkspaceRootEnv::pin(root.path());
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let lane = leaked_lane();
        // The timer sweep's own acquisition (`run_one_tick`).
        let _timer = lane.acquire().await.expect("a fresh lane is open");

        let (status, text) =
            status_and_text(prune_worktrees_core_in(&state, merged_prs_preview(), lane).await)
                .await;
        assert_eq!(status, StatusCode::CONFLICT, "body: {text}");
        assert!(
            text.contains("in progress") && text.contains("nothing was pruned"),
            "the refusal must say why and that nothing ran: {text}"
        );
    }

    /// 🔴 #8994 FAIL-CLOSED: a lane that is closed — not held by another
    /// pass — stops the `--merged-prs` prune with a 500 before any work.
    ///
    /// Fails when the route proceeds without a permit on any refusal other
    /// than busy: it then answers 200 having run both passes.
    #[allow(clippy::await_holding_lock)]
    #[serial_test::serial]
    #[tokio::test]
    async fn a_merged_prs_prune_fails_closed_on_a_closed_lane() {
        let root = crate::test_support::hermetic_temp_dir();
        // #8994: a regressed route must never scan the operator's real fleet or
        // call a real `gh`; the pinned root names an empty directory.
        let _env = crate::core::trusty_tools_config::WorkspaceRootEnv::pin(root.path());
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let lane = leaked_lane();
        lane.close();

        let (status, text) =
            status_and_text(prune_worktrees_core_in(&state, merged_prs_preview(), lane).await)
                .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body: {text}");
        assert!(
            text.contains("lane is closed") && text.contains("nothing was pruned"),
            "the refusal must say the lane is closed and that nothing ran: {text}"
        );
    }

    /// 🔴 #8994: a client that disconnects mid-pass drops the handler future, but
    /// the blocking reclaim and orphan loops keep removing worktrees, so the
    /// lane must stay held until the pass ends or the timer sweep starts a
    /// second pass beside it.
    ///
    /// The pass is held open by the session store's write lock, which its first
    /// step (`SessionManager::list`) needs. The route future is dropped by a
    /// timeout while blocked there; the lane must still refuse, and must free
    /// once the store is released and the pass finishes.
    ///
    /// Fails when the permit is held by the handler future (the pre-fix shape)
    /// or dropped at once (`let _ = permit`): the lane is then free after the drop.
    /// Test: this function IS the test.
    #[allow(clippy::await_holding_lock)]
    #[serial_test::serial]
    #[tokio::test]
    async fn a_dropped_prune_request_keeps_the_lane_until_the_pass_ends() {
        let root = crate::test_support::hermetic_temp_dir();
        let _env = crate::core::trusty_tools_config::WorkspaceRootEnv::pin(root.path());
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let lane = leaked_lane();
        let mgr = state.session_manager().await;
        let gate = mgr.store.write().await;

        let dropped = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            prune_worktrees_core_in(&state, merged_prs_preview(), lane),
        )
        .await;
        assert!(
            dropped.is_err(),
            "the pass must still be blocked at the gate"
        );
        tokio::task::yield_now().await;
        assert!(
            matches!(
                lane.try_acquire(),
                Err(tokio::sync::TryAcquireError::NoPermits)
            ),
            "the lane must stay held after the handler future is dropped mid-pass"
        );

        drop(gate);
        let freed = tokio::time::timeout(std::time::Duration::from_secs(30), lane.acquire()).await;
        assert!(freed.is_ok(), "the lane must free once the pass ends");
    }

    /// 🔴 #7619: a dropped HTTP request cancels its pass — the orphans it
    /// would have removed are still on disk when the pass ends, and the lane
    /// it held is released.
    ///
    /// The pass is parked on the store's write lock, as in the test above,
    /// and the route future is dropped there. Fails with the drop guard
    /// removed (the pre-#7619 shape): the detached pass removes the orphans.
    #[allow(clippy::await_holding_lock)]
    #[serial_test::serial]
    #[tokio::test]
    async fn worktree_7619_a_dropped_prune_request_removes_nothing_further() {
        use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
        let fx = GitWorktreeFixture::new();
        let trees: Vec<std::path::PathBuf> = (0..3)
            .map(|i| {
                let wt = fx.add_worktree(&format!("dropped-7619-{i}"));
                GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
                wt
            })
            .collect();
        let root = crate::test_support::hermetic_temp_dir();
        let _env = crate::core::trusty_tools_config::WorkspaceRootEnv::pin(&fx.repos_root);
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let lane = leaked_lane();
        let mgr = state.session_manager().await;
        let gate = mgr.store.write().await;
        // A real removal with the merged-PR pass, so the lane marks the end.
        let req = PruneWorktreesRequest {
            dry_run: false,
            ..merged_prs_preview()
        };

        let dropped = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            prune_worktrees_core_in(&state, req, lane),
        )
        .await;
        assert!(dropped.is_err(), "the pass must be parked at the gate");

        drop(gate);
        let freed = tokio::time::timeout(std::time::Duration::from_secs(60), lane.acquire()).await;
        assert!(
            freed.is_ok(),
            "the lane must free once the cancelled pass stops"
        );
        for wt in &trees {
            assert!(
                wt.exists(),
                "the pass kept deleting after its request was dropped: {}",
                wt.display()
            );
        }
    }

    /// Test: this function IS the test.
    #[tokio::test]
    async fn prune_route_rejects_an_unparseable_invoking_session() {
        let root = crate::test_support::hermetic_temp_dir();
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

        let bad = prune_managed_route(
            State(state.clone()),
            Json(PruneRequest {
                state: "unresolvable".into(),
                dry_run: true,
                include_active: false,
                invoking_session: Some("not-a-uuid".into()),
            }),
        )
        .await
        .into_response();
        assert_eq!(
            bad.status(),
            StatusCode::BAD_REQUEST,
            "a malformed self-exclusion must stop the prune, not be dropped"
        );

        // CONTROL: absent is legitimate (an ad-hoc shell, the MCP tool).
        let ok = prune_managed_route(
            State(state),
            Json(PruneRequest {
                state: "unresolvable".into(),
                dry_run: true,
                include_active: false,
                invoking_session: None,
            }),
        )
        .await
        .into_response();
        assert_eq!(ok.status(), StatusCode::OK);
    }

    /// #6118: the ephemeral route's `dry_run` is honoured AND echoed.
    ///
    /// Why: the echo is what lets the CLI tell a real preview from an older
    /// daemon that dropped the param and swept for real. A route that honours
    /// the flag but does not report it is indistinguishable from one that
    /// ignored it.
    /// What: posts `?dry_run=true` against an empty store and asserts both keys
    /// come back, with `dry_run: true`.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn ephemeral_route_dry_run_reports_without_tearing_down() {
        let root = crate::test_support::hermetic_temp_dir();
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let body = body_json(
            decommission_ephemeral_route(
                State(state),
                axum::extract::Query(EphemeralQuery { dry_run: true }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(
            body.get("dry_run").and_then(serde_json::Value::as_bool),
            Some(true),
            "the route must echo the honoured preview flag: {body}"
        );
        assert_eq!(
            body.get("decommissioned")
                .and_then(serde_json::Value::as_u64),
            Some(0),
            "an empty store sweeps nothing: {body}"
        );
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
            .prune_orphaned_worktrees(&fx.repos_root, &[], true, DirtyWorktreePolicy::Skip, &[])
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
            context_usage: None,
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
                    invoking_session: None,
                    project_root: None,
                    only_orphan_paths: None,
                    only_merged_paths: None,
                    only_discard_paths: None,
                    detach: false,
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
