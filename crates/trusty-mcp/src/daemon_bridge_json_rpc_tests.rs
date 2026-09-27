//! Unit tests for [`super::daemon_bridge_json_rpc`].
//!
//! Why (#8351): the production module gained the per-request socket
//! resolver and the local-answer hook, and its inline tests would have
//! pushed it past the 500-SLOC production cap. Split out under the
//! `_tests.rs` basename, which the line-cap gate measures against the test
//! cap instead.
//! What: the same module body, included by `#[path]` from the production
//! file, so it keeps private-item access.
//! Test: this file.

use super::*;
use serde_json::json;

fn config() -> UdsBridgeConfig {
    UdsBridgeConfig::new("/tmp/absent.sock", "trusty-test")
}

/// Why: the defaults are part of the contract a consumer relies on when it
/// names neither a timeout nor a budget.
/// What: asserts the constructed defaults are the documented ones.
/// Test: this test.
#[test]
fn config_defaults_are_the_documented_ones() {
    let c = config();
    assert_eq!(c.request_timeout, DEFAULT_REQUEST_TIMEOUT);
    assert_eq!(c.max_frame_bytes, trusty_common::uds::MAX_FRAME_BYTES);
    assert!(c.streaming_methods.is_empty());
}

/// Why (#8267): the bridge's first dial may legitimately precede its
/// daemon's own bind, and its hundredth may not. If the selection ever
/// collapsed to one policy, a cold start would fail as fast as a dead
/// daemon — or every later request would pay a multi-second floor.
/// What: the first call returns the startup bound, every later call the
/// per-request bound. Asserted on the selection rather than through
/// `forward`, which would spend a real 2.7-second floor on a dead socket.
/// Test: this test.
#[test]
fn the_first_dial_spends_the_startup_bound_and_later_dials_do_not() {
    let bridge = DaemonBridgeJsonRpc::new(config());

    assert_eq!(bridge.next_retry_policy(), ConnectRetry::startup());
    assert_eq!(bridge.next_retry_policy(), ConnectRetry::per_request());
    assert_eq!(bridge.next_retry_policy(), ConnectRetry::per_request());

    // The two bounds must actually differ, or the assertions above pass
    // for the wrong reason.
    assert!(
        ConnectRetry::startup().backoff_floor() > ConnectRetry::per_request().backoff_floor(),
        "the startup bound must be the longer one"
    );
}

/// Why: the #6286 trap — an omitted `jsonrpc` serialises as `null` and the
/// daemon's router refuses it.
/// What: an envelope with no `jsonrpc` gains `"2.0"`.
/// Test: this test.
#[test]
fn an_absent_jsonrpc_is_normalised() {
    let out = normalise_jsonrpc(json!({"id": 1, "method": "ping"}));
    assert_eq!(out["jsonrpc"], "2.0");
}

/// Why: a client that sends a version this transport does not speak should
/// still be forwarded, not failed on a field it did not choose.
/// What: `jsonrpc: "1.0"` and `jsonrpc: null` both become `"2.0"`.
/// Test: this test.
#[test]
fn a_wrong_jsonrpc_is_normalised() {
    assert_eq!(
        normalise_jsonrpc(json!({"jsonrpc": "1.0", "method": "ping"}))["jsonrpc"],
        "2.0"
    );
    assert_eq!(
        normalise_jsonrpc(json!({"jsonrpc": null, "method": "ping"}))["jsonrpc"],
        "2.0"
    );
}

/// Why: the wrapped form is the one a real MCP client sends, and missing it
/// is what makes a streamed call hang instead of erroring.
/// What: both the bare method and the `tools/call` envelope resolve to the
/// same name; a `tools/call` with no `params.name` falls back to the outer.
/// Test: this test.
#[test]
fn effective_method_sees_through_the_tools_call_envelope() {
    assert_eq!(
        effective_method(&json!({"method": "memory.chat"})),
        Some("memory.chat")
    );
    assert_eq!(
        effective_method(&json!({
            "method": "tools/call",
            "params": {"name": "memory.chat"}
        })),
        Some("memory.chat")
    );
    assert_eq!(
        effective_method(&json!({"method": "tools/call"})),
        Some("tools/call")
    );
    assert_eq!(effective_method(&json!({"id": 1})), None);
}

