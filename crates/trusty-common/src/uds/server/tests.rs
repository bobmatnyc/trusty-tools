//! Coverage for the #6277 generic UDS JSON-RPC server.
//!
//! The `dispatch_*` tests drive [`RpcRouter::dispatch`] over raw bytes, so every
//! refusal arm is asserted without a socket. The `serve_*` and `server_*` tests
//! stand up a real listener bound through `bind_hardened` and dial it with
//! `uds::send_framed_request`, which is the client half production callers use —
//! so the framing contract is proven end to end rather than against a bespoke
//! test writer.
//!
//! The foreign-peer refusal is not reproduced here: rejecting a connection from
//! another uid needs a second account, and the decision behind it is already
//! covered as a pure function by `uds::tests`' `peer_uid_verdict_*`. What this
//! file proves is that [`handle_connection`] calls it before reading a byte.

use super::*;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

/// A test request/response pair, so the typed-handler path is exercised with
/// real caller-defined types rather than `serde_json::Value`.
#[derive(Debug, Serialize, Deserialize)]
struct Greet {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Greeting {
    text: String,
}

/// A router with one typed method and one that always fails.
fn greeting_router() -> RpcRouter {
    RpcRouter::new()
        .typed("greet", |req: Greet| async move {
            Ok(Greeting {
                text: format!("hello {}", req.name),
            })
        })
        .typed("explode", |_req: ()| async move {
            Err::<(), _>(RpcError::new(-32001, "handler said no"))
        })
}

/// One request frame as bytes, newline excluded — `dispatch` takes the frame
/// body, and the framing is `encode_frame`'s job.
fn frame(id: u64, method: &str, params: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("serialize test frame")
}

// ── dispatch: the decision, without a socket ────────────────────────────────

#[tokio::test]
async fn dispatch_routes_to_the_registered_handler() {
    let response = greeting_router()
        .dispatch(&frame(7, "greet", json!({ "name": "ada" })))
        .await;

    assert_eq!(response.id, json!(7), "the request id must be echoed back");
    assert!(response.error.is_none(), "unexpected error: {response:?}");
    let greeting: Greeting =
        serde_json::from_value(response.result.expect("a result")).expect("decode result");
    assert_eq!(
        greeting,
        Greeting {
            text: "hello ada".to_string()
        }
    );
}

#[tokio::test]
async fn dispatch_reports_method_not_found_for_an_unregistered_method() {
    // Why: the whole point of a method table. A server that hung up instead
    // would give a drifted client a transport error where a name exists.
    let response = greeting_router()
        .dispatch(&frame(1, "review", json!(null)))
        .await;

    let error = response.error.expect("an error");
    assert_eq!(error.code, CODE_METHOD_NOT_FOUND);
    assert!(
        error.message.contains("review") && error.message.contains("greet"),
        "the refusal must name both the bad method and the served ones: {}",
        error.message
    );
    assert_eq!(response.id, json!(1));
}

#[tokio::test]
async fn dispatch_rejects_an_unparseable_frame() {
    let response = greeting_router().dispatch(b"{not json").await;

    assert_eq!(
        response.error.expect("an error").code,
        CODE_PARSE_ERROR,
        "an unreadable frame is a parse error, not a method-not-found"
    );
    assert_eq!(
        response.id,
        serde_json::Value::Null,
        "there was no readable id to echo"
    );
}

#[tokio::test]
async fn dispatch_rejects_a_wrong_jsonrpc_version() {
    let raw = serde_json::to_vec(&json!({
        "jsonrpc": "1.0",
        "id": 3,
        "method": "greet",
        "params": { "name": "ada" },
    }))
    .expect("serialize");

    let response = greeting_router().dispatch(&raw).await;

    assert_eq!(response.error.expect("an error").code, CODE_INVALID_REQUEST);
}

#[tokio::test]
async fn dispatch_reports_invalid_params_for_an_undecodable_payload() {
    // `greet` needs `{ "name": String }`; a number is the shape an axum `Json`
    // extractor would reject with a 422, and must not reach the handler.
    let response = greeting_router()
        .dispatch(&frame(4, "greet", json!({ "name": 17 })))
        .await;

    let error = response.error.expect("an error");
    assert_eq!(error.code, CODE_INVALID_PARAMS);
    assert!(
        error.message.contains("params do not decode"),
        "the serde reason must survive: {}",
        error.message
    );
}

#[tokio::test]
async fn dispatch_propagates_a_handler_error_verbatim() {
    let response = greeting_router()
        .dispatch(&frame(5, "explode", json!(null)))
        .await;

    assert_eq!(
        response.error.expect("an error"),
        RpcError::new(-32001, "handler said no"),
        "a handler's own code and message must not be rewritten"
    );
}

#[test]
fn method_names_are_sorted_and_complete() {
    let router = greeting_router();
    assert_eq!(
        router.method_names().collect::<Vec<_>>(),
        vec!["explode", "greet"]
    );
}

// ── the socket ──────────────────────────────────────────────────────────────

/// Bind a server on a fresh temp socket and serve it until the returned sender
/// is fired or dropped. Returns the socket path, the shutdown trigger, and the
/// join handle for the serving task.
fn spawn_server(
    dir: &std::path::Path,
    router: RpcRouter,
    options: RpcServeOptions,
) -> (
    std::path::PathBuf,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), RpcServerError>>,
) {
    let socket = dir.join("rpc.sock");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = RpcServer::new(socket.clone(), router).with_options(options);
    let handle = tokio::spawn(async move {
        server
            .run(async move {
                let _ = rx.await;
            })
            .await
    });
    (socket, tx, handle)
}

