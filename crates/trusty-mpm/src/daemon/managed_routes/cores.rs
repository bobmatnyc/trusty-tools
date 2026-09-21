//! The transport-neutral bodies of the managed-session routes (#6288 slice 4).
//!
//! Why: each of these routes is served over BOTH axum and the daemon's Unix
//! socket, and the bar is one implementation rather than two that drift. The
//! bodies moved here verbatim from `mod.rs`'s handlers; what stayed there is a
//! wrapper that supplies the axum extractors. Moving them also kept `mod.rs`
//! under its 500-SLOC cap, which it was six lines away from.
//!
//! What: one `*_core` per route, each returning a [`RouteOutcome`]. Behaviour is
//! unchanged from the pre-slice handlers — the same branches, the same statuses,
//! the same message strings.
//!
//! Test: every pre-existing handler test in `tests/session_manager_mvp.rs` and
//! `managed_routes::tests` now runs through these bodies; the socket half is
//! `daemon::rpc::managed_tests`.

use std::sync::Arc;

use tracing::warn;

use super::route_outcome_http::outcome_from_response;
use super::*;
use crate::daemon::rpc::managed::outcome::{CODE_PANE_GONE, CODE_WORKSPACE_GONE, RouteOutcome};

/// Parse a managed-session id, or the 400 the HTTP route answers with.
fn parse_id_neutral(id_str: &str) -> Result<ManagedSessionId, RouteOutcome> {
    parse_id(id_str).map_err(|(code, msg)| RouteOutcome::text(code.as_u16(), msg))
}

/// The 404 body every id-addressed route in this module shares.
fn not_found(id_str: &str) -> RouteOutcome {
    RouteOutcome::text(404, format!("session {id_str} not found"))
}

/// `POST /api/v1/sessions/managed` — see [`super::spawn_session`].
pub(crate) async fn spawn_core(state: &Arc<DaemonState>, req: SpawnRequest) -> RouteOutcome {
    // Reject an invalid runtime selector up front with a 400 (the shared
    // `spawn_managed` helper also rejects it, but doing it here keeps the
    // 400-vs-500 distinction existing clients rely on).
    if let Some(raw) = req.runtime.as_deref()
        && let Err(e) = raw.parse::<RuntimeKind>()
    {
        warn!("spawn_session: invalid runtime selector: {e}");
        return RouteOutcome::text(400, e.to_string());
    }

    // Deliverable linkage (DOC-35 §10.6, #2379): validate BEFORE any
    // provisioning side effect, mirroring the runtime-selector pre-check
    // above — an invalid `--deliverable` must never mint infrastructure for a
    // link that was never going to be recorded.
    if let Some(deliverable_id) = req.deliverable_id.as_deref()
        && let Err(resp) =
            deliverable_link::validate_deliverable_scope(state, &req.repo_url, deliverable_id).await
    {
        return outcome_from_response(resp).await;
    }

    let params = SpawnParams {
        repo_url: req.repo_url,
        git_ref: req.git_ref,
        task: req.task,
        name_hint: req.name_hint,
        runtime: req.runtime,
        ephemeral: req.ephemeral,
        // A client call (`tm launch`/`tm ticket`) — never subject to the MCP
        // spawn gate (#1836/#1837).
        mcp_initiated: false,
        // Turnkey by default (#1903/#1299); a client sets `inject_task: false`
        // to opt into the legacy metadata-only behavior (`--no-inject`).
        inject_task: req.inject_task,
        deliverable_id: req.deliverable_id,
        // Force-new opt-out (#2450): a client (the picker's "launch new
        // session", `tm session new`) sets `force_new: true` to skip the
        // in-project reconnect pre-flight and always spawn fresh.
        force_new: req.force_new,
        // #5274: the explicit "give this session its own worktree" request.
        worktree: req.worktree,
    };

    // Async path (#2605): provision on a detached task and return the job id
    // immediately, so a large-repo clone never outlasts the client's timeout
    // and the blocking provision runs OFF the request path.
    if req.background {
        return provision_status::accept_async_spawn(Arc::clone(state), params);
    }

    match spawn_managed(state, ManagedSessionId::new(), params).await {
        Ok(final_record) => {
            let resp = SpawnResponse {
                id: final_record.id.to_string(),
                name: final_record.tmux_name.clone(),
                workspace_path: final_record
                    .workspace_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                repo_url: final_record.repo_url.clone(),
                branch: final_record.branch.clone(),
                state: final_record.state.to_string(),
                created_at: final_record.created_at.to_rfc3339(),
                attach_cmd: attach_cmd_for(&final_record.tmux_name),
                runtime: final_record.runtime.as_str().to_owned(),
                deliverable_id: final_record.deliverable_id.map(|id| id.to_string()),
            };
            RouteOutcome::created(&resp)
        }
        Err(e) => RouteOutcome::text(500, e),
    }
}