/// Why: MCP §4.1 — replying to a notification corrupts the stdio channel.
/// What: an id-less request and a `notifications/*` request are both
/// notifications; an ordinary call is not.
/// Test: this test.
#[test]
fn notifications_are_recognised_before_the_daemon_is_touched() {
    let bare = Request {
        jsonrpc: Some("2.0".into()),
        id: None,
        method: "ping".into(),
        params: None,
    };
    assert!(is_notification(&bare));

    let named = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "notifications/initialized".into(),
        params: None,
    };
    assert!(is_notification(&named));

    let call = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: "ping".into(),
        params: None,
    };
    assert!(!is_notification(&call));
}

/// Why: a notification must produce no wire write at all.
/// What: `answer` returns the suppressed sentinel without dialling — the
/// configured socket does not exist, so a dial would surface as an error.
/// Test: this test.
#[tokio::test]
async fn a_notification_is_suppressed_without_dialling() {
    let bridge = DaemonBridgeJsonRpc::new(config());
    let resp = bridge
        .answer(Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "ping".into(),
            params: None,
        })
        .await;
    assert!(resp.suppress);
}

/// Why: the daemon's error is the answer; wrapping it would hide the code
/// and message the client needs.
/// What: `map_reply` passes a daemon error through with its own code.
/// Test: this test.
#[test]
fn a_daemon_error_reaches_the_client_unaltered() {
    let bridge = DaemonBridgeJsonRpc::new(config());
    let resp = bridge.map_reply(
        Path::new("/tmp/absent.sock"),
        Some(json!(7)),
        json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "no such tool"}}),
    );
    let err = resp.error.expect("the daemon's error survives the hop");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    assert_eq!(err.message, "no such tool");
    assert_eq!(resp.id, Some(json!(7)));
    assert_eq!(resp.jsonrpc, "2.0");
}

/// Why: a daemon that dropped the id would otherwise produce an unmatchable
/// answer, which a client cannot tell from a hang (#6309).
/// What: a reply with a null id falls back to the request's own id.
/// Test: this test.
#[test]
fn a_null_reply_id_falls_back_to_the_requests_own() {
    let bridge = DaemonBridgeJsonRpc::new(config());
    let resp = bridge.map_reply(
        Path::new("/tmp/absent.sock"),
        Some(json!(42)),
        json!({"id": null, "result": {"ok": true}}),
    );
    assert_eq!(resp.id, Some(json!(42)));
}

// ── #8351: recoverable inside one client session ────────────────────────────

/// A request with an id, so every answer below is matchable.
fn call(method: &str, id: i64) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(id)),
        method: method.into(),
        params: None,
    }
}

/// Why (#8351): the whole point. An MCP client that cannot complete
/// `initialize` marks the server failed for the session and never re-spawns
/// it, so the handshake must be answerable with nothing listening.
/// What: a bridge whose socket does not exist answers both handshake methods
/// from the local handler, with a result and no error.
/// Test: this test.
#[tokio::test]
async fn a_local_handler_answers_without_a_daemon() {
    let bridge =
        DaemonBridgeJsonRpc::new(config()).with_local_handler(|req| match req.method.as_str() {
            "initialize" => Some(json!({"protocolVersion": "2024-11-05"})),
            "tools/list" => Some(json!({"tools": []})),
            _ => None,
        });

    for (method, id) in [("initialize", 1), ("tools/list", 2)] {
        let resp = bridge.answer(call(method, id)).await;
        assert!(
            resp.error.is_none(),
            "{method} must be answered locally, got {:?}",
            resp.error
        );
        assert!(resp.result.is_some(), "{method} must carry a result");
        assert_eq!(resp.id, Some(json!(id)), "{method} must be matchable");
    }
}

/// Why: a handler that claimed everything would turn the bridge into a stub —
/// a tool call has to reach the daemon, and fail visibly when it cannot.
/// What: the same handler declines `tools/call`, which is forwarded to a
/// socket nothing serves and comes back as an error naming the daemon.
/// Test: this test.
#[tokio::test]
async fn a_local_handler_that_declines_still_forwards() {
    let bridge = DaemonBridgeJsonRpc::new(config())
        .with_local_handler(|req| (req.method == "initialize").then(|| json!({})));

    let resp = bridge.answer(call("tools/call", 3)).await;
    let message = resp
        .error
        .expect("a declined method is forwarded, and nothing is listening")
        .message;
    assert!(
        message.contains("trusty-test") && message.contains("absent.sock"),
        "the error must name the daemon and the socket: {message}"
    );
    assert_eq!(resp.id, Some(json!(3)));
}