/// Dial `socket` with the production client half and return the response frame.
async fn call(
    socket: &std::path::Path,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> RpcResponse {
    let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    crate::uds::send_framed_request(socket, &request, Duration::from_secs(10))
        .await
        .expect("round trip")
}

/// Poll until the server has bound, so a test never races the accept loop.
async fn await_socket(socket: &std::path::Path) {
    for _ in 0..200 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server never bound {}", socket.display());
}

#[tokio::test]
async fn serve_round_trips_a_request_over_a_real_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), greeting_router(), RpcServeOptions::default());
    await_socket(&socket).await;

    let response = call(&socket, 42, "greet", json!({ "name": "grace" })).await;

    assert_eq!(response.id, json!(42));
    let greeting: Greeting =
        serde_json::from_value(response.result.expect("a result")).expect("decode");
    assert_eq!(greeting.text, "hello grace");
}

#[tokio::test]
async fn serve_answers_an_unknown_method_rather_than_hanging_up() {
    // Over the wire, not just in `dispatch`: the client must get a frame back,
    // because a dropped connection surfaces as `UdsRpcError::NoResponse` and
    // tells the caller nothing about why.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), greeting_router(), RpcServeOptions::default());
    await_socket(&socket).await;

    let response = call(&socket, 1, "nope", json!(null)).await;

    assert_eq!(
        response.error.expect("an error").code,
        CODE_METHOD_NOT_FOUND
    );
}

#[tokio::test]
async fn serve_handles_concurrent_connections_without_serialising() {
    // A handler that cannot finish until every peer has arrived can only
    // complete if the connections run in parallel. A loop that dispatched
    // inline deadlocks here — and would also read as `NotServing` to a prober
    // under load, which is why the per-connection spawn is a requirement rather
    // than a throughput choice.
    let peers = 4usize;
    let barrier = Arc::new(tokio::sync::Barrier::new(peers));
    let router = RpcRouter::new().typed("wait", move |_req: ()| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            Ok::<bool, RpcError>(true)
        }
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let mut calls = Vec::new();
    for n in 0..peers {
        let socket = socket.clone();
        calls.push(tokio::spawn(async move {
            call(&socket, n as u64, "wait", json!(null)).await
        }));
    }
    for handle in calls {
        let response = handle.await.expect("join");
        assert_eq!(
            response.result,
            Some(json!(true)),
            "a serialised server deadlocks here instead of answering"
        );
    }
}

#[tokio::test]
async fn serve_stops_on_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, stop, handle) =
        spawn_server(tmp.path(), greeting_router(), RpcServeOptions::default());
    await_socket(&socket).await;

    stop.send(()).expect("signal shutdown");

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the server must return once shutdown resolves")
        .expect("join")
        .expect("clean shutdown");
}

#[tokio::test]
async fn server_round_trips_and_removes_its_socket_on_shutdown() {
    // `bind_hardened` binds and chmods; nothing in it or in tokio's `Drop`
    // unlinks the path, so without the explicit `remove_file` the next start
    // fails to bind. This is the regression proof for that cleanup.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, stop, handle) =
        spawn_server(tmp.path(), greeting_router(), RpcServeOptions::default());
    await_socket(&socket).await;

    let response = call(&socket, 9, "greet", json!({ "name": "ada" })).await;
    assert!(response.error.is_none());

    stop.send(()).expect("signal shutdown");
    handle.await.expect("join").expect("clean shutdown");

    assert!(
        !socket.exists(),
        "the socket file must be gone after shutdown, or the next bind fails"
    );
}

// ── one connection, driven directly ─────────────────────────────────────────

#[tokio::test]
async fn serve_rejects_an_oversized_frame() {
    // The budget is the server's half of the contract `send_framed_request_capped`
    // states on the client side. Over budget must be a refusal, not an
    // unbounded read.
    let options = RpcServeOptions {
        max_frame_bytes: 64,
        ..RpcServeOptions::default()
    };
    let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");

    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let huge = frame(1, "greet", json!({ "name": "x".repeat(4096) }));
        let _ = client.write_all(&huge).await;
        let _ = client.flush().await;
        // Hold the write half open: the refusal must come from the budget, not
        // from an EOF that happened to arrive first.
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let outcome = handle_connection(server, Arc::new(greeting_router()), options).await;

    match outcome {
        Err(RpcServerError::FrameTooLarge { limit }) => assert_eq!(limit, 64),
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
    writer.abort();
}

#[tokio::test]
async fn handle_connection_reports_a_liveness_probe_rather_than_a_failure() {
    // `uds::probe::socket_is_serving` connects and closes without writing. If
    // that read as a failed request, the one warning an operator greps for
    // would fire on every health check.
    let (client, server) = tokio::net::UnixStream::pair().expect("socketpair");
    drop(client);

    let served = handle_connection(
        server,
        Arc::new(greeting_router()),
        RpcServeOptions::default(),
    )
    .await
    .expect("a closed probe is not an error");

    assert_eq!(served, Served::LivenessProbe);
}

#[tokio::test]
async fn handle_connection_reports_an_error_response_as_answered() {
    let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let mut bytes = frame(1, "nope", json!(null));
        bytes.push(b'\n');
        let _ = client.write_all(&bytes).await;
        let _ = client.flush().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let served = handle_connection(
        server,
        Arc::new(greeting_router()),
        RpcServeOptions::default(),
    )
    .await
    .expect("a refusal is still an answer");

    assert_eq!(served, Served::Answered { errored: true });
    writer.abort();
}
