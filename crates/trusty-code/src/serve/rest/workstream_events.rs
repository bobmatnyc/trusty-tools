//! `GET /workstreams/{id}/events` — workstream-level SSE aggregation route
//! (DOC-48 §5.3, issue #3297, epic #3292).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft) §5.3
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
//!
//! Why: `crate::serve::http::session_events_sse` is the per-session SSE
//! primitive (replay + live, session-scoped per AC-7.3); this route composes
//! it into a single per-WORKSTREAM stream by fanning out over the
//! workstream's bound `session_ids` (§5.3 steps 1-3) via
//! `crate::workstreams::events::aggregate`, merged with the daemon-global
//! workstream-level bus (`WorkstreamActivationChanged`/
//! `WorkstreamStateInferred`). Kept in its own file (mirroring every other
//! `rest::*` resource group) rather than inline in `crate::serve::http` —
//! that file is already near this crate's 500-SLOC production-file cap.
//! Looks the workstream up via [`super::call`] against the already-registered
//! `workstream.get` RPC method (not `WorkstreamStore` directly) so a REST-only
//! client and this SSE route can never disagree about what "the workstream's
//! current session_ids" means, and so this route needs no new plumbing to
//! reach the store beyond the `Arc<Router>` every other `rest::*` group
//! already receives.
//!
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `WorkstreamEventsState { router, sessions }`, `with_state`-erased) mapping
//! `GET /workstreams/{id}/events` to [`workstream_events_sse`]. 404 (mapped
//! from `workstream.get`'s `-32002 not_found`) if the workstream doesn't
//! exist; otherwise an SSE stream that never terminates on its own (client
//! disconnect is "detach", exactly like `session_events_sse`) — including for
//! a workstream with an EMPTY `session_ids` (§5.3: "stream stays open,
//! workstream-level events only", proven by
//! `tests::zero_session_workstream_stream_stays_open`).
//!
//! `crate::serve::http::build_axum_router` merges this in alongside every
//! other REST resource group.
//! Test: `tests::*`.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router as AxumRouter};
use futures_util::{Stream, StreamExt};
use serde_json::json;
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;
use crate::session::SessionRegistry;
use crate::workstreams::events::{
    RegistrySessionEventSource, WorkstreamEventEnvelope, aggregate, subscribe_workstream_bus,
};

use super::{call, rpc_error_to_status, throwaway_ctx};

/// Shared axum state: the JSON-RPC router (to resolve the workstream via
/// `workstream.get`) and the session registry (the concrete
/// [`RegistrySessionEventSource`] this route's fan-out reads from).
#[derive(Clone)]
struct WorkstreamEventsState {
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
}

/// Build the `GET /workstreams/{id}/events` route group.
///
/// Why: kept separate from `crate::serve::rest::workstreams` (the CRUD/
/// activation REST group) since this route needs `Arc<SessionRegistry>` in
/// its state alongside the router, unlike every other `workstream.*` REST
/// handler (which only ever calls through [`super::respond`]).
/// Test: `tests::*`.
pub fn routes(router: Arc<Router>, sessions: Arc<SessionRegistry>) -> AxumRouter {
    AxumRouter::new()
        .route("/workstreams/{id}/events", get(workstream_events_sse))
        .with_state(WorkstreamEventsState { router, sessions })
}

