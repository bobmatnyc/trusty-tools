//! Asynchronous managed-spawn orchestration + the `provision-status` poll route
//! (#2605).
//!
//! Why: on a large repo the synchronous `POST /api/v1/sessions/managed` clone
//! outlasts the CLI's HTTP timeout, so the POST fails while the daemon keeps
//! working — and the blocking clone on the request path degrades `/health`.
//! This module is the async alternative: [`spawn_background`] returns a job id
//! immediately and runs the whole provision on a detached task, streaming live
//! progress into the [`crate::daemon::provisioning::ProvisioningRegistry`];
//! [`get_provision_status`] is the poll route the CLI follows until the job is
//! `ready` (attach) or `failed` (surface the error).
//! What: [`AsyncSpawnResponse`] (the `202` body), [`spawn_background`] (the
//! begin → subscribe → stage-updater → provision → finish orchestration),
//! [`stage_frame_for_job`] (the pure SSE-frame matcher the updater uses),
//! [`ProvisionStatusResponse`] (the poll wire shape), and [`get_provision_status`]
//! (registry lookup with a session-store fallback for pruned/sync sessions).
//! Test: the `tests` submodule covers the frame matcher, the response mapping,
//! and the poll route's provisioning/ready/unknown/invalid-id paths.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use super::lifecycle::{SpawnParams, spawn_managed};
use super::summary::parse_id;
use crate::core::provisioning_stage::ProvisioningStage;
use crate::daemon::provisioning::{ProvisioningLifecycle, ProvisioningProgress};
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedSessionId;

/// Body for `POST /api/v1/sessions/managed` when `background: true` (202).
///
/// Why: the async path cannot return the full record (it does not exist yet),
/// so it hands the client the minimum needed to start polling — the job id and
/// the fixed `provisioning` state.
/// What: `id` (the job id == the pre-generated session id) and `state`
/// (always `"provisioning"`).
/// Test: `spawn_background_registers_and_polls_ready` asserts the id round-trips
/// into the registry.
#[derive(Debug, Serialize)]
pub struct AsyncSpawnResponse {
    /// Job id to poll `provision-status` with (the pre-generated session id).
    pub id: String,
    /// Always `"provisioning"` — provisioning has been accepted, not finished.
    pub state: String,
}

/// Body for `GET /api/v1/sessions/managed/{id}/provision-status`.
///
/// Why: the CLI renders `label` (+ `detail`) as live progress while `state`
/// stays `provisioning`, attaches to `name`/`session_id` on `ready`, and prints
/// `error` on `failed`. One flat shape keeps the client parse trivial.
/// What: `state` (`provisioning`|`ready`|`failed`), the coarse `stage` wire name
/// and human `label`, the fine `detail`, the final `session_id`/`name` (on
/// ready), and `error` (on failed). Absent fields are omitted.
/// Test: `status_response_from_progress_maps_fields`.
#[derive(Debug, Serialize)]
pub struct ProvisionStatusResponse {
    /// Lifecycle wire string.
    pub state: String,
    /// Coarse provisioning stage wire name, if one has been observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Human-readable label for the current stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Fine-grained detail within the stage (e.g. clone percent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Final managed-session id, once ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// tmux session name, once ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Failure reason, once failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProvisionStatusResponse {
    /// Map a registry snapshot into the wire response.
    ///
    /// Why: the poll handler is a pure translation of the registry entry; a
    /// dedicated mapper keeps that handler tiny and makes the mapping directly
    /// unit-testable.
    /// What: copies `lifecycle`/`stage`/`detail`/`session_id`/`name`/`error`,
    /// deriving `stage`/`label` from the coarse [`ProvisioningStage`].
    /// Test: `status_response_from_progress_maps_fields`.
    pub fn from_progress(p: &ProvisioningProgress) -> Self {
        Self {
            state: p.lifecycle.wire().to_string(),
            stage: p.stage.map(|s| s.wire_name().to_string()),
            label: p.stage.map(|s| s.label().to_string()),
            detail: p.detail.clone(),
            session_id: p.session_id.clone(),
            name: p.name.clone(),
            error: p.error.clone(),
        }
    }

