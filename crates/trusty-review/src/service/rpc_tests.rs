//! Wire tests for `service::rpc` — the UDS transport, over a real socket.
//!
//! Why (#6277): `handlers_tests.rs` proves each operation's logic by calling it
//! directly. What it cannot prove is the part the transport swap actually
//! changed: that a frame reaches the right method, that a bad payload comes back
//! as a coded error instead of a dropped connection, that the socket is unlinked
//! on shutdown, and that a `review.run` request larger than the shared 8 MiB
//! default is accepted.
//!
//! Test: this is the test module.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::net::UnixListener;

use super::*;
use crate::{
    integrations::search_client::{
        EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
        SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
};

// ── Fakes ─────────────────────────────────────────────────────────────────────

struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: r#"LGTM.
```json
{"verdict":"APPROVE","summary":"ok","findings":[]}
```"#
                .to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

fn test_state() -> AppState {
    AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FakeSearch),
        None,
    )
}

/// Dispatch one frame through the router without a socket.
///
/// The router is the decision half; every test that is about WHICH method ran,
/// or about how a malformed payload is refused, belongs here rather than behind
/// an accept loop.
async fn dispatch(frame: serde_json::Value) -> trusty_common::uds::server::RpcResponse {
    let router = build_router(test_state());
    let bytes = serde_json::to_vec(&frame).expect("encode frame");
    router.dispatch(&bytes).await
}

// ── Router behaviour ──────────────────────────────────────────────────────────

/// Why: a no-argument method is called with `params` ABSENT, which reaches the
/// handler's request type as `Value::Null`. A unit struct refuses null, so
/// getting this wrong makes every health probe in the workspace answer
/// `invalid_params` while the daemon is perfectly healthy — the #4246 false-down
/// class with a new cause.
/// What: a frame with no `params` key answers with a result carrying a `status`.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_answers_with_no_params() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": METHOD_HEALTH,
    }))
    .await;

    assert!(
        !resp.is_error(),
        "health must not answer an error: {resp:?}"
    );
    let result = resp.result.expect("result");
    assert!(
        result.get("status").and_then(|s| s.as_str()).is_some(),
        "the health envelope must carry a string `status` — tctl's probe keys \
         off exactly that field: {result}"
    );
}

/// Why: a client that adds a field to a no-argument call must not be refused —
/// that would turn an additive change on one side into an outage on the other.
/// What: a stray `params` object is ignored.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_answers_with_a_stray_params_object() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": METHOD_HEALTH,
        "params": {"unexpected": true},
    }))
    .await;

    assert!(!resp.is_error(), "a stray field must not refuse: {resp:?}");
}

/// Why: `status` is a distinct method, and a router that mapped two names onto
/// one handler would pass a health test and silently answer the wrong body here.
/// What: the status method answers with `in_flight`.
/// Test: this is the test.
#[tokio::test]
async fn rpc_status_answers_with_the_in_flight_count() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": METHOD_STATUS,
    }))
    .await;

    let result = resp.result.expect("result");
    assert_eq!(
        result.get("in_flight").and_then(serde_json::Value::as_u64),
        Some(0),
        "status must report the in-flight count: {result}"
    );
}

/// REGRESSION (#6277): `review.run` must answer a structured error for a
/// request it cannot use — never hang, never drop the connection.
///
/// Why: over HTTP the axum `Json` extractor turned a body with the wrong shape
/// into a 400 with a reason, and `resolve_diff_source` turned a body naming
/// neither a PR nor a diff into a 400 with a reason. Both had to survive the
/// transport swap or a caller that gets a request wrong learns nothing about
/// why. This is the second of those two cases; `rpc_run_reports_invalid_params_
/// for_an_undecodable_payload` is the first.
/// What: a well-formed frame whose params name no diff source answers
/// `invalid_params` with a message naming the missing field.
/// Test: this is the test.
#[tokio::test]
async fn rpc_run_reports_invalid_params_for_a_request_naming_no_diff() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": METHOD_RUN,
        "params": {},
    }))
    .await;

    let error = resp
        .error
        .expect("an unusable request must answer an error");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_INVALID_PARAMS,
        "got {error:?}"
    );
    assert!(
        error.message.contains("owner"),
        "the message must name what was missing, as the 400 body did: {}",
        error.message
    );
    assert_eq!(resp.id, json!(7), "the request id must be echoed");
}