/// Why (#8351 item 3): the reported bridge had baked a socket path in at
/// process start, so a wrong path stayed wrong for the session. Proving
/// re-resolution needs a resolver whose answer CHANGES between two calls —
/// asserting a single resolved path would pass against a cached `PathBuf`.
/// What: a resolver that returns a different path each time; the two error
/// messages name the two different paths.
/// Test: this test.
#[tokio::test]
async fn a_resolver_is_consulted_on_every_request() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let bridge = DaemonBridgeJsonRpc::new(config()).with_socket_resolver(move || {
        let n = seen.fetch_add(1, Ordering::Relaxed);
        Ok(PathBuf::from(format!("/nonexistent/resolved-{n}.sock")))
    });

    let first = bridge.answer(call("tools/call", 1)).await;
    let second = bridge.answer(call("tools/call", 2)).await;

    let first = first.error.expect("nothing serves the first path").message;
    let second = second
        .error
        .expect("nothing serves the second path")
        .message;
    assert!(
        first.contains("resolved-0.sock"),
        "the first dial uses the first resolution: {first}"
    );
    assert!(
        second.contains("resolved-1.sock"),
        "the second dial must re-resolve, not reuse a cached path: {second}"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "one resolution per request"
    );
}

/// Why (the fail-open check): a resolver that cannot answer must not be
/// downgraded to the configured path. That path is exactly the stale value
/// re-resolution exists to escape, so dialling it would report someone else's
/// socket as the cause.
/// What: a failing resolver produces an error carrying the request's id, with
/// no result, naming the resolver's own cause.
/// Test: this test.
#[tokio::test]
async fn a_failing_resolver_is_an_error_not_a_fallback() {
    let bridge = DaemonBridgeJsonRpc::new(config())
        .with_socket_resolver(|| anyhow::bail!("no data directory"));

    let resp = bridge.answer(call("tools/call", 9)).await;
    let message = resp.error.expect("a resolver failure is an error").message;
    assert!(
        message.contains("no data directory"),
        "the resolver's cause must survive: {message}"
    );
    assert!(
        !message.contains("absent.sock"),
        "a failed resolution must not fall back to the configured path: {message}"
    );
    assert!(resp.result.is_none(), "never an empty result");
    assert_eq!(resp.id, Some(json!(9)), "the error must be matchable");
}

/// Why (#8351): the incident's error text could not be attributed to a build,
/// and the binary that produced it had already been replaced.
/// What: the configured bridge version appears in the transport error.
/// Test: this test.
#[tokio::test]
async fn a_named_bridge_version_appears_in_the_transport_error() {
    let bridge = DaemonBridgeJsonRpc::new(config().with_bridge_version("9.9.9-test"));
    let message = bridge
        .answer(call("tools/call", 1))
        .await
        .error
        .expect("nothing is listening")
        .message;
    assert!(
        message.contains("9.9.9-test"),
        "the bridge build must be attributable from the error alone: {message}"
    );
}

/// Why: a bridge that recovered the handshake but not the tool call would
/// still leave the session useless. The daemon going away and coming back at
/// the SAME path between two calls is the real sequence.
/// What: one bridge instance, one path. Call one fails with the socket named
/// while nothing is bound; a listener then binds that path and call two
/// succeeds — no new bridge and no restart.
/// Test: this test.
#[tokio::test]
async fn the_next_call_after_the_daemon_returns_succeeds_on_the_same_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("sockets").join("comes-back.sock");
    let bridge = DaemonBridgeJsonRpc::new(
        UdsBridgeConfig::new(socket.clone(), "trusty-stub")
            .with_request_timeout(Duration::from_secs(5)),
    );

    let down = bridge.answer(call("tools/call", 1)).await;
    let message = down.error.expect("nothing is bound yet").message;
    assert!(
        message.contains("comes-back.sock"),
        "the error must name the socket: {message}"
    );

    // The daemon comes back at the same path.
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut conn, _)) = listener.accept().await else {
            return;
        };
        let mut frame = Vec::new();
        let _ = conn.read_to_end(&mut frame).await;
        let seen: Value = serde_json::from_slice(&frame).unwrap_or_default();
        let body = json!({
            "jsonrpc": "2.0",
            "id": seen.get("id").cloned().unwrap_or_default(),
            "result": {"ok": true},
        });
        let mut bytes = serde_json::to_vec(&body).unwrap_or_default();
        bytes.push(b'\n');
        let _ = conn.write_all(&bytes).await;
        let _ = conn.flush().await;
    });

    let up = bridge.answer(call("tools/call", 2)).await;
    assert!(
        up.error.is_none(),
        "the same bridge must recover once the daemon is back: {:?}",
        up.error
    );
    assert_eq!(up.result, Some(json!({"ok": true})));
}