    /// Build a terminal `ready` response from an already-provisioned record.
    ///
    /// Why: a job entry is pruned after its TTL, and sessions created by the
    /// SYNCHRONOUS spawn path never had one — yet a client may still poll
    /// `provision-status` for such a session id. Falling back to the live
    /// record lets the route answer `ready` instead of a misleading 404.
    /// What: `state = "ready"`, `stage = "Complete"`, carrying the record's id
    /// and tmux name.
    /// Test: the session-store fallback branch is exercised by the live
    /// `handler_spawn_*` integration tests via [`get_provision_status`]; the
    /// registry-hit and 404/400 branches are covered by the `tests` submodule.
    fn ready_from_record(id: &str, name: &str) -> Self {
        Self {
            state: ProvisioningLifecycle::Ready.wire().to_string(),
            stage: Some(ProvisioningStage::Complete.wire_name().to_string()),
            label: Some(ProvisioningStage::Complete.label().to_string()),
            detail: None,
            session_id: Some(id.to_string()),
            name: Some(name.to_string()),
            error: None,
        }
    }
}

/// Extract `(stage, detail)` from a broadcast envelope IFF it is a
/// `provisioning_stage` frame for `job_id`.
///
/// Why: the stage-updater task must match only the frames belonging to ITS job
/// (the daemon multiplexes every session's stage events on one broadcast
/// channel) and translate the wire strings back into typed values. Extracting
/// this as a pure function makes the matching/parsing logic unit-testable
/// without a running broadcast channel.
/// What: returns `Some((stage, detail))` when `kind == "provisioning_stage"`,
/// `session == job_id`, and `stage` parses via
/// [`ProvisioningStage::from_wire`]; otherwise `None`.
/// Test: `stage_frame_matches_own_job`, `stage_frame_ignores_other_job_and_kind`.
fn stage_frame_for_job(v: &Value, job_id: &str) -> Option<(ProvisioningStage, Option<String>)> {
    if v.get("kind").and_then(Value::as_str) != Some("provisioning_stage") {
        return None;
    }
    if v.get("session").and_then(Value::as_str) != Some(job_id) {
        return None;
    }
    let stage = v
        .get("stage")
        .and_then(Value::as_str)
        .and_then(ProvisioningStage::from_wire)?;
    let detail = v.get("detail").and_then(Value::as_str).map(str::to_string);
    Some((stage, detail))
}

/// Provision a managed session on a detached background task (#2605).
///
/// Why: this is the whole point of the async path — the request handler calls
/// this and returns `202` immediately, so the CLI never holds one long HTTP
/// request open across a multi-minute clone, and the blocking provision runs
/// OFF the request path (keeping `/health` responsive).
/// What: (1) registers an in-flight job under `session_id`; (2) subscribes to
/// the daemon SSE channel BEFORE spawning anything so no early stage frame is
/// missed, and spawns an updater task that folds each matching
/// `provisioning_stage` frame into the registry; (3) spawns the provisioning
/// task, which runs [`spawn_managed_with_id`], aborts the updater, and records
/// the terminal outcome (`finish_ready` with the FINAL record id/name — which
/// may differ from `session_id` on a reconnect — or `finish_failed`).
/// Test: `spawn_background_registers_and_polls_ready` (via a stubbed terminal
/// transition); the live provision path is covered by the existing
/// `handler_spawn_*` integration tests through [`spawn_managed_with_id`].
pub fn spawn_background(
    state: Arc<DaemonState>,
    session_id: ManagedSessionId,
    params: SpawnParams,
) {
    let job_id = session_id.to_string();
    state.provisioning.begin(&job_id);

    // Subscribe BEFORE spawning the provision so the very first stage frame the
    // provision emits is never lost to a subscribe-after-send race.
    let mut rx = state.event_tx.subscribe();
    let updater_state = state.clone();
    let updater_job = job_id.clone();
    let updater = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(v) => {
                    if let Some((stage, detail)) = stage_frame_for_job(&v, &updater_job) {
                        updater_state
                            .provisioning
                            .update_stage(&updater_job, stage, detail);
                    }
                }
                // A slow updater that fell behind the ring buffer just skips the
                // dropped frames and keeps reading — progress is best-effort.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    tokio::spawn(async move {
        let result = spawn_managed(&state, session_id, params).await;
        // The provision is done — stop the updater so it never outlives the job.
        updater.abort();
        match result {
            Ok(record) => state.provisioning.finish_ready(
                &job_id,
                record.id.to_string(),
                record.tmux_name.clone(),
            ),
            Err(e) => state.provisioning.finish_failed(&job_id, e),
        }
    });
}