/// `POST /api/v1/sessions/managed/adopt` — see [`super::adopt_existing_session`].
pub(crate) async fn adopt_core(
    state: &Arc<DaemonState>,
    req: AdoptExistingRequest,
) -> RouteOutcome {
    // Resolve the runtime selector up front so a typo is a 400, not a 500.
    let runtime = match req.runtime.as_deref() {
        None => RuntimeKind::default(),
        Some(raw) => match raw.parse::<RuntimeKind>() {
            Ok(rk) => rk,
            Err(e) => {
                warn!("adopt_existing: invalid runtime selector: {e}");
                return RouteOutcome::text(400, e.to_string());
            }
        },
    };

    let task = req.task.unwrap_or_default();
    let cwd = std::path::PathBuf::from(&req.cwd);

    let mgr = state.session_manager().await;
    match mgr
        .adopt_existing(
            &req.tmux_name,
            cwd,
            task,
            runtime,
            req.ephemeral.unwrap_or(false),
        )
        .await
    {
        Ok(record) => RouteOutcome::created(&AdoptExistingResponse {
            id: record.id.to_string(),
            name: record.tmux_name.clone(),
            state: record.state.to_string(),
            cwd: record.cwd.to_string_lossy().to_string(),
            runtime: record.runtime.as_str().to_owned(),
            attach_cmd: attach_cmd_for(&record.tmux_name),
        }),
        Err(crate::session_manager::ManagedError::TmuxSessionMissing(msg)) => {
            RouteOutcome::text(404, msg)
        }
        Err(crate::session_manager::ManagedError::AlreadyAdopted(msg)) => {
            RouteOutcome::text(409, msg)
        }
        Err(e) => RouteOutcome::text(500, e.to_string()),
    }
}

/// `GET /api/v1/sessions/managed` — see [`super::list_managed_sessions`].
pub(crate) async fn list_core(
    state: &Arc<DaemonState>,
    source_id: Option<&str>,
    slim: bool,
) -> RouteOutcome {
    let mgr = state.session_manager().await;
    // #3034: numbering is observed against the FULL, unfiltered record set
    // BEFORE the `source_id` filter is applied — otherwise a session outside
    // the current filter would go unobserved and receive a fresh number the
    // next time it IS listed.
    let all_records = mgr.list().await;
    let numbered = mgr.numbered_snapshot(&all_records).await;
    let sessions = numbered_summaries(numbered, mgr.tmux.as_ref(), source_id, !slim).await;
    // #5007: if `mgr.list()` just served its last-known in-memory set because
    // the store could not be read, say so in the same response.
    let store_health = mgr.store_health().await.map(|d| StoreHealthPayload {
        message: d.message,
        corrupt: d.corrupt,
        observed_at: d.observed_at.to_rfc3339(),
    });
    RouteOutcome::ok(&ListSessionsResponse {
        sessions,
        store_health,
    })
}

/// `GET /api/v1/sessions/managed/{id}` — see [`super::get_managed_session`].
pub(crate) async fn get_core(state: &Arc<DaemonState>, id_str: &str) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    match mgr.get(&id).await {
        Ok(record) => {
            // Reconcile against live tmux like the list body — this is the
            // record `tm session resume <id>` reads, and its resume decision
            // keys off `state`; serving a stale `stopped` for a live session
            // would let resume destructively recreate the pane.
            let mut summary = record_to_summary_checked(&record).await;
            reconcile_against_tmux(
                mgr.tmux.as_ref(),
                std::slice::from_mut(&mut summary),
                std::slice::from_ref(&record),
            );
            RouteOutcome::ok(&summary)
        }
        Err(_) => not_found(id_str),
    }
}

/// `POST /api/v1/sessions/managed/{id}/send` — see [`super::send_to_session`].
pub(crate) async fn send_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    req: SendInputRequest,
) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    let tmux_name = match mgr.get(&id).await {
        Ok(r) => r.tmux_name,
        Err(_) => return not_found(id_str),
    };
    match mgr.send_input(&id, &req.text).await {
        Ok(()) => RouteOutcome::ok(&SendInputResponse {
            sent: true,
            tmux_name,
        }),
        Err(e) => RouteOutcome::text(500, e.to_string()),
    }
}

