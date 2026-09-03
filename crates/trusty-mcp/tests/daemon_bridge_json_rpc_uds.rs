//! The shared stdio↔UDS forwarder, against a real `UnixListener` (#6316).
//!
//! Why: #6316's acceptance names one integration test that drives the forwarder
//! against a UDS echo server and asserts streaming refusal plus `jsonrpc`
//! normalisation. The three failure arms are here too, because each of them is
//! a way the bridge could go silent instead of answering — a client that gets no
//! matchable response cannot tell a failure from a hang (#6309), so "it errors"
//! is the behaviour under test, not an incidental detail.
//!
//! What: a stub daemon binds a hardened socket in a temp dir, records the
//! envelopes it receives, and answers each connection from a scripted list. Every
//! listener runs as a task on the test's own runtime, so it is reaped when the
//! test ends — nothing outlives the process.
//!
//! Test: this file. The pure helpers are unit-tested in
//! `src/daemon_bridge_json_rpc.rs`.

#![cfg(all(unix, feature = "daemon-bridge-json-rpc"))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use trusty_mcp::daemon_bridge_json_rpc::{DaemonBridgeJsonRpc, UdsBridgeConfig};
use trusty_mcp::{Request, error_codes};

/// How the stub answers one connection.
enum Reply {
    /// Echo `result` back with the request's own id.
    Echo,
    /// Write these bytes verbatim, then close.
    Bytes(&'static str),
    /// Accept, read the request, then never answer.
    Silence,
}

/// A stub daemon: a hardened socket plus the envelopes it has been sent.
struct Stub {
    socket: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    _dir: tempfile::TempDir,
}

impl Stub {
    /// Bind a socket in a fresh temp dir and serve one connection per reply.
    ///
    /// The listener task holds the only reference to the listener; dropping the
    /// test's runtime cancels it and unlinks nothing the temp dir will not.
    fn spawn(replies: Vec<Reply>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sockets").join("stub.sock");
        let listener: UnixListener =
            trusty_common::uds::bind_hardened(&socket).expect("bind the stub socket");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);

        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                // The client half-closes after writing, so read-to-EOF gets the
                // whole request frame and never blocks.
                let mut frame = Vec::new();
                let _ = conn.read_to_end(&mut frame).await;
                let parsed: Value = serde_json::from_slice(&frame).unwrap_or(Value::Null);
                recorder
                    .lock()
                    .expect("record the envelope")
                    .push(parsed.clone());

                match reply {
                    Reply::Echo => {
                        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                        let method = parsed.get("method").cloned().unwrap_or(Value::Null);
                        let body = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"echoed_method": method},
                        });
                        let mut bytes = serde_json::to_vec(&body).expect("encode the reply");
                        bytes.push(b'\n');
                        let _ = conn.write_all(&bytes).await;
                        let _ = conn.flush().await;
                    }
                    Reply::Bytes(raw) => {
                        let _ = conn.write_all(raw.as_bytes()).await;
                        let _ = conn.flush().await;
                    }
                    Reply::Silence => {
                        // Outlive any test budget; cancelled with the runtime.
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    }
                }
            }
        });

        Self {
            socket,
            seen,
            _dir: dir,
        }
    }

    fn envelopes(&self) -> Vec<Value> {
        self.seen
            .lock()
            .expect("read the recorded envelopes")
            .clone()
    }
}

/// A short-budget config so a failure arm fails fast rather than at 60 s.
fn config(socket: &Path) -> UdsBridgeConfig {
    UdsBridgeConfig::new(socket, "trusty-stub").with_request_timeout(Duration::from_millis(500))
}

fn call(method: &str, id: i64) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(id)),
        method: method.into(),
        params: None,
    }
}

/// Why (#6316 acceptance): the whole point of the module — a request goes in,
/// reaches the daemon over the socket, and the daemon's answer comes back out.
/// What: drives `answer` against an echoing stub and asserts both halves: the
/// client sees the daemon's `result`, and the daemon received an envelope whose
/// `jsonrpc` is `"2.0"` even though the client omitted the field.
/// Test: this test.
#[tokio::test]
async fn a_request_is_forwarded_and_the_reply_comes_back() {
    let stub = Stub::spawn(vec![Reply::Echo]);
    let bridge = DaemonBridgeJsonRpc::new(config(&stub.socket));

    let resp = bridge
        .answer(Request {
            // The #6286 trap: an omitted `jsonrpc` serialises as null.
            jsonrpc: None,
            id: Some(json!(11)),
            method: "memory.recall".into(),
            params: None,
        })
        .await;

    assert!(!resp.suppress, "an id-bearing call must be answered");
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    assert_eq!(resp.id, Some(json!(11)));
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(
        resp.result.expect("the daemon's result reaches the client")["echoed_method"],
        "memory.recall"
    );

    let seen = stub.envelopes();
    assert_eq!(seen.len(), 1, "exactly one frame reached the daemon");
    assert_eq!(
        seen[0]["jsonrpc"], "2.0",
        "the forwarded envelope is normalised: {}",
        seen[0]
    );
    assert_eq!(seen[0]["method"], "memory.recall");
}

