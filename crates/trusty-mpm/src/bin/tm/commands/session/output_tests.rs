//! Tests for `tm sessions output` resolution (#9162).
//!
//! Why: `Output` resolved only project sessions, so every managed session read
//! "not found". These tests pin the managed-first resolution and, above all,
//! that no failure branch reads as "not found" (a fail-open check).
//! What: each test serves a stub daemon whose managed list, managed output
//! route and project output route answer canned responses, then drives
//! [`fetch_output`] against it.
//! Test: this file IS the test module for `session::output`.

use std::future::IntoFuture as _;

use axum::http::StatusCode;
use axum::routing::get;

use super::fetch_output;

/// One managed session the stub lists.
const NAMED: (&str, &str) = ("11111111-0000-0000-0000-000000000001", "tm-alpha-01");
/// A second session sharing the `tm-alpha-` prefix with [`NAMED`].
const SIBLING: (&str, &str) = ("11111111-0000-0000-0000-000000000002", "tm-alpha-02");

/// What the stub answers, route by route.
struct Stub {
    /// `GET /api/v1/sessions/managed`: status and body.
    list: (StatusCode, String),
    /// `GET /api/v1/sessions/managed/{id}/output`: status and body.
    managed_output: (StatusCode, String),
    /// `GET /sessions/{id}/output`: status and body.
    project_output: (StatusCode, String),
}

fn list_of(sessions: &[(&str, &str)]) -> (StatusCode, String) {
    let rows: Vec<_> = sessions
        .iter()
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name, "state": "active" }))
        .collect();
    (
        StatusCode::OK,
        serde_json::json!({ "sessions": rows }).to_string(),
    )
}

fn pane(text: &str) -> (StatusCode, String) {
    (
        StatusCode::OK,
        serde_json::json!({
            "output": text, "lines": 50, "original_bytes": 0,
            "compressed_bytes": 0, "compress_level": null, "input_box": null
        })
        .to_string(),
    )
}

fn no_route() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such session".to_string())
}

/// Serve `stub` on an ephemeral loopback port and return a client for it.
async fn client_for(stub: Stub) -> trusty_mpm::client::DaemonClient {
    let Stub {
        list,
        managed_output,
        project_output,
    } = stub;
    let app = axum::Router::new()
        .route(
            "/api/v1/sessions/managed",
            get(move || std::future::ready(list.clone())),
        )
        .route(
            "/api/v1/sessions/managed/{id}/output",
            get(move || std::future::ready(managed_output.clone())),
        )
        .route(
            "/sessions/{id}/output",
            get(move || std::future::ready(project_output.clone())),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    trusty_mpm::client::DaemonClient::with_client(reqwest::Client::new(), format!("http://{addr}"))
}

/// #9162 (criterion 1): a managed session name reads that session's pane
/// through the managed route. Red on main: the project route 404s it.
#[tokio::test]
async fn managed_name_reads_the_managed_pane_9162() {
    let daemon = client_for(Stub {
        list: list_of(&[NAMED]),
        managed_output: pane("managed pane text"),
        project_output: no_route(),
    })
    .await;
    let body = fetch_output(&daemon, NAMED.1, 50, false)
        .await
        .expect("a managed session name must resolve");
    assert_eq!(body["output"], "managed pane text");
}

/// #9162 (criterion 2): a project session name still reads through the
/// project route.
#[tokio::test]
async fn project_name_still_reads_the_project_pane_9162() {
    let daemon = client_for(Stub {
        list: list_of(&[NAMED]),
        managed_output: no_route(),
        project_output: pane("project pane text"),
    })
    .await;
    let body = fetch_output(&daemon, "some-project-session", 50, false)
        .await
        .expect("a project session name must still resolve");
    assert_eq!(body["output"], "project pane text");
}

/// #9162 (criterion 3): an ambiguous prefix fails naming the ambiguity, not
/// "not found". Red on main: the project route answers "not found".
#[tokio::test]
async fn ambiguous_prefix_names_the_ambiguity_9162() {
    let daemon = client_for(Stub {
        list: list_of(&[NAMED, SIBLING]),
        managed_output: pane("must not be read"),
        project_output: no_route(),
    })
    .await;
    let err = fetch_output(&daemon, "tm-alpha-", 50, false)
        .await
        .expect_err("an ambiguous prefix must fail");
    let msg = err.to_string();
    assert!(msg.contains("ambiguous"), "not an ambiguity error: {msg}");
    assert!(msg.contains(NAMED.1) && msg.contains(SIBLING.1), "{msg}");
}

/// #9162 (criterion 4): a managed session whose pane the daemon refuses to
/// read fails with the daemon's reason, never "not found" and never an empty Ok.
#[tokio::test]
async fn managed_session_without_a_pane_fails_with_the_reason_9162() {
    let daemon = client_for(Stub {
        list: list_of(&[NAMED]),
        managed_output: (
            StatusCode::CONFLICT,
            "refusing to act on the recorded pane: this record carries no pane id".to_string(),
        ),
        project_output: no_route(),
    })
    .await;
    let err = fetch_output(&daemon, NAMED.1, 50, false)
        .await
        .expect_err("a managed session with no pane must fail closed");
    let msg = err.to_string();
    assert!(msg.contains("no pane id"), "reason lost: {msg}");
    assert!(!msg.contains("not found"), "read as not found: {msg}");
}

/// #9162 (criterion 5, fail-open): a list failure surfaces as that failure.
/// Red on main: the project route 404s and the error reads "not found".
#[tokio::test]
async fn a_list_error_never_reads_as_not_found_9162() {
    let daemon = client_for(Stub {
        list: (
            StatusCode::INTERNAL_SERVER_ERROR,
            "store unavailable".to_string(),
        ),
        managed_output: pane("must not be read"),
        project_output: no_route(),
    })
    .await;
    let err = fetch_output(&daemon, NAMED.1, 50, false)
        .await
        .expect_err("a list failure must fail the verb");
    let msg = err.to_string();
    assert!(msg.contains("500"), "the daemon error was swallowed: {msg}");
    assert!(!msg.contains("not found"), "read as not found: {msg}");
}

/// An id in neither store is still "not found".
#[tokio::test]
async fn an_unknown_session_is_not_found_9162() {
    let daemon = client_for(Stub {
        list: list_of(&[NAMED]),
        managed_output: no_route(),
        project_output: no_route(),
    })
    .await;
    let err = fetch_output(&daemon, "nope", 50, false)
        .await
        .expect_err("an unknown session must fail");
    assert!(err.to_string().contains("not found"), "{err}");
}