/// `POST /api/v1/sessions/managed/{id}/answer` — see
/// [`super::answer_session_decision`].
pub(crate) async fn answer_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    req: AnswerRequest,
) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    let record = match mgr.get(&id).await {
        Ok(r) => r,
        Err(_) => return not_found(id_str),
    };
    let tmux_name = record.tmux_name.clone();

    // FRONT-gate (#1360) path: a session escalated *before* spawn sits in
    // `Provisioning` with a `pending_decision` and NO running harness.
    // Answering it must clear the decision AND launch the withheld runtime
    // (AC-15) — not inject the answer into a bare pane.
    let is_front_gate_escalation = record.state
        == crate::session_manager::ManagedSessionState::Provisioning
        && record.pending_decision.is_some();

    if is_front_gate_escalation {
        if let Err(e) = mgr.clear_pending_decision(&id).await {
            return RouteOutcome::text(500, e.to_string());
        }
        // Re-fetch AFTER clearing: `clear_pending_decision` upserts a new
        // record version, so the `record` captured above is now stale.
        let fresh = match mgr.get(&id).await {
            Ok(r) => r,
            Err(_) => return not_found(id_str),
        };
        match spawn_runtime_for(state, &fresh).await {
            Ok(()) => RouteOutcome::ok(&AnswerResponse {
                injected: true,
                tmux_name,
            }),
            Err(e) => RouteOutcome::text(500, e),
        }
    } else {
        match mgr.answer_decision(&id, &req.answer).await {
            Ok(()) => RouteOutcome::ok(&AnswerResponse {
                injected: true,
                tmux_name,
            }),
            Err(e) => RouteOutcome::text(500, e.to_string()),
        }
    }
}

/// `GET /api/v1/sessions/managed/{id}/attach-cmd` — see
/// [`super::get_attach_cmd`].
pub(crate) async fn attach_cmd_core(state: &Arc<DaemonState>, id_str: &str) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    match mgr.get(&id).await {
        Ok(record) => RouteOutcome::ok(&AttachCmdResponse {
            attach_cmd: attach_cmd_for(&record.tmux_name),
        }),
        Err(_) => not_found(id_str),
    }
}

/// `POST /api/v1/sessions/managed/{id}/runtime-stop` — see
/// [`super::stop_managed_session_runtime`].
pub(crate) async fn runtime_stop_core(state: &Arc<DaemonState>, id_str: &str) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    match mgr.stop(&id).await {
        Ok(record) => RouteOutcome::ok(&record_to_summary(&record)),
        Err(_) => not_found(id_str),
    }
}

/// `POST /api/v1/sessions/managed/{id}/resume` — see
/// [`super::resume_managed_session`].
///
/// The two 422 refusals ([`ResumeManagedError::WorkspaceGone`],
/// [`ResumeManagedError::PaneGone`]) carry a distinct RPC code each, because on
/// HTTP they are told apart by the `x-trusty-resume-reason` header and the
/// socket has no headers (#6288). The HTTP wrapper re-attaches that header from
/// the same code, so neither transport loses the discriminant.
pub(crate) async fn resume_core(state: &Arc<DaemonState>, id_str: &str) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    // Single round-trip: `resume_managed` performs the existence + state check
    // inside `SessionManager::resume` and re-spawns the runtime. No pre-flight
    // `get` (which introduced a TOCTOU race) and no `Display`-substring match.
    match resume_managed(state, &id).await {
        Ok(final_record) => RouteOutcome::ok(&record_to_summary(&final_record)),
        Err(ResumeManagedError::NotFound(id)) => {
            RouteOutcome::text(404, format!("session {id} not found"))
        }
        Err(ResumeManagedError::InvalidState(reason)) => RouteOutcome::text(409, reason),
        // #8233 item 1: nothing failed — another path holds this session's
        // resume. A conflict, never a 500.
        Err(e @ ResumeManagedError::AlreadyResuming(_)) => RouteOutcome::text(409, e.to_string()),
        Err(ResumeManagedError::WorkspaceGone(msg)) => {
            RouteOutcome::text(422, msg).with_rpc_code(CODE_WORKSPACE_GONE)
        }
        Err(ResumeManagedError::PaneGone(msg)) => {
            RouteOutcome::text(422, msg).with_rpc_code(CODE_PANE_GONE)
        }
        Err(ResumeManagedError::Other(msg)) => RouteOutcome::text(500, msg),
    }
}