/// Why: MCP stdio writes one response per request, so a streamed method must be
/// refused rather than forwarded — a forwarded one leaves the client waiting for
/// a frame sequence that never arrives (#6286).
/// What: configures `memory.chat` as streaming and asserts the refusal is an
/// `INVALID_REQUEST` carrying the request's id, and that the daemon was never
/// dialled (its recorder stays empty).
/// Test: this test.
#[tokio::test]
async fn a_streaming_method_is_refused_before_the_socket_is_dialled() {
    let stub = Stub::spawn(vec![Reply::Echo]);
    let bridge = DaemonBridgeJsonRpc::new(
        config(&stub.socket).with_streaming_methods(["memory.chat", "memory.activity_stream"]),
    );

    let resp = bridge.answer(call("memory.chat", 3)).await;

    let err = resp
        .error
        .expect("a streamed method is refused, not answered");
    assert_eq!(err.code, error_codes::INVALID_REQUEST);
    assert!(
        err.message.contains("memory.chat") && err.message.contains("stream"),
        "the refusal names the method and the reason: {}",
        err.message
    );
    assert_eq!(resp.id, Some(json!(3)), "the refusal is matchable");
    assert!(
        stub.envelopes().is_empty(),
        "the refusal happens before the dial"
    );
}

/// Why: a real MCP client wraps every tool call in `tools/call`, so a refusal
/// list checked only against the outer `method` catches nothing in practice.
/// What: sends `tools/call` with `params.name` naming a streaming method and
/// asserts the same refusal.
/// Test: this test.
#[tokio::test]
async fn a_streaming_tools_call_is_refused_too() {
    let stub = Stub::spawn(vec![Reply::Echo]);
    let bridge =
        DaemonBridgeJsonRpc::new(config(&stub.socket).with_streaming_methods(["memory.chat"]));

    let resp = bridge
        .answer(Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(4)),
            method: "tools/call".into(),
            params: Some(json!({"name": "memory.chat", "arguments": {}})),
        })
        .await;

    let err = resp.error.expect("the wrapped form is refused too");
    assert_eq!(err.code, error_codes::INVALID_REQUEST);
    assert!(stub.envelopes().is_empty());
}

/// Fail-Open Check, arm 1 of 3 — the daemon is not listening.
///
/// Why: nothing at the socket is the ordinary case when a daemon has not been
/// started or has died. Answering nothing, or answering without the id, is
/// indistinguishable from a hang to the client (#6309).
/// What: points the bridge at a path with no listener and asserts an
/// `INTERNAL_ERROR` response that carries the request id and names both the
/// daemon and the socket.
/// Test: this test.
#[tokio::test]
async fn a_dead_socket_answers_with_an_error_naming_the_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("sockets").join("absent.sock");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("create the socket dir");

    let bridge = DaemonBridgeJsonRpc::new(config(&socket));
    let resp = bridge.answer(call("memory.recall", 5)).await;

    assert!(!resp.suppress, "a failure is still an answer");
    assert!(resp.result.is_none(), "a failure is never an empty result");
    let err = resp.error.expect("a dead socket is reported");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert_eq!(resp.id, Some(json!(5)), "the error is matchable");
    assert!(
        err.message.contains("trusty-stub") && err.message.contains("absent.sock"),
        "the error names the daemon and the socket: {}",
        err.message
    );
}

/// Fail-Open Check, arm 2 of 3 — the daemon accepts and never answers.
///
/// Why: this is the arm that would hang forever without a budget, and a hang is
/// the one failure a client cannot recover from.
/// What: a stub that accepts and sleeps, under a 500 ms budget. Asserts the call
/// returns an error naming the timeout, and that it returns well inside the test
/// runner's patience.
/// Test: this test.
#[tokio::test]
async fn a_silent_daemon_answers_with_a_timeout_error() {
    let stub = Stub::spawn(vec![Reply::Silence]);
    let bridge = DaemonBridgeJsonRpc::new(config(&stub.socket));

    let started = std::time::Instant::now();
    let resp = bridge.answer(call("memory.recall", 6)).await;
    let elapsed = started.elapsed();

    let err = resp
        .error
        .expect("a silent daemon is reported, not waited on");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert_eq!(resp.id, Some(json!(6)));
    assert!(
        err.message.contains("did not complete the exchange"),
        "the error names the timeout: {}",
        err.message
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the budget bounded the wait, took {elapsed:?}"
    );
}

