//! Tests for the HTTP-or-socket transport (#6288 step 1).

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use trusty_common::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};

use super::super::socket_routes::{resolve, status_for_code};
use super::DaemonClient;

/// A socket serving `mpm.sessions.output` (echoing its params) and
/// `mpm.sessions.get` (refusing with the daemon's not-found code).
struct FakeDaemon {
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

async fn fake_daemon() -> FakeDaemon {
    // A space in the path, as in the macOS default `Application Support`.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("with space").join("trusty-mpm.sock");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("socket dir");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let router = RpcRouter::new()
        .typed::<Value, Value, _, _>(
            "mpm.sessions.output",
            |params: Value| async move { Ok(params) },
        )
        .typed::<Value, Value, _, _>("mpm.sessions.get", |_: Value| async move {
            Err(RpcError::new(-32004, "session x not found"))
        });
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });
    FakeDaemon {
        socket,
        _dir: dir,
        stop: Some(stop),
    }
}

/// Path captures, literal-before-capture ordering, and an unserved route.
#[test]
fn resolve_maps_routes_onto_methods() {
    let (method, captures) = resolve(&Method::GET, "/sessions/a%20b/output").expect("mapped");
    assert_eq!(method, "mpm.sessions.output");
    assert_eq!(captures.get("id"), Some(&json!("a b")));

    let (method, captures) = resolve(&Method::DELETE, "/sessions/dead").expect("mapped");
    assert_eq!(method, "mpm.sessions.reap", "the literal route wins");
    assert!(captures.is_empty());

    let (method, captures) =
        resolve(&Method::GET, "/api/v1/projects/p1/deliverables/d2").expect("mapped");
    assert_eq!(method, "mpm.deliverables.get");
    assert_eq!(captures.get("project"), Some(&json!("p1")));
    assert_eq!(captures.get("id"), Some(&json!("d2")));

    // #6288 step 2a: `/events` is an SSE stream; the socket serves no unary
    // method for it.
    assert!(resolve(&Method::GET, "/events").is_none());
    let (method, captures) =
        resolve(&Method::POST, "/api/v1/delegations/by-id/d1/repair").expect("mapped");
    assert_eq!(method, "mpm.delegation.repair_by_id");
    assert_eq!(captures.get("delegation_id"), Some(&json!("d1")));
    let (method, captures) = resolve(&Method::POST, "/rpc").expect("mapped");
    assert_eq!(method, "mpm.mcp.dispatch");
    assert!(captures.is_empty());
    assert!(resolve(&Method::POST, "/health").is_none(), "verb matters");
}

/// The client's inverse agrees with the daemon's own status → code table.
#[cfg(feature = "daemon")]
#[test]
fn status_for_code_inverts_the_daemon_table() {
    use crate::daemon::error::{
        CODE_GONE, CODE_PANE_GONE, CODE_WORKSPACE_GONE, rpc_code_for_status,
    };
    for status in [400u16, 403, 404, 409, 422, 500, 502, 503] {
        assert_eq!(
            status_for_code(rpc_code_for_status(status)).as_u16(),
            status,
            "status {status}"
        );
    }
    assert_eq!(status_for_code(CODE_GONE), StatusCode::GONE);
    assert_eq!(
        status_for_code(CODE_WORKSPACE_GONE),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        status_for_code(CODE_PANE_GONE),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

/// Path, query and body merge into one params object; a refusal keeps its
/// status and message.
#[tokio::test]
async fn socket_call_round_trips_a_mapped_route() {
    let daemon = fake_daemon().await;
    let client = DaemonClient::over_socket(&daemon.socket);

    // The shape `tm sessions output --summarize` sends: two `.query` calls,
    // one typed, plus a typed bool and a JSON body field.
    let resp = client
        .get("/sessions/a%20b/output")
        .query(&[("lines", 5u32)])
        .query(&[("compress", "summarise")])
        .query(&[("force", true)])
        .json(&json!({ "note": "n" }))
        .send()
        .await
        .expect("the socket answers");
    assert_eq!(resp.status(), StatusCode::OK);
    let echoed: Value = resp.json().await.expect("json");
    assert_eq!(
        echoed,
        json!({ "id": "a b", "lines": 5, "compress": "summarise", "force": true, "note": "n" })
    );

    let refused = client.get("/sessions/x").send().await.expect("answered");
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    let err = refused.error_for_status().expect_err("404 is an error");
    assert!(err.to_string().contains("session x not found"), "{err}");
}

/// Fail-open check (#6288): an absent socket is `Unreachable`, names the path,
/// and is never turned into a healthy answer.
#[tokio::test]
async fn socket_call_to_an_absent_socket_is_unreachable_and_names_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let client = DaemonClient::over_socket(&socket);

    let err = client
        .health_snapshot_within(Duration::from_secs(2))
        .await
        .expect_err("nothing is listening");
    assert!(
        err.is_connect(),
        "an absent socket is a dial failure: {err:?}"
    );
    assert!(
        err.to_string().contains(&socket.display().to_string()),
        "the error names the socket: {err}"
    );
    assert!(client.health_snapshot().await.is_err());
    assert!(!client.is_healthy().await);
}

/// A route the socket does not serve is refused, never sent over HTTP.
#[tokio::test]
async fn socket_call_refuses_an_unmapped_route() {
    let daemon = fake_daemon().await;
    let client = DaemonClient::over_socket(&daemon.socket);
    let err = client.get("/events").send().await.expect_err("unmapped");
    assert!(!err.is_connect());
    assert!(
        err.to_string().contains("no method on the daemon socket"),
        "{err}"
    );
}

/// #6288 critic LOW: a query entry that is not a (string key, value) pair is
/// an error, never a field dropped on the way to the daemon.
#[tokio::test]
async fn a_query_pair_with_a_non_string_key_is_refused() {
    let daemon = fake_daemon().await;
    let client = DaemonClient::over_socket(&daemon.socket);
    let err = client
        .get("/sessions/a/output")
        .query(&[(5u32, 1u32)])
        .send()
        .await
        .expect_err("a numeric key");
    assert!(
        err.to_string()
            .contains("is not a (string key, value) pair"),
        "{err}"
    );
}
