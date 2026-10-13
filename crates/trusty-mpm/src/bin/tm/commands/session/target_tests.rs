//! Tests for managed-session target resolution in `pause`, `events`, `run` and
//! `activity` (#9834).
//!
//! Why: those four verbs resolved only project sessions (or sent the raw
//! argument), so a managed session name read "not found" or hit the wrong
//! route. These tests pin the managed-first resolution, the explicit refusal
//! for verbs a managed session has no route for, and that a managed-list
//! failure never reads as "not found" (a fail-open check).
//! What: each test serves a stub daemon that answers canned responses and
//! records every request it receives, then drives the `session` dispatcher.
//! Test: this file IS the test module for `session::target`.

use std::future::IntoFuture as _;
use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode, Uri};
use axum::routing::{get, post};

use crate::cli::SessionAction;
use crate::commands::session::session;

/// One managed session the stub lists.
const NAMED: (&str, &str) = ("11111111-0000-0000-0000-000000000001", "tm-alpha-01");
/// A second managed session sharing the `tm-alpha-` prefix with [`NAMED`].
const SIBLING: (&str, &str) = ("11111111-0000-0000-0000-000000000002", "tm-alpha-02");
/// One project session the stub lists on `GET /sessions`.
const PROJECT: (&str, &str) = ("22222222-0000-0000-0000-000000000001", "tm-proj-01");

type Reply = (StatusCode, String);
/// Every request the stub served, as `"<METHOD> <path>"`.
type Hits = Arc<Mutex<Vec<String>>>;

/// What the stub answers, route by route.
struct Stub {
    /// `GET /api/v1/sessions/managed`.
    list: Reply,
    /// `GET /api/v1/sessions/managed/{id}/activity`.
    activity: Reply,
    /// `GET /sessions` (the project-session list `events` resolves against).
    project_list: Reply,
    /// `POST /sessions/{id}/pause`, `POST /sessions/{id}/command` and
    /// `GET /sessions/{id}/events/poll`.
    project: Reply,
}

fn ok(body: serde_json::Value) -> Reply {
    (StatusCode::OK, body.to_string())
}

fn managed_list(sessions: &[(&str, &str)]) -> Reply {
    let rows: Vec<_> = sessions
        .iter()
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name, "state": "active" }))
        .collect();
    ok(serde_json::json!({ "sessions": rows }))
}

fn project_list() -> Reply {
    ok(serde_json::json!({ "sessions": [{
        "id": PROJECT.0, "workdir": "/work", "status": "Active", "tmux_name": PROJECT.1
    }] }))
}

fn activity_body() -> Reply {
    ok(serde_json::json!({
        "raw_pane": "", "runtime_active": true, "state": "idle", "summary": "idle",
        "confidence": 1.0, "cache_hit": false, "input_tokens": 0, "output_tokens": 0,
        "latency_ms": 0, "total_input_tokens": 0, "total_output_tokens": 0
    }))
}

/// A project-route body every project verb accepts (pause, command, events).
fn project_ok() -> Reply {
    ok(serde_json::json!({ "summary": "saved", "output": "ran", "events": [] }))
}

fn list_error() -> Reply {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "store unavailable".to_string(),
    )
}

fn no_route() -> Reply {
    (StatusCode::NOT_FOUND, "no such session".to_string())
}

/// A handler that records the request and answers `reply`.
fn recorded(
    hits: &Hits,
    reply: Reply,
) -> impl Fn(Method, Uri) -> std::future::Ready<Reply> + Clone + Send + Sync + 'static {
    let hits = hits.clone();
    move |method: Method, uri: Uri| {
        hits.lock()
            .unwrap()
            .push(format!("{method} {}", uri.path()));
        std::future::ready(reply.clone())
    }
}