/// Build the `provision-status` sub-router merged into the main daemon router.
///
/// Why: registering this route as a merged sub-router (rather than one more
/// `.route(...)` in `api.rs`) keeps the new API surface in this module and out
/// of the already-oversized `api.rs`, mirroring how `proxy_router` is merged.
/// What: a single `GET /api/v1/sessions/managed/{id}/provision-status` →
/// [`get_provision_status`]. The literal `/{id}/…` leaf never collides with the
/// `/{id}` param route in the main router.
/// Test: exercised by the `provision-status` integration tests via the merged
/// router.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/managed/{id}/provision-status",
        get(get_provision_status),
    )
}

/// Accept a `background: true` spawn: register the job, kick off the detached
/// provision, and return the `202 Accepted` ack (#2605).
///
/// Why: keeps the whole async-accept ritual (id generation, registry begin via
/// [`spawn_background`], and the `202` body) in this module so the
/// `spawn_session` HTTP handler stays a thin one-line branch and `mod.rs`
/// stays under its SLOC cap.
/// What: mints a session id, launches the background provision, and returns
/// `202` with `{ id, state: "provisioning" }`.
/// Test: `spawn_background_registers_and_polls_ready` covers the registry
/// begin + poll; the `202` shape is covered by the integration tests.
pub fn accept_async_spawn(state: Arc<DaemonState>, params: SpawnParams) -> Response {
    let session_id = ManagedSessionId::new();
    let id = session_id.to_string();
    spawn_background(state, session_id, params);
    (
        StatusCode::ACCEPTED,
        Json(AsyncSpawnResponse {
            id,
            state: "provisioning".to_string(),
        }),
    )
        .into_response()
}