/// The HTTP status a non-404 decommission failure reports (#7877).
///
/// Why: the #3764 shared-workspace guard and the #3649 worktree-owner gate both
/// REFUSE a decommission the caller asked for while a sibling session still
/// claims the directory. That is a conflict in the caller's request, not a
/// server fault, but every non-404 error used to collapse into a 500 — so the
/// CLI could not tell a legitimate refusal from a crashed daemon, and printed
/// reqwest's bare "HTTP status server error (500 …)" instead of the guard's
/// reason. 409 Conflict is the status the rest of this module already uses for
/// a refused-because-of-current-state request (see `resume_core`'s
/// `InvalidState` arm), so the socket transport's `status_to_rpc_code` already
/// carries it.
/// What: 409 for the two guard refusals, 500 for everything else (an I/O
/// failure, a tmux failure, a store write) — those really are server faults.
/// The message is the error's `Display` in both cases, unchanged.
/// Test: `a_shared_workspace_guard_refusal_is_a_409_not_a_500`,
/// `an_ordinary_decommission_failure_is_still_a_500`.
fn decommission_failure_status(err: &crate::session_manager::ManagedError) -> u16 {
    use crate::session_manager::ManagedError;
    match err {
        ManagedError::ForeignActiveWorkspaceClaim(..) | ManagedError::WorktreeOwnerMismatch(..) => {
            409
        }
        _ => 500,
    }
}

/// `POST /api/v1/sessions/managed/{id}/decommission` — see
/// [`super::decommission_managed_session`].
pub(crate) async fn decommission_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    record_only: bool,
) -> RouteOutcome {
    let id = match parse_id_neutral(id_str) {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let mgr = state.session_manager().await;
    // Pre-fetch the record to obtain the workspace_path BEFORE the tombstone
    // clears it. `decommission` returns `workspace_removed` directly;
    // `workspace_path_was` for the CLI's `git worktree prune` hint must still
    // be captured here since the tombstone nulls it out.
    let pre = mgr.get(&id).await.ok();
    let pre_owned = pre.as_ref().map(|r| r.workspace_owned).unwrap_or(false);
    let pre_ws = pre.and_then(|r| r.workspace_path);
    let outcome = if record_only {
        mgr.decommission_record_only(&id).await
    } else {
        mgr.decommission(&id, None).await
    };
    match outcome {
        Ok((record, workspace_removed)) => {
            // workspace_path_was: only meaningful for owned sessions.
            let workspace_path_was = if pre_owned {
                pre_ws.map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            };
            RouteOutcome::ok(&DecommissionResponse {
                summary: record_to_summary(&record),
                workspace_removed,
                workspace_path_was,
            })
        }
        Err(crate::session_manager::ManagedError::SessionNotFound(_)) => not_found(id_str),
        // #7877: a guard refusal is the caller's conflict, not a server fault.
        Err(e) => RouteOutcome::text(decommission_failure_status(&e), e.to_string()),
    }
}

/// Re-attach the `x-trusty-resume-reason` header a 422 resume refusal carries
/// on HTTP (#6288).
///
/// Why: the header is the CLI's machine-readable discriminant between "the
/// workspace is gone" (safe to `tm session delete --force`) and "the pane is
/// gone" (a live sibling window may still exist, so teardown is dangerous).
/// [`resume_core`] records that same distinction as an RPC code, which is what
/// the socket carries; this reads the code back and restores the header, so the
/// HTTP contract is byte-identical to what it was before the extraction.
/// What: maps [`CODE_WORKSPACE_GONE`] → `workspace_missing` and
/// [`CODE_PANE_GONE`] → `pane_gone`; every other outcome passes through
/// untouched.
/// Test: `resume_http_response_restores_the_reason_header_from_the_rpc_code`
/// in this module's `cores_tests`.
pub(crate) fn resume_http_response(outcome: RouteOutcome) -> axum::response::Response {
    let reason = match outcome.rpc_code {
        Some(CODE_WORKSPACE_GONE) => Some("workspace_missing"),
        Some(CODE_PANE_GONE) => Some("pane_gone"),
        _ => None,
    };
    let mut resp = axum::response::IntoResponse::into_response(outcome);
    if let Some(reason) = reason {
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static("x-trusty-resume-reason"),
            axum::http::HeaderValue::from_static(reason),
        );
    }
    resp
}

#[cfg(test)]
mod cores_tests {
    use super::*;
    use crate::daemon::rpc::managed::outcome::{RouteBody, status_to_rpc_code};
    use crate::session_manager::{ManagedSessionState, SessionRecord};