/// REGRESSION (#6277): a params payload of the WRONG TYPE answers
/// `invalid_params` carrying serde's reason.
///
/// Why: `pr` is a `u64`. Over HTTP, `{"pr": "twelve"}` produced a 400 whose body
/// said what serde objected to. A transport that answered a bare hang-up, or
/// that silently coerced the value, would be a regression a health check could
/// not see.
///
/// **One difference from the axum `Json` extractor, stated rather than hidden.**
/// axum decodes through `serde_path_to_error`, so its message names the FIELD:
/// `pr: invalid type: string "twelve", expected u64`. `RpcRouter::typed` decodes
/// with plain `serde_json::from_value`, so the message carries the type mismatch
/// without the path. The error CODE, the structured shape, and the reason all
/// survive; only the field name is lost. Closing that gap means threading
/// `serde_path_to_error` through `trusty_common::uds::server::typed_method` —
/// a shared-crate change and a new workspace dependency, which is not what this
/// transport swap is for.
///
/// What: a string where a number belongs answers `invalid_params`, and the
/// message carries serde's own explanation.
/// Test: this is the test.
#[tokio::test]
async fn rpc_run_reports_invalid_params_for_an_undecodable_payload() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": METHOD_RUN,
        "params": {"owner": "o", "repo": "r", "pr": "twelve"},
    }))
    .await;

    let error = resp.error.expect("a mistyped field must answer an error");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_INVALID_PARAMS,
        "got {error:?}"
    );
    assert!(
        error.message.contains("invalid type") && error.message.contains("expected u64"),
        "serde's reason must survive verbatim, or a caller learns nothing about \
         what it got wrong: {}",
        error.message
    );
}

/// Why: a client on an older or newer method name must read which names exist
/// rather than a dropped connection.
/// What: an unknown method answers `method_not_found` listing the three served.
/// Test: this is the test.
#[tokio::test]
async fn rpc_reports_method_not_found_for_an_unknown_method() {
    let resp = dispatch(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "review.nope",
    }))
    .await;

    let error = resp.error.expect("unknown method must answer an error");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_METHOD_NOT_FOUND,
        "got {error:?}"
    );
    for method in [METHOD_HEALTH, METHOD_STATUS, METHOD_RUN] {
        assert!(
            error.message.contains(method),
            "the refusal must name {method}, or a client author cannot find the \
             typo: {}",
            error.message
        );
    }
}

// ── Socket behaviour ──────────────────────────────────────────────────────────

/// Spawn the accept loop over `listener` and return a shutdown trigger.
fn spawn_serve(
    listener: UnixListener,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let router = Arc::new(build_router(test_state()));
    let handle = tokio::spawn(async move {
        trusty_common::uds::server::serve_until(&listener, router, super::serve_options(), async {
            let _ = rx.await;
        })
        .await;
    });
    (tx, handle)
}

/// Why: the router tests prove the decision; this proves the whole path — a
/// hardened bind, the peer-uid check, the framing, and one answered frame.
/// What: binds a temp socket, dials it with the shared client, and asserts the
/// health envelope comes back.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_answers_over_a_real_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("review.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let (shutdown, served) = spawn_serve(listener);

    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request(
            &socket,
            &json!({"jsonrpc": "2.0", "id": 1, "method": METHOD_HEALTH}),
            Duration::from_secs(10),
        )
        .await
        .expect("round trip");

    assert!(!response.is_error(), "got {response:?}");
    assert!(
        response
            .result
            .as_ref()
            .and_then(|r| r.get("version"))
            .is_some(),
        "the console reads `version` off this envelope to render the service \
         card: {response:?}"
    );

    let _ = shutdown.send(());
    served.await.expect("join");
}

/// REGRESSION (#6277): the retired `http_addr` files must actually be deleted.
///
/// Why: on an upgraded machine both files are still on disk holding
/// `127.0.0.1:7891`, and nothing rewrites them. A stale discovery file is not
/// inert — `tctl`'s bootstrap guard resolves a member's port through
/// `read_daemon_addr`, so leaving it would make an install refuse because some
/// unrelated process holds a port trusty-review no longer binds. An absent file
/// must be tolerated silently: that is the fresh-install case, and every start
/// after the first.
/// What: removes a planted file, then runs again over the same now-absent path.
/// Test: this is the test.
#[test]
fn remove_if_present_deletes_a_stale_file_and_tolerates_an_absent_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stale = tmp.path().join("http_addr");
    std::fs::write(&stale, "127.0.0.1:7891").expect("plant a stale discovery file");

    super::remove_if_present(&stale);
    assert!(!stale.exists(), "the stale file must be gone");

    // Idempotent: the second start must not fail on the file it already removed.
    super::remove_if_present(&stale);
}