/// Serve `stub` on an ephemeral loopback port; return a client and its hits.
async fn client_for(stub: Stub) -> (trusty_mpm::client::DaemonClient, Hits) {
    let hits: Hits = Arc::default();
    let app = axum::Router::new()
        .route("/api/v1/sessions/managed", get(recorded(&hits, stub.list)))
        .route(
            "/api/v1/sessions/managed/{id}/activity",
            get(recorded(&hits, stub.activity)),
        )
        .route("/sessions", get(recorded(&hits, stub.project_list)))
        .route(
            "/sessions/{id}/pause",
            post(recorded(&hits, stub.project.clone())),
        )
        .route(
            "/sessions/{id}/command",
            post(recorded(&hits, stub.project.clone())),
        )
        .route(
            "/sessions/{id}/events/poll",
            get(recorded(&hits, stub.project)),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    let client = trusty_mpm::client::DaemonClient::with_client(
        reqwest::Client::new(),
        format!("http://{addr}"),
    );
    (client, hits)
}

/// Requests that reached any project-session route (`/sessions...`).
fn project_hits(hits: &Hits) -> Vec<String> {
    hits.lock()
        .unwrap()
        .iter()
        .filter(|h| {
            h.split(' ')
                .nth(1)
                .is_some_and(|p| p.starts_with("/sessions"))
        })
        .cloned()
        .collect()
}

/// Requests that reached the managed activity route.
fn activity_hits(hits: &Hits) -> Vec<String> {
    hits.lock()
        .unwrap()
        .iter()
        .filter(|h| h.ends_with("/activity"))
        .cloned()
        .collect()
}

fn pause(target: &str) -> SessionAction {
    SessionAction::Pause {
        id_or_name: target.to_string(),
        note: None,
    }
}

fn run(target: &str) -> SessionAction {
    SessionAction::Run {
        id_or_name: target.to_string(),
        command: "ls".to_string(),
        summarize: false,
    }
}

fn events(target: &str) -> SessionAction {
    SessionAction::Events {
        id_or_name: target.to_string(),
    }
}

fn activity(target: &str) -> SessionAction {
    SessionAction::Activity {
        id: target.to_string(),
    }
}

/// Drive `action` and return its error text, failing the test on success.
async fn error_of(daemon: &trusty_mpm::client::DaemonClient, action: SessionAction) -> String {
    match session(daemon, action).await {
        Ok(()) => panic!("expected an error, the verb succeeded"),
        Err(e) => format!("{e:#}"),
    }
}

fn healthy(list: Reply) -> Stub {
    Stub {
        list,
        activity: activity_body(),
        project_list: project_list(),
        project: project_ok(),
    }
}

/// #9834 (Q1): a managed name reaches the activity route by canonical id.
/// Red on main: the raw name is sent as the id.
#[tokio::test]
async fn activity_resolves_managed_name_to_canonical_id_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED, SIBLING]))).await;
    session(&daemon, activity(NAMED.1))
        .await
        .expect("a managed session name must resolve");
    assert_eq!(
        activity_hits(&hits),
        vec![format!("GET /api/v1/sessions/managed/{}/activity", NAMED.0)]
    );
}

/// #9834 (Q1): a unique managed prefix reaches the activity route by id.
#[tokio::test]
async fn activity_resolves_unique_prefix_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    session(&daemon, activity("tm-alpha"))
        .await
        .expect("a unique managed prefix must resolve");
    assert_eq!(
        activity_hits(&hits),
        vec![format!("GET /api/v1/sessions/managed/{}/activity", NAMED.0)]
    );
}

/// #9834 (Q1): an ambiguous managed prefix is an error naming every candidate,
/// and no activity request is sent.
#[tokio::test]
async fn activity_ambiguous_prefix_names_candidates_and_sends_nothing_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED, SIBLING]))).await;
    let err = error_of(&daemon, activity("tm-alpha-0")).await;
    assert!(err.contains("ambiguous"), "{err}");
    assert!(err.contains(NAMED.1) && err.contains(SIBLING.1), "{err}");
    assert!(
        activity_hits(&hits).is_empty(),
        "{:?}",
        hits.lock().unwrap()
    );
}

/// #9834 (Q2, Q3): `pause` on a managed name is refused, names the
/// alternative, and sends nothing to the project route. Red on main: it POSTs
/// the project pause route.
#[tokio::test]
async fn pause_on_managed_name_is_refused_and_names_alternative_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    let err = error_of(&daemon, pause(NAMED.1)).await;
    assert!(err.contains("is a managed session"), "{err}");
    assert!(err.contains("not supported for managed sessions"), "{err}");
    assert!(err.contains("tm sessions stop"), "{err}");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// #9834 (Q2): `run` on a managed name is refused in favour of `send`.
#[tokio::test]
async fn run_on_managed_name_is_refused_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    let err = error_of(&daemon, run(NAMED.1)).await;
    assert!(err.contains("not supported for managed sessions"), "{err}");
    assert!(err.contains("tm sessions send"), "{err}");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// #9834 (Q2): `events` on a managed id is refused in favour of
/// `activity`/`output`.
#[tokio::test]
async fn events_on_managed_name_is_refused_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    let err = error_of(&daemon, events(NAMED.0)).await;
    assert!(err.contains("not supported for managed sessions"), "{err}");
    assert!(err.contains("tm sessions activity"), "{err}");
    assert!(err.contains("tm sessions output"), "{err}");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// Assert a list-failure error: names the list failure, never "not found".
