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

// ── the fallback seam (#6286) ───────────────────────────────────────────────

/// A fallback that answers every name, echoing back what it was handed.
///
/// `refuse` names the one method it fails on, so the success and error arms are
/// driven by the same implementation rather than two near-identical stubs.
struct EchoFallback {
    refuse: &'static str,
}

#[async_trait::async_trait]
impl RpcFallback for EchoFallback {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        if method == self.refuse {
            return Err(RpcError::new(-32002, format!("fallback refused {method}")));
        }
        Ok(json!({ "method": method, "params": params }))
    }
}

#[tokio::test]
async fn dispatch_routes_an_unregistered_method_to_the_fallback() {
    // The seam #6286 exists for: a name the router never registered reaches the
    // service's own dispatcher, with the METHOD NAME intact — a fallback that
    // saw only the params could not decide what to run.
    let router = greeting_router().fallback(EchoFallback { refuse: "" });

    let response = router
        .dispatch(&frame(11, "review", json!({ "n": 1 })))
        .await;

    assert!(response.error.is_none(), "unexpected error: {response:?}");
    assert_eq!(
        response.result,
        Some(json!({ "method": "review", "params": { "n": 1 } })),
        "the fallback must receive both the method name and the params"
    );
    assert_eq!(response.id, json!(11), "the request id is still echoed");
}

#[tokio::test]
async fn dispatch_prefers_a_registered_method_over_the_fallback() {
    // Registered wins. Mounting a dispatcher must not silently take over a
    // method the service deliberately registered separately — that would make
    // the override direction depend on registration order.
    let router = greeting_router().fallback(EchoFallback { refuse: "" });

    let response = router
        .dispatch(&frame(12, "greet", json!({ "name": "ada" })))
        .await;

    assert_eq!(
        response.result,
        Some(json!({ "text": "hello ada" })),
        "the registered handler answered, not the fallback"
    );
}

#[tokio::test]
async fn dispatch_maps_a_fallback_error_to_an_rpc_error_response() {
    // Fail-open check: a fallback that returns `Err` must produce a coded error
    // FRAME. Dropping the connection instead would hand the caller a transport
    // failure with no reason in it, and rewriting the code would lose the
    // service's own refusal.
    let router = greeting_router().fallback(EchoFallback { refuse: "review" });

    let response = router.dispatch(&frame(13, "review", json!(null))).await;

    assert_eq!(
        response.error.expect("an error"),
        RpcError::new(-32002, "fallback refused review"),
        "the fallback's own code and message must survive verbatim"
    );
    assert!(response.result.is_none());
    assert_eq!(response.id, json!(13));
}

// A router with no fallback still answers `method_not_found` — proven by
// `dispatch_reports_method_not_found_for_an_unregistered_method` above, which
// #6286 left untouched. It is not restated here.

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

/// Poll until the server is answering, so a test never races the accept loop.
///
/// `socket.exists()` is not the readiness question: `bind_hardened` creates the
/// file before `serve_until` reaches its first `accept`, so an existence check
/// can hand the test a socket nothing is serving yet.
async fn await_socket(socket: &std::path::Path) {
    for _ in 0..200 {
        if crate::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server never began serving {}", socket.display());
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
async fn serve_survives_a_panicking_handler_and_answers_the_next_connection() {
    // #6277 review: a panicking handler used to be swallowed whole — the
    // client saw a dropped connection and the server logged nothing. The log
    // line itself needs a global tracing subscriber to assert, which is not
    // worth the cross-test interference; what is worth asserting is that one
    // panicking connection neither answers nor takes the accept loop with it.
    let router = RpcRouter::new()
        .typed("boom", |_req: ()| async move {
            panic!("a handler exploded");
            #[allow(unreachable_code)]
            Ok::<(), RpcError>(())
        })
        .typed("greet", |req: Greet| async move {
            Ok(Greeting {
                text: format!("hello {}", req.name),
            })
        });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "boom", "params": null });
    let panicked: Result<RpcResponse, _> =
        crate::uds::send_framed_request(&socket, &request, Duration::from_secs(10)).await;
    assert!(
        panicked.is_err(),
        "a panicking handler answers nothing; the client must see a transport \
         failure rather than hang: {panicked:?}"
    );

    // The property that matters: the server is still serving.
    let response = call(&socket, 2, "greet", json!({ "name": "ada" })).await;
    assert_eq!(
        response.result,
        Some(json!({ "text": "hello ada" })),
        "one panicking connection must not stop the accept loop"
    );
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

// ── streaming (#6286) ───────────────────────────────────────────────────────

/// A request that opts into a stream. The flag is the whole negotiation: a
/// frame without it is the protocol exactly as it stood.
fn stream_frame(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params, "stream": true })
}

