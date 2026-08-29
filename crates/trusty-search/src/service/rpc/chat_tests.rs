//! Socket-versus-HTTP parity for `search.chat` (#6285 slice 5.6).
//!
//! Why: `POST /chat` reaches a network provider on its success path, so the
//! answer a caller gets is not something a test can pin. What CAN be pinned is
//! everything this slice actually added — the decoder both transports run, and
//! every refusal they reach before the first packet leaves the box. Those are
//! also the arms a consumer branches on: an empty question and an unconfigured
//! daemon are what the migrated UI has to render.
//!
//! Every case drives the REAL axum router and the REAL RPC router over ONE
//! shared state, so a `chat_report` core that stopped being the body both
//! transports serve fails here.
//!
//! **Every state below disables both providers explicitly.** `SearchAppState`
//! reads `OPENROUTER_API_KEY` from the environment and `LocalModelConfig`
//! defaults to probing Ollama on `localhost:11434`, so a developer running
//! either would otherwise turn these cases into live network calls.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt as _;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INVALID_PARAMS};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::chat;
use crate::service::rpc::error::CODE_UNAVAILABLE;
use crate::service::server::{build_router_on, SearchAppState};

/// The index every case names, registered so a call reaches the provider
/// resolution rather than stopping at a registry lookup.
const INDEX: &str = "chat-6285";

/// One state with NO chat provider reachable, plus both routers on it.
///
/// Why both from the SAME `Arc`: the property under test one level down is that
/// `build_router_on` and `chat::register` read ONE state. Two `Arc::new` calls
/// would make every comparison below a comparison of two daemons.
fn routers() -> (Router, RpcRouter) {
    let registry = IndexRegistry::new();
    let root = "/nonexistent/chat-6285";
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX.to_string()),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(INDEX, root))),
        root.into(),
    ));
    let state = Arc::new(
        SearchAppState::new(registry)
            .with_local_model(trusty_common::LocalModelConfig {
                enabled: false,
                base_url: "http://127.0.0.1:1".to_string(),
                model: "none".to_string(),
            })
            .with_openrouter_api_key(""),
    );
    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = chat::register(RpcRouter::new(), &state);
    (http, rpc)
}

async fn http_chat(router: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("encode")))
        .expect("build the request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (status, parsed)
}

async fn rpc_chat_err(rpc: &RpcRouter, params: serde_json::Value) -> RpcError {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": chat::METHOD_CHAT, "params": params,
    }))
    .expect("encode the frame");
    let response = rpc.dispatch(&frame).await;
    response
        .error
        .unwrap_or_else(|| panic!("chat must be refused; got {:?}", response.result))
}

/// Assert the socket's error frame is the HTTP refusal, rendered.
///
/// Why not tautological: it re-derives the frame from the `(status, body)` pair
/// the OTHER transport actually produced, so it passes only when both ran the
/// same core and reached the same refusal. Same shape as
/// `admin_tests::assert_same_refusal`.
fn assert_same_refusal(
    http: &(StatusCode, serde_json::Value),
    over_socket: &RpcError,
    expected_code: i64,
    what: &str,
) {
    let (status, body) = http;
    let rendered = crate::service::rpc::error::rpc_error_from_http(*status, body);
    assert_eq!(
        over_socket.code, expected_code,
        "{what}: the socket must classify this refusal as {expected_code}, \
         got {} (HTTP said {status}: {body})",
        over_socket.code
    );
    assert_eq!(
        rendered.code, expected_code,
        "{what}: HTTP's {status} must project onto {expected_code}: {body}"
    );
    assert_eq!(
        over_socket.message, rendered.message,
        "{what}: the socket's message must be the HTTP body's own wording"
    );
}

/// Why: the empty-question check is the first thing `chat_report` does, and a
/// socket that skipped it would send a provider a request with no question in it
/// — billed, and answered with whatever the model makes of an empty prompt. Both
/// transports must refuse it, with the same wording, before any provider is
/// resolved.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_message_is_refused_identically_on_both_transports() {
    let (http, rpc) = routers();
    let body = serde_json::json!({ "index_id": INDEX, "message": "" });

    let over_http = http_chat(&http, body.clone()).await;
    let over_socket = rpc_chat_err(&rpc, body).await;

    assert_eq!(over_http.0, StatusCode::BAD_REQUEST);
    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_INVALID_PARAMS,
        "an empty message",
    );
}

/// Why: an unconfigured daemon is the state the migrated UI meets on a fresh
/// machine, and `503` is the answer it renders as "chat is off". The refusal is
/// retryable — setting `OPENROUTER_API_KEY` or starting Ollama clears it without
/// a code change — so it must project onto `CODE_UNAVAILABLE` rather than the
/// permanent sibling, which would tell the UI to stop offering the panel.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn chat_without_a_provider_is_refused_identically_on_both_transports() {
    let (http, rpc) = routers();
    let body = serde_json::json!({ "index_id": INDEX, "message": "how does auth work?" });

    let over_http = http_chat(&http, body.clone()).await;
    let over_socket = rpc_chat_err(&rpc, body).await;

    assert_eq!(over_http.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_same_refusal(&over_http, &over_socket, CODE_UNAVAILABLE, "no provider");
    assert!(
        over_socket.message.contains("no chat provider available"),
        "the socket must carry the route's own wording: {}",
        over_socket.message
    );
}

/// Why: `ChatRequest` carries the `question`/`message` alias and a serde default
/// on every other field, and the socket decodes THAT type rather than a
/// hand-written mirror of it. A mirror would be the one place the two doors could
/// disagree about what a well-formed request is — so a request phrased the way
/// issue #15 specified must get past the empty-question check on both, and land
/// on the same provider refusal rather than a `400`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_question_alias_decodes_on_both_transports() {
    let (http, rpc) = routers();
    let body = serde_json::json!({ "index_id": INDEX, "question": "how does auth work?" });

    let over_http = http_chat(&http, body.clone()).await;
    let over_socket = rpc_chat_err(&rpc, body).await;

    assert_eq!(
        over_http.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "`question` must be accepted as the message, not refused as absent: {}",
        over_http.1
    );
    assert_same_refusal(&over_http, &over_socket, CODE_UNAVAILABLE, "the alias");
}

/// Why: `POST /chat` collects the provider's deltas into one envelope and
/// answers once, so `search.chat` belongs in the router's UNARY table. A slice
/// that moved it into the streaming one would give the socket a shape HTTP has
/// never had, and `lanes_tests::code_of` — which picks its request flag off
/// `streams::METHODS` — would start reading a `CODE_STREAM_REQUIRED` refusal as
/// a lane verdict.
/// Test: this function IS the test.
#[test]
fn chat_is_a_unary_method_and_not_a_stream() {
    assert!(
        !crate::service::rpc::streams::METHODS.contains(&chat::METHOD_CHAT),
        "search.chat must not be registered as a stream"
    );
}