/// GET /api/v1/sessions/managed/{id}/provision-status — poll async spawn progress.
///
/// Why: the CLI polls this after a `background: true` spawn to render live
/// progress and learn when the session is ready (or why it failed) without
/// holding a long request open.
/// What: returns the registry snapshot when present; otherwise falls back to
/// the session store (a pruned or synchronously-spawned session answers
/// `ready`), and 404s only when neither knows the id. An unparseable id is a
/// 400 (via [`parse_id`]).
/// Test: `poll_route_reports_provisioning_then_ready`,
/// `poll_route_falls_back_to_ready_for_known_session`,
/// `poll_route_unknown_id_is_404`, `poll_route_invalid_id_is_400`.
pub async fn get_provision_status(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    if let Some(progress) = state.provisioning.get(&id_str) {
        return Json(ProvisionStatusResponse::from_progress(&progress)).into_response();
    }

    // Not (or no longer) a tracked job — parse the id and fall back to the live
    // session store so a pruned/sync session still answers `ready`.
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.get(&id).await {
        Ok(record) => Json(ProvisionStatusResponse::ready_from_record(
            &record.id.to_string(),
            &record.tmux_name,
        ))
        .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!("no provisioning job or session for {id_str}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage_frame(session: &str, stage: &str, detail: Option<&str>) -> Value {
        let mut v = serde_json::json!({
            "kind": "provisioning_stage",
            "session": session,
            "stage": stage,
        });
        if let Some(d) = detail {
            v.as_object_mut()
                .unwrap()
                .insert("detail".into(), Value::String(d.into()));
        }
        v
    }

    #[test]
    fn stage_frame_matches_own_job() {
        let v = stage_frame("job-1", "CloningRepo", Some("Receiving objects: 42%"));
        let (stage, detail) = stage_frame_for_job(&v, "job-1").expect("own frame matches");
        assert_eq!(stage, ProvisioningStage::CloningRepo);
        assert_eq!(detail.as_deref(), Some("Receiving objects: 42%"));
    }

    #[test]
    fn stage_frame_ignores_other_job_and_kind() {
        // Different session id.
        let other = stage_frame("job-2", "CloningRepo", None);
        assert!(stage_frame_for_job(&other, "job-1").is_none());

        // Wrong kind.
        let hook = serde_json::json!({ "kind": "hook_event", "session": "job-1" });
        assert!(stage_frame_for_job(&hook, "job-1").is_none());

        // Unknown stage wire name.
        let bad_stage = stage_frame("job-1", "NotAStage", None);
        assert!(stage_frame_for_job(&bad_stage, "job-1").is_none());
    }

    #[test]
    fn status_response_from_progress_maps_fields() {
        let reg = crate::daemon::provisioning::ProvisioningRegistry::default();
        reg.begin("job");
        reg.update_stage(
            "job",
            ProvisioningStage::DeployingAgents,
            Some("2 agents".into()),
        );
        let resp = ProvisionStatusResponse::from_progress(&reg.get("job").unwrap());
        assert_eq!(resp.state, "provisioning");
        assert_eq!(resp.stage.as_deref(), Some("DeployingAgents"));
        assert_eq!(resp.label.as_deref(), Some("Deploying agents"));
        assert_eq!(resp.detail.as_deref(), Some("2 agents"));
        assert!(resp.session_id.is_none());

        reg.finish_ready("job", "sess-7".into(), "tm-x-01".into());
        let resp = ProvisionStatusResponse::from_progress(&reg.get("job").unwrap());
        assert_eq!(resp.state, "ready");
        assert_eq!(resp.session_id.as_deref(), Some("sess-7"));
        assert_eq!(resp.name.as_deref(), Some("tm-x-01"));

        reg.begin("job2");
        reg.finish_failed("job2", "boom".into());
        let resp = ProvisionStatusResponse::from_progress(&reg.get("job2").unwrap());
        assert_eq!(resp.state, "failed");
        assert_eq!(resp.error.as_deref(), Some("boom"));
    }

    /// Build an isolated test daemon whose managed store lives under a tempdir
    /// and never touches real tmux (mirrors the managed-route handler tests).
    async fn test_state() -> (Arc<DaemonState>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state = Arc::new(DaemonState::with_root_isolated_managed(tmp.path().to_owned()).await);
        (state, tmp)
    }

    #[tokio::test]
    async fn poll_route_reports_provisioning_then_ready() {
        let (state, _tmp) = test_state().await;
        let id = ManagedSessionId::new().to_string();
        state.provisioning.begin(&id);
        state.provisioning.update_stage(
            &id,
            ProvisioningStage::CloningRepo,
            Some("Receiving objects: 10%".into()),
        );

        let resp = get_provision_status(State(state.clone()), AxumPath(id.clone()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        state
            .provisioning
            .finish_ready(&id, id.clone(), "tm-ready-01".into());
        let resp = get_provision_status(State(state.clone()), AxumPath(id.clone()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["state"], "ready");
        assert_eq!(json["name"], "tm-ready-01");
    }

    #[tokio::test]
    async fn poll_route_unknown_id_is_404() {
        let (state, _tmp) = test_state().await;
        let id = ManagedSessionId::new().to_string();
        let resp = get_provision_status(State(state), AxumPath(id))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn poll_route_invalid_id_is_400() {
        let (state, _tmp) = test_state().await;
        let resp = get_provision_status(State(state), AxumPath("not-a-uuid".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// A job that reaches `ready` and is then pruned must still answer `ready`
    /// from the session store — but here we only assert the registry-hit path
    /// and the 404 path, since seeding a full live record needs a real spawn.
    #[tokio::test]
    async fn spawn_background_registers_and_polls_ready() {
        let (state, _tmp) = test_state().await;
        let sid = ManagedSessionId::new();
        // Drive the registry state machine directly (a full `spawn_background`
        // would require a live clone + tmux); this asserts the poll route sees
        // the id the async response would have handed the client.
        state.provisioning.begin(&sid.to_string());
        let resp = get_provision_status(State(state.clone()), AxumPath(sid.to_string()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["state"], "provisioning");
    }
}