/// Fail-Open Check, arm 3 of 3 — the daemon replies with a non-response.
///
/// Why: valid JSON that is not a JSON-RPC response would otherwise pass the
/// decode step and reach `map_reply` with neither `result` nor `error`. Emitting
/// it as an empty result is the silent failure this arm exists to prevent.
/// What: a stub that answers `{"hello":"world"}` and a stub that answers bytes
/// that are not JSON at all. Both produce an error naming the daemon.
/// Test: this test.
#[tokio::test]
async fn a_malformed_daemon_reply_is_reported_rather_than_passed_through() {
    let stub = Stub::spawn(vec![Reply::Bytes("{\"hello\":\"world\"}\n")]);
    let bridge = DaemonBridgeJsonRpc::new(config(&stub.socket));

    let resp = bridge.answer(call("memory.recall", 7)).await;

    assert!(resp.result.is_none(), "a malformed reply is never a result");
    let err = resp.error.expect("a non-response is reported");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert_eq!(resp.id, Some(json!(7)), "the error is matchable");
    assert!(
        err.message.contains("not a JSON-RPC response"),
        "the error names the cause: {}",
        err.message
    );
}

/// Why: the sibling of the arm above — bytes that are not JSON fail one layer
/// earlier, in the framed client's decode step, and must reach the client as an
/// error rather than as a panic or a dropped response.
/// What: a stub answering `not json` and an assertion that the decode failure is
/// reported with the request's id.
/// Test: this test.
#[tokio::test]
async fn a_non_json_daemon_reply_is_reported_rather_than_passed_through() {
    let stub = Stub::spawn(vec![Reply::Bytes("not json\n")]);
    let bridge = DaemonBridgeJsonRpc::new(config(&stub.socket));

    let resp = bridge.answer(call("memory.recall", 8)).await;

    let err = resp.error.expect("garbage is not a response");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert_eq!(resp.id, Some(json!(8)));
    assert!(
        err.message.contains("decode response frame"),
        "the error names the cause: {}",
        err.message
    );
}

/// Why: slice 3 re-points trusty-memory onto this module, and memory injects a
/// `--palace` default plus its own caller identity into every forwarded
/// envelope. If the hook did not reach the wire, that behaviour would be lost in
/// the migration.
/// What: installs a rewriter that adds a field and strips `jsonrpc`, then asserts
/// the daemon saw the added field AND that `jsonrpc` was re-stamped afterwards —
/// a rewriter must not be able to un-normalise the envelope.
/// Test: this test.
#[tokio::test]
async fn a_rewriter_sees_the_envelope_and_its_edit_reaches_the_daemon() {
    let stub = Stub::spawn(vec![Reply::Echo]);
    let bridge =
        DaemonBridgeJsonRpc::new(config(&stub.socket)).with_request_rewriter(|mut envelope| {
            if let Some(obj) = envelope.as_object_mut() {
                obj.insert("palace".into(), json!("myproj"));
                obj.remove("jsonrpc");
            }
            envelope
        });

    let resp = bridge.answer(call("memory.recall", 9)).await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let seen = stub.envelopes();
    assert_eq!(
        seen[0]["palace"], "myproj",
        "the rewrite reached the daemon"
    );
    assert_eq!(
        seen[0]["jsonrpc"], "2.0",
        "a rewriter cannot un-normalise the envelope: {}",
        seen[0]
    );
}

/// Why: MCP §4.1 forbids answering a notification, and a forwarded notification
/// would earn a daemon response the loop then writes to stdout, corrupting the
/// channel.
/// What: asserts an id-less request is suppressed and never reaches the socket.
/// Test: this test.
#[tokio::test]
async fn a_notification_is_neither_forwarded_nor_answered() {
    let stub = Stub::spawn(vec![Reply::Echo]);
    let bridge = DaemonBridgeJsonRpc::new(config(&stub.socket));

    let resp = bridge
        .answer(Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        })
        .await;

    assert!(resp.suppress, "a notification produces no wire write");
    assert!(stub.envelopes().is_empty(), "and never reaches the daemon");
}