/// REGRESSION (#6277 design review): the socket file must be GONE after
/// shutdown, not assumed gone.
///
/// Why: neither `bind_hardened` nor tokio's `UnixListener::Drop` removes the
/// path. A daemon that returned without unlinking leaves a file the next
/// `bind_hardened` refuses — under launchd's `KeepAlive::Always` that is a
/// ten-second crash loop with no operator-visible cause.
///
/// The test drives `serve_with_shutdown`, which is the production body — the
/// bind, the loop, and the unlink. Nothing here touches the file, so deleting
/// the `remove_file` in `rpc.rs` turns this red. An earlier version of this test
/// spawned `serve_until` and then deleted the socket itself; it asserted the
/// filesystem, not the daemon, and passed either way.
/// What: serves on a temp path, resolves the shutdown future, and asserts the
/// file is gone once `serve_with_shutdown` returns.
/// Test: this is the test.
#[tokio::test]
async fn rpc_unlinks_its_socket_on_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("review.sock");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let socket_for_serve = socket.clone();
    let served = tokio::spawn(async move {
        super::serve_with_shutdown(test_state(), &socket_for_serve, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    // The bind is inside the task, so wait for the file to appear before asking
    // it to stop — otherwise a fast shutdown could race the bind and the
    // assertion below would pass without the unlink ever running.
    wait_for_socket(&socket).await;

    let _ = shutdown_tx.send(());
    served
        .await
        .expect("join")
        .expect("serve returned an error");

    assert!(
        !socket.exists(),
        "a socket file left behind makes the next launchd start fail its bind"
    );
}

/// Poll until `socket` exists, or panic after a bounded wait.
///
/// A condition poll rather than a sleep: the bind takes microseconds, and a
/// fixed sleep would either flake on a loaded machine or slow every run.
async fn wait_for_socket(socket: &std::path::Path) {
    for _ in 0..200 {
        if socket.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} never appeared", socket.display());
}

/// Why (#6277 design review): the shared frame budget is 8 MiB, and a
/// `review.run` request carries a caller-supplied raw diff that nothing bounds
/// before it arrives. Under the default the connection is dropped unanswered,
/// and the failure would only ever appear on a large PR.
///
/// The frame MUST go over a real socket. The budget is enforced by the
/// `take(max_frame_bytes)` in `serve_until`'s `handle_connection`, so a test
/// that called `RpcRouter::dispatch` on the bytes directly would bypass the only
/// code the budget lives in — `serve_options()` could regress to
/// `RpcServeOptions::default()` and that test would still pass while every large
/// `review.run` in production was silently dropped. This one goes through
/// `spawn_serve`, which uses `super::serve_options()`.
///
/// What: dials with the PLAIN client — `send_framed_request` writes the request
/// uncapped, which is the other half of the asymmetry `MAX_FRAME_BYTES` documents
/// — with a `local_diff_text` past the shared default, and asserts a response
/// frame comes back carrying the request id. The answer's CONTENT is the
/// pipeline's business, not this test's.
/// Test: this is the test.
#[tokio::test]
async fn rpc_accepts_a_review_request_larger_than_the_shared_default() {
    const {
        assert!(
            MAX_FRAME_BYTES > trusty_common::uds::MAX_FRAME_BYTES,
            "this test is only meaningful while this service raises the budget"
        );
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("review.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let (shutdown, served) = spawn_serve(listener);

    // One byte-per-char diff past the shared default, still inside ours.
    let oversized = "x".repeat((trusty_common::uds::MAX_FRAME_BYTES + 1024) as usize);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": METHOD_RUN,
        "params": {"local_diff_text": oversized},
    });
    let encoded_len = serde_json::to_vec(&request).expect("encode").len() as u64;
    assert!(
        encoded_len > trusty_common::uds::MAX_FRAME_BYTES,
        "the frame must actually exceed the shared default to prove anything"
    );
    assert!(
        encoded_len < MAX_FRAME_BYTES,
        "a {encoded_len} byte frame must fit this service's {MAX_FRAME_BYTES} byte budget"
    );

    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request(&socket, &request, Duration::from_secs(60))
            .await
            .expect("an oversized review.run must be answered, not dropped");

    assert_eq!(response.id, json!(9), "the oversized request was answered");

    let _ = shutdown.send(());
    served.await.expect("join");
}