    /// An `Active` record claiming `workspace_path` — the #1744 collision shape.
    fn active_record_claiming(
        id: ManagedSessionId,
        workspace_path: &std::path::Path,
    ) -> SessionRecord {
        SessionRecord {
            id,
            tmux_name: format!("tm-7877-{id}"),
            cwd: workspace_path.to_path_buf(),
            task: "task".into(),
            state: ManagedSessionState::Active,
            created_at: chrono::Utc::now(),
            last_activity_at: None,
            workspace_path: Some(workspace_path.to_path_buf()),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: None,
            terminal_at: None,
            stop_cause: None,
        }
    }

    /// The #3764 shared-workspace guard answers 409, not 500 (#7877).
    ///
    /// Why: the refusal is the caller's conflict — a sibling `Active` session
    /// still claims the directory — and a 500 made it indistinguishable from a
    /// crashed daemon, so the CLI printed reqwest's bare status line instead of
    /// the guard's reason. This drives the REAL route body against a seeded
    /// store, so it fails on the pre-fix `RouteOutcome::text(500, …)` arm.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn a_shared_workspace_guard_refusal_is_a_409_not_a_500() {
        let data_root = crate::test_support::hermetic_temp_dir();
        let state = Arc::new(
            crate::daemon::state::DaemonState::with_root_isolated_managed(
                data_root.path().to_path_buf(),
            )
            .await,
        );
        let mgr = state.session_manager().await;

        let shared = data_root.path().join("shared-7877-workspace");
        let sibling = ManagedSessionId::new();
        let target = ManagedSessionId::new();
        for id in [sibling, target] {
            mgr.store
                .write()
                .await
                .upsert(active_record_claiming(id, &shared))
                .await
                .expect("seed record");
        }

        let outcome = decommission_core(&state, &target.to_string(), false).await;
        assert_eq!(
            outcome.status, 409,
            "a guard refusal must be a client-side conflict, not a server fault"
        );
        match &outcome.body {
            RouteBody::Text(msg) => assert!(
                msg.contains(&sibling.to_string()),
                "the refusal must name the sibling that blocks it, got {msg}"
            ),
            other => panic!("a refusal body must be text, got {other:?}"),
        }

        // Nothing may have been mutated — the guard runs before every removal.
        assert_eq!(
            mgr.get(&target).await.expect("target record").state,
            ManagedSessionState::Active
        );
    }

    /// Every OTHER decommission failure is still a 500 (#7877 error arm).
    ///
    /// Why: narrowing the guard refusal to 409 must not quietly reclassify a
    /// real server fault — an I/O or tmux failure — as a client error the CLI
    /// would report as "your request conflicts".
    /// Test: this function IS the test.
    #[test]
    fn an_ordinary_decommission_failure_is_still_a_500() {
        let err = crate::session_manager::ManagedError::TmuxUnavailable("no tmux".into());
        assert_eq!(decommission_failure_status(&err), 500);
    }

    /// A 422 resume refusal keeps BOTH discriminants: the RPC code the socket
    /// reads, and the HTTP header the CLI reads (#6288, #2577).
    #[test]
    fn resume_http_response_restores_the_reason_header_from_the_rpc_code() {
        for (code, expected) in [
            (CODE_WORKSPACE_GONE, "workspace_missing"),
            (CODE_PANE_GONE, "pane_gone"),
        ] {
            let resp = resume_http_response(RouteOutcome::text(422, "gone").with_rpc_code(code));
            assert_eq!(resp.status().as_u16(), 422);
            assert_eq!(
                resp.headers()
                    .get("x-trusty-resume-reason")
                    .and_then(|v| v.to_str().ok()),
                Some(expected)
            );
        }
    }

    /// Every other resume outcome is header-free, exactly as before.
    #[test]
    fn resume_http_response_adds_no_header_to_an_ordinary_refusal() {
        let resp = resume_http_response(RouteOutcome::text(404, "session x not found"));
        assert_eq!(resp.status().as_u16(), 404);
        assert!(resp.headers().get("x-trusty-resume-reason").is_none());
    }

    /// The neutral id parser answers the SAME 400 the HTTP route answered.
    #[test]
    fn an_unparseable_id_is_a_400_on_both_transports() {
        let refusal = parse_id_neutral("not-a-uuid").expect_err("must refuse");
        assert_eq!(refusal.status, 400);
        assert_eq!(
            status_to_rpc_code(refusal.status),
            trusty_common::uds::server::CODE_INVALID_PARAMS
        );
    }
}