/// `GET /workstreams/{id}/events` handler (DOC-48 §5.3).
///
/// Why: see module docs for the fan-out architecture.
/// What: 404 with a JSON-RPC error envelope if `id` names no workstream
/// (`workstream.get` -> `-32002 not_found`, mapped via
/// [`rpc_error_to_status`]). Otherwise reads the workstream's current
/// `session_ids` from that SAME `workstream.get` response (a snapshot at
/// subscribe time — see `crate::workstreams::events` module docs for why
/// membership is static-at-subscribe-time in this ticket), builds one
/// [`RegistrySessionEventSource`]-backed stream per bound session plus the
/// workstream-level bus stream, and merges them via
/// `crate::workstreams::events::aggregate` into one SSE response.
/// Test: `tests::events_route_streams_replay_from_bound_session`,
/// `tests::events_route_unknown_workstream_returns_404`,
/// `tests::zero_session_workstream_stream_stays_open`.
async fn workstream_events_sse(
    State(state): State<WorkstreamEventsState>,
    Path(workstream_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, (StatusCode, Json<Response>)> {
    let ws = call(
        &state.router,
        "workstream.get",
        json!({"id": workstream_id}),
        &throwaway_ctx(),
    )
    .await
    .map_err(|e| {
        let status = rpc_error_to_status(&e);
        (status, Json(Response::err(None, e.code, e.message)))
    })?;

    let session_ids: Vec<String> = ws["session_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let source = RegistrySessionEventSource::new(state.sessions.clone());
    let merged = aggregate(&source, &session_ids, subscribe_workstream_bus());

    Ok(
        Sse::new(merged.map(|envelope| Ok(sse_event_for(&envelope))))
            .keep_alive(KeepAlive::default()),
    )
}

/// Serialise one [`WorkstreamEventEnvelope`] as an SSE `data:` frame —
/// mirrors `crate::serve::http::sse_event_for`'s fallback-on-serialize-error
/// convention.
fn sse_event_for(envelope: &WorkstreamEventEnvelope) -> SseEvent {
    SseEvent::default()
        .json_data(envelope)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn build(sessions: Arc<SessionRegistry>, workstreams_router: Router) -> AxumRouter {
        routes(Arc::new(workstreams_router), sessions)
    }

    async fn router_with_workstream() -> (AxumRouter, String, Arc<SessionRegistry>) {
        let sessions = Arc::new(SessionRegistry::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::workstreams::WorkstreamStore::load(dir.path().join("workstreams-test.json"))
                .await
                .expect("load");
        std::mem::forget(dir);
        let mut router = Router::new();
        crate::workstreams::protocol::register(
            &mut router,
            Arc::new(tokio::sync::Mutex::new(store)),
        );
        let id = call(
            &router,
            "workstream.create",
            json!({"name": "t"}),
            &throwaway_ctx(),
        )
        .await
        .expect("create")["id"]
            .as_str()
            .unwrap()
            .to_string();

        let app = build(sessions.clone(), router);
        (app, id, sessions)
    }

    /// A workstream with no bound sessions must still return `200` and keep
    /// the stream open, per DOC-48 §5.3 — proven by reading a bounded prefix
    /// (a `keep-alive` comment ping) rather than the whole (never-ending)
    /// body.
    #[tokio::test]
    async fn zero_session_workstream_stream_stays_open() {
        let (app, id, _sessions) = router_with_workstream().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/workstreams/{id}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        // Publish a workstream-level event and confirm it reaches this
        // response body — proves the stream is genuinely live, not merely
        // "200 then immediately closed".
        let marker = crate::workstreams::WorkstreamId::new();
        crate::workstreams::events::publish_activation_changed(marker, None);

        let mut stream = resp.into_body().into_data_stream();
        let mut collected = Vec::new();
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !String::from_utf8_lossy(&collected).contains(&marker.to_string()) {
                match stream.next().await {
                    Some(Ok(chunk)) => collected.extend_from_slice(&chunk),
                    _ => break,
                }
            }
        })
        .await;
        assert!(read.is_ok(), "timed out waiting for the marker event");
    }

    /// An unknown workstream id must 404 with the `workstream.get` mapped
    /// error, not a 200-wrapped envelope.
    #[tokio::test]
    async fn events_route_unknown_workstream_returns_404() {
        let (app, _id, _sessions) = router_with_workstream().await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/workstreams/00000000-0000-0000-0000-000000000000/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// A workstream with a bound session must fan out that session's replay,
    /// tagged with its `session_id`, into the merged SSE stream.
    ///
    /// Why: `session.create(workstream_id)` binding (#3298) does not exist
    /// yet — see `crate::workstreams::events` module docs — so this test
    /// seeds `Workstream::session_ids` directly (a `pub` field on the real
    /// domain type) via a hand-written store file, mirroring
    /// `crate::serve::mod::tests::build_router_reconciles_dangling_active_pointer_on_boot`'s
    /// established "seed the raw store file, then load it for real"
    /// pattern. This proves the ROUTE'S fan-out over real `session_ids`
    /// end-to-end; `crate::workstreams::events::events_tests` already proves
    /// `aggregate`/`RegistrySessionEventSource` in isolation.
    #[tokio::test]
    async fn events_route_streams_replay_from_bound_session() {
        let sessions = Arc::new(SessionRegistry::new());
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let mut ws = crate::workstreams::Workstream::new("bound");
        ws.session_ids.push(session.id.clone());
        let ws_id = ws.id;
        let raw = json!({
            "version": "1.0",
            "active_workstream_id": serde_json::Value::Null,
            "workstreams": [ws],
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let store_path = dir.path().join("workstreams-test.json");
        tokio::fs::write(&store_path, serde_json::to_string_pretty(&raw).unwrap())
            .await
            .expect("seed raw store file with a bound session");

        let store = crate::workstreams::WorkstreamStore::load(store_path)
            .await
            .expect("load seeded store");
        let mut router = Router::new();
        crate::workstreams::protocol::register(
            &mut router,
            Arc::new(tokio::sync::Mutex::new(store)),
        );
        let app = build(sessions, router);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/workstreams/{ws_id}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut stream = resp.into_body().into_data_stream();
        let mut collected = Vec::new();
        let want = format!("\"session_id\":\"{}\"", session.id);
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !String::from_utf8_lossy(&collected).contains(&want) {
                match stream.next().await {
                    Some(Ok(chunk)) => collected.extend_from_slice(&chunk),
                    _ => break,
                }
            }
        })
        .await;
        let text = String::from_utf8_lossy(&collected).to_string();
        assert!(
            read.is_ok(),
            "timed out waiting for the bound session's tagged event; body so far: {text}"
        );
        assert!(
            text.contains("\"event_type\":\"session_started\""),
            "{text}"
        );
    }
}