/// A router that streams `count` tokens on `"tokens"`, plus the unary methods,
/// so the two tables are exercised against one another.
///
/// `count` of zero streams nothing and still ends on a terminal frame, which is
/// the case a "the end is implied by silence" design would get wrong.
fn token_router(count: usize) -> RpcRouter {
    greeting_router().typed_stream("tokens", move |_req: ()| async move {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        // Produced from a task, exactly as an LLM client feeds its channel:
        // the handler returns the receiver before a single token exists.
        tokio::spawn(async move {
            for n in 0..count {
                if tx.send(Ok(json!(format!("t{n}")))).await.is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    })
}

#[test]
fn stream_frames_carry_the_phase_discriminant() {
    // A plain response has no `stream` field at all; that absence is what lets a
    // reader tell the two envelopes apart on one socket.
    let item = serde_json::to_value(RpcStreamFrame::item(json!(1), json!("tok"))).expect("encode");
    assert_eq!(item["stream"], json!("item"));
    assert_eq!(item["result"], json!("tok"));

    let end = serde_json::to_value(RpcStreamFrame::end(json!(1))).expect("encode");
    assert_eq!(end["stream"], json!("end"));
    assert!(end.get("result").is_none() && end.get("error").is_none());

    let failed =
        serde_json::to_value(RpcStreamFrame::error(json!(1), RpcError::internal("no"))).expect("e");
    assert_eq!(failed["stream"], json!("error"));
    assert_eq!(failed["error"]["message"], json!("no"));

    let unary = serde_json::to_value(RpcResponse::success(json!(1), json!("x"))).expect("encode");
    assert!(
        unary.get("stream").is_none(),
        "a unary response must never carry the discriminant"
    );
}

#[test]
fn stream_names_are_sorted_and_separate_from_unary_names() {
    let router = token_router(1);
    assert_eq!(router.stream_names().collect::<Vec<_>>(), vec!["tokens"]);
    assert_eq!(
        router.method_names().collect::<Vec<_>>(),
        vec!["explode", "greet"],
        "a streaming name must not appear in the unary table"
    );
}

#[tokio::test]
async fn dispatch_streaming_answers_a_unary_request_unchanged() {
    // Backward compatibility, at the decision layer: the wider entry point must
    // produce byte-identical answers for every request that predates #6286.
    for (id, method, params) in [
        (1u64, "greet", json!({ "name": "ada" })),
        (2, "explode", json!(null)),
        (3, "nope", json!(null)),
    ] {
        let raw = frame(id, method, params.clone());
        let unary = greeting_router().dispatch(&raw).await;
        let wide = match greeting_router().dispatch_streaming(&raw).await {
            RpcOutcome::Single(response) => response,
            other => panic!("a request without the flag must not stream: {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(&unary).expect("encode"),
            serde_json::to_value(&wide).expect("encode"),
            "dispatch_streaming changed the answer for {method}"
        );
    }
}

#[tokio::test]
async fn stream_opt_in_is_read_from_the_request_frame() {
    // The negotiation itself: the same method, the same params, and only the
    // flag decides which shape comes back.
    let router = token_router(1);

    let without = frame(1, "tokens", json!(null));
    match router.dispatch_streaming(&without).await {
        RpcOutcome::Single(response) => {
            assert_eq!(response.error.expect("an error").code, CODE_STREAM_REQUIRED)
        }
        other => panic!("no flag must not produce a stream: {other:?}"),
    }

    let with = serde_json::to_vec(&stream_frame(1, "tokens", json!(null))).expect("encode");
    match router.dispatch_streaming(&with).await {
        RpcOutcome::Stream { id, .. } => assert_eq!(id, json!(1)),
        other => panic!("the flag must produce a stream: {other:?}"),
    }
}

/// Read every frame of a streaming call, returning the items and the terminal
/// frame. Uses the production client half, so the contract is proven end to end.
async fn collect_stream(
    socket: &std::path::Path,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<Vec<String>, crate::uds::UdsRpcError> {
    let request = stream_frame(id, method, params);
    let mut stream: crate::uds::FramedStream<String> =
        crate::uds::send_framed_stream_request(socket, &request, Duration::from_secs(10)).await?;
    let mut items = Vec::new();
    while let Some(item) = stream.next_frame().await {
        items.push(item?);
    }
    Ok(items)
}

#[tokio::test]
async fn stream_round_trips_many_frames_over_a_real_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), token_router(3), RpcServeOptions::default());
    await_socket(&socket).await;

    let items = collect_stream(&socket, 1, "tokens", json!(null))
        .await
        .expect("the stream must complete");

    assert_eq!(items, vec!["t0", "t1", "t2"]);
}

#[tokio::test]
async fn stream_of_zero_items_still_ends_on_a_terminal_frame() {
    // "No items" and "truncated" must not look the same on the wire, or an empty
    // answer and a lost one are indistinguishable.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), token_router(0), RpcServeOptions::default());
    await_socket(&socket).await;

    let items = collect_stream(&socket, 1, "tokens", json!(null))
        .await
        .expect("an empty stream is still a complete one");

    assert!(items.is_empty());
}

#[tokio::test]
async fn stream_reports_a_handler_error_as_a_terminal_frame() {
    // The Fail-Open branch: a producer that fails after two items must reach the
    // client as a REASON, never as a stream that quietly stopped at two.
    let router = RpcRouter::new().typed_stream("tokens", |_req: ()| async move {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(json!("t0"))).await;
            let _ = tx.send(Ok(json!("t1"))).await;
            let _ = tx
                .send(Err(RpcError::new(-32003, "the model gave up")))
                .await;
        });
        Ok(rx)
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let err = collect_stream(&socket, 1, "tokens", json!(null))
        .await
        .expect_err("a mid-stream failure must not read as a complete answer");

    match err {
        crate::uds::UdsRpcError::Stream { error, .. } => {
            assert_eq!(error, RpcError::new(-32003, "the model gave up"),);
        }
        other => panic!("expected Stream, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_reports_an_open_failure_as_a_terminal_frame() {
    // A handler that refuses before producing anything answers in the same shape
    // as one that fails half way — the caller has one thing to read either way.
    let router = RpcRouter::new().typed_stream("tokens", |_req: ()| async move {
        Err(RpcError::new(-32004, "no model configured"))
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let err = collect_stream(&socket, 1, "tokens", json!(null))
        .await
        .expect_err("an open failure is still an error");

    assert!(
        matches!(&err, crate::uds::UdsRpcError::Stream { error, .. } if error.code == -32004),
        "expected the handler's own code, got {err:?}"
    );
}

#[tokio::test]
async fn stream_reports_invalid_params_before_opening_the_stream() {
    let router = RpcRouter::new().typed_stream("tokens", |_req: Greet| async move {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let err = collect_stream(&socket, 1, "tokens", json!({ "name": 17 }))
        .await
        .expect_err("bad params must not open a stream");

    assert!(
        matches!(&err, crate::uds::UdsRpcError::Stream { error, .. }
            if error.code == CODE_INVALID_PARAMS),
        "expected invalid_params, got {err:?}"
    );
}

#[tokio::test]
async fn stream_request_for_a_non_streaming_method_is_refused() {
    // The streaming client API against a unary method. It must fail with a
    // reason and finish — never hang waiting for a terminal frame that a unary
    // answer does not contain.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), token_router(1), RpcServeOptions::default());
    await_socket(&socket).await;

    for method in ["greet", "no-such-method"] {
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            collect_stream(&socket, 1, method, json!({ "name": "ada" })),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} hung instead of failing"))
        .expect_err("a method that does not stream must refuse");

        match err {
            crate::uds::UdsRpcError::Stream { error, .. } => {
                assert_eq!(error.code, CODE_STREAM_UNSUPPORTED);
                assert!(
                    error.message.contains("tokens"),
                    "the refusal must name what this listener does stream: {}",
                    error.message
                );
            }
            other => panic!("expected a terminal error frame for {method}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn unary_request_for_a_streaming_method_is_refused_in_one_frame() {
    // The other direction: an old client, which writes one frame and reads one
    // frame, must get exactly that — not a stream it cannot parse, and not a
    // hang.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) =
        spawn_server(tmp.path(), token_router(3), RpcServeOptions::default());
    await_socket(&socket).await;

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        call(&socket, 1, "tokens", json!(null)),
    )
    .await
    .expect("a unary call on a streaming method must not hang");

    let error = response.error.expect("an error");
    assert_eq!(error.code, CODE_STREAM_REQUIRED);
    assert!(
        error.message.contains("stream"),
        "the refusal must say how to ask again: {}",
        error.message
    );
}

#[tokio::test]
async fn stream_refuses_an_item_larger_than_the_frame_budget() {
    // Per frame, not per stream: an item the client could not buffer becomes a
    // terminal error rather than a frame that desynchronises the connection.
    let router = RpcRouter::new().typed_stream("tokens", |_req: ()| async move {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(async move {
            let _ = tx.send(Ok(json!("small"))).await;
            let _ = tx.send(Ok(json!("x".repeat(4096)))).await;
        });
        Ok(rx)
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(
        tmp.path(),
        router,
        RpcServeOptions {
            max_frame_bytes: 512,
            ..RpcServeOptions::default()
        },
    );
    await_socket(&socket).await;

    let request = stream_frame(1, "tokens", json!(null));
    let mut stream: crate::uds::FramedStream<String> =
        crate::uds::send_framed_stream_request_capped(
            &socket,
            &request,
            Duration::from_secs(10),
            512,
        )
        .await
        .expect("open");

    assert_eq!(
        stream.next_frame().await.expect("an item").expect("ok"),
        "small",
        "the frames before the oversized one still arrive"
    );
    let err = stream
        .next_frame()
        .await
        .expect("a report")
        .expect_err("an oversized item must not be written");
    assert!(
        matches!(&err, crate::uds::UdsRpcError::Stream { error, .. }
            if error.message.contains("frame budget")),
        "expected a terminal budget refusal, got {err:?}"
    );
}

#[tokio::test]
async fn stream_serves_one_frame_requests_on_other_connections_while_running() {
    // A stream holds one connection for as long as its producer runs. The accept
    // loop must keep answering everything else meanwhile — the property a server
    // that drained streams inline would lose.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let router = greeting_router().typed_stream("tokens", move |_req: ()| {
        let release = Arc::clone(&release);
        async move {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(json!("first"))).await;
                // Hold the stream open until the unary calls have been served.
                if let Some(gate) = release.lock().await.take() {
                    let _ = gate.await;
                }
                let _ = tx.send(Ok(json!("last"))).await;
            });
            Ok(rx)
        }
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    let request = stream_frame(1, "tokens", json!(null));
    let mut stream: crate::uds::FramedStream<String> =
        crate::uds::send_framed_stream_request(&socket, &request, Duration::from_secs(10))
            .await
            .expect("open");
    assert_eq!(
        stream.next_frame().await.expect("an item").expect("ok"),
        "first",
        "the stream is live before the interleaved calls"
    );

    for n in 0..3u64 {
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            call(&socket, 100 + n, "greet", json!({ "name": "ada" })),
        )
        .await
        .expect("a one-frame call must not queue behind a live stream");
        assert_eq!(response.result, Some(json!({ "text": "hello ada" })));
    }

    release_tx.send(()).expect("release the stream");
    assert_eq!(
        stream.next_frame().await.expect("an item").expect("ok"),
        "last"
    );
    assert!(
        stream.next_frame().await.is_none(),
        "the terminal frame ends the stream"
    );
}

#[tokio::test]
async fn stream_survives_a_client_that_disconnects_mid_stream() {
    // Dropping a reader mid-stream must not wedge the accept loop or take the
    // process down. The producer stops on its own when its receiver is dropped;
    // what is asserted here is that the server still serves afterwards.
    let router = greeting_router().typed_stream("tokens", |_req: ()| async move {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            // Far more items than the client will read, so the write fails
            // part-way rather than at a convenient boundary.
            for n in 0..10_000u32 {
                if tx.send(Ok(json!(format!("t{n}")))).await.is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _stop, _handle) = spawn_server(tmp.path(), router, RpcServeOptions::default());
    await_socket(&socket).await;

    {
        let request = stream_frame(1, "tokens", json!(null));
        let mut stream: crate::uds::FramedStream<String> =
            crate::uds::send_framed_stream_request(&socket, &request, Duration::from_secs(10))
                .await
                .expect("open");
        assert_eq!(
            stream.next_frame().await.expect("an item").expect("ok"),
            "t0"
        );
        // Drop the reader with thousands of frames still to come.
    }

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        call(&socket, 2, "greet", json!({ "name": "ada" })),
    )
    .await
    .expect("an abandoned stream must not wedge the accept loop");
    assert_eq!(response.result, Some(json!({ "text": "hello ada" })));
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

/// Serve one frame over a socket pair under `max_frame_bytes`, holding the
/// write half open so the outcome comes from the budget rather than an EOF.
async fn serve_one_frame(body: Vec<u8>, max_frame_bytes: u64) -> Result<Served, RpcServerError> {
    let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let _ = client.write_all(&body).await;
        let _ = client.flush().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let options = RpcServeOptions {
        max_frame_bytes,
        ..RpcServeOptions::default()
    };
    let outcome = handle_connection(server, Arc::new(greeting_router()), options).await;
    writer.abort();
    outcome
}

#[tokio::test]
async fn frame_of_exactly_the_budget_including_its_newline_is_accepted() {
    // The documented boundary, asserted from both sides: `max_frame_bytes`
    // counts the terminator, so a JSON body of `max_frame_bytes - 1` is the
    // largest one that fits. `uds::rpc`'s reader draws the line at the same
    // byte, and a test here is what stops the two ends drifting apart.
    let empty = frame(1, "greet", json!({ "name": "" })).len();
    let budget = (empty + 40) as u64;
    let padding = "x".repeat(budget as usize - 1 - empty);
    let mut body = frame(1, "greet", json!({ "name": padding }));
    assert_eq!(
        body.len() as u64,
        budget - 1,
        "the JSON body must be one byte short of the budget"
    );
    body.push(b'\n');

    assert_eq!(
        serve_one_frame(body.clone(), budget)
            .await
            .expect("a frame that exactly fills the budget is accepted"),
        Served::Answered { errored: false }
    );

    match serve_one_frame(body, budget - 1).await {
        Err(RpcServerError::FrameTooLarge { limit }) => assert_eq!(limit, budget - 1),
        other => panic!("one byte over the budget must be refused, got {other:?}"),
    }
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