fn assert_list_error(err: &str, target: &str) {
    assert!(
        err.contains(&format!(
            "could not list managed sessions to resolve '{target}'"
        )),
        "{err}"
    );
    assert!(!err.contains("not found"), "{err}");
}

/// #9834 (Q4): a managed-list failure on `pause` is that failure; the project
/// route is never tried.
#[tokio::test]
async fn pause_list_error_never_reads_as_not_found_9834() {
    let (daemon, hits) = client_for(Stub {
        project: no_route(),
        ..healthy(list_error())
    })
    .await;
    assert_list_error(&error_of(&daemon, pause("tm-x")).await, "tm-x");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// #9834 (Q4): a managed-list failure on `run` is that failure.
#[tokio::test]
async fn run_list_error_never_reads_as_not_found_9834() {
    let (daemon, hits) = client_for(Stub {
        project: no_route(),
        ..healthy(list_error())
    })
    .await;
    assert_list_error(&error_of(&daemon, run("tm-x")).await, "tm-x");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// #9834 (Q4): a managed-list failure on `events` is that failure.
#[tokio::test]
async fn events_list_error_never_reads_as_not_found_9834() {
    let (daemon, hits) = client_for(healthy(list_error())).await;
    assert_list_error(&error_of(&daemon, events("tm-x")).await, "tm-x");
    assert!(project_hits(&hits).is_empty(), "{:?}", project_hits(&hits));
}

/// #9834 (Q4): a managed-list failure on `activity` is that failure, and no
/// activity request is sent.
#[tokio::test]
async fn activity_list_error_never_reads_as_not_found_9834() {
    let (daemon, hits) = client_for(Stub {
        activity: no_route(),
        ..healthy(list_error())
    })
    .await;
    assert_list_error(&error_of(&daemon, activity("tm-x")).await, "tm-x");
    assert!(
        activity_hits(&hits).is_empty(),
        "{:?}",
        activity_hits(&hits)
    );
}

/// #9834 (criterion 5): a project name still reaches the project pause route.
#[tokio::test]
async fn pause_project_name_still_reaches_project_route_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    session(&daemon, pause(PROJECT.1))
        .await
        .expect("a project session must still pause");
    assert_eq!(
        project_hits(&hits),
        vec![format!("POST /sessions/{}/pause", PROJECT.1)]
    );
}

/// #9834 (criterion 5): a project name still reaches the project command route.
#[tokio::test]
async fn run_project_name_still_reaches_project_route_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    session(&daemon, run(PROJECT.1))
        .await
        .expect("a project session must still run a command");
    assert_eq!(
        project_hits(&hits),
        vec![format!("POST /sessions/{}/command", PROJECT.1)]
    );
}

/// #9834 (criterion 5): a project name still resolves to its UUID for events.
#[tokio::test]
async fn events_project_name_still_reaches_project_route_9834() {
    let (daemon, hits) = client_for(healthy(managed_list(&[NAMED]))).await;
    session(&daemon, events(PROJECT.1))
        .await
        .expect("a project session must still list events");
    assert_eq!(
        project_hits(&hits),
        vec![
            "GET /sessions".to_string(),
            format!("GET /sessions/{}/events/poll", PROJECT.0)
        ]
    );
}

/// #9834: an ambiguous managed prefix still tries the project route (an exact
/// project name wins); on its 404 the error names the ambiguity, not "not
/// found". Red on main: "not found".
#[tokio::test]
async fn ambiguous_managed_prefix_with_project_404_is_ambiguity_9834() {
    let (daemon, hits) = client_for(Stub {
        project: no_route(),
        ..healthy(managed_list(&[NAMED, SIBLING]))
    })
    .await;
    let err = error_of(&daemon, pause("tm-alpha-0")).await;
    assert!(err.contains("ambiguous"), "{err}");
    assert!(err.contains(NAMED.1) && err.contains(SIBLING.1), "{err}");
    assert_eq!(project_hits(&hits).len(), 1, "{:?}", project_hits(&hits));
}

/// #9834 (criterion 6): an unknown target with a healthy list still reads
/// "not found" from the project route's 404.
#[tokio::test]
async fn pause_unknown_target_still_reads_not_found_9834() {
    let (daemon, _hits) = client_for(Stub {
        project: no_route(),
        ..healthy(managed_list(&[NAMED]))
    })
    .await;
    let err = error_of(&daemon, pause("tm-nobody")).await;
    assert!(err.contains("session 'tm-nobody' not found"), "{err}");
}
