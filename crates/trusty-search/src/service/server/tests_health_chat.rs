//! Tests for `/health`'s `chat_available` field (#9030).
use super::*;

/// #9030: `chat_available` is true with a chat provider and false without.
///
/// Why: the console's chat panel gates on this field instead of guessing from
/// the transport; a wrong `true` offers a chat that answers 503.
/// What: reads the field off `health_report` (the body both doors serve) as a
/// raw `Value`, so a missing key fails on the assertion, not at compile time.
/// The local-model probe is disabled so the result depends on the key alone.
/// Test: this function IS the test.
#[tokio::test]
async fn health_reports_chat_available_only_with_a_provider() {
    let build = |key: &str| {
        let mut state = SearchAppState::new(crate::core::registry::IndexRegistry::new())
            .with_openrouter_api_key(key);
        state.local_model.enabled = false;
        std::sync::Arc::new(state)
    };
    let with = health_report(build("sk-test-9030")).await;
    assert_eq!(with["chat_available"], serde_json::json!(true), "{with}");
    let without = health_report(build("")).await;
    assert_eq!(
        without["chat_available"],
        serde_json::json!(false),
        "{without}"
    );
}

/// #9030: an unresolved or timed-out provider lookup reads `false`.
///
/// Why: the error arm of `chat_available`; "cannot tell" must never be `true`.
/// What: a never-completing resolution under a 20 ms budget yields `false`,
/// and a completed `true` passes through (so the helper is not constant).
/// Test: this function IS the test.
#[tokio::test]
async fn chat_availability_fails_closed_when_resolution_does_not_finish() {
    use super::health::resolve_chat_available;
    use std::time::Duration;
    let hung = resolve_chat_available(std::future::pending::<bool>(), Duration::from_millis(20));
    assert!(!hung.await);
    assert!(resolve_chat_available(async { true }, Duration::from_millis(20)).await);
}

/// #9030: a `/health` call made before the provider exists never decides it.
///
/// Why: `chat_provider` fills a daemon-lifetime `OnceCell`; an early health
/// poll at boot, before Ollama is up, locked in `None` and `search.chat`
/// answered 503 until restart.
/// What: serves `/v1/models` on an ephemeral port (ADR-0032) that answers 503
/// until a flag flips, so the probe sees no provider; calls `health_report`
/// (`chat_available` is false), flips the flag to 200, and asserts a later
/// `chat_provider()` resolves it.
/// Test: this function IS the test.
#[tokio::test]
async fn a_health_call_before_the_provider_exists_does_not_decide_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let up = std::sync::Arc::new(AtomicBool::new(false));
    let flag = up.clone();
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let up = flag.load(Ordering::SeqCst);
            async move {
                if up {
                    (axum::http::StatusCode::OK, "{}")
                } else {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "")
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut state = SearchAppState::new(crate::core::registry::IndexRegistry::new())
        .with_openrouter_api_key("");
    state.local_model.enabled = true;
    state.local_model.base_url = format!("http://{addr}");
    let state = std::sync::Arc::new(state);

    let early = health_report(state.clone()).await;
    assert_eq!(early["chat_available"], serde_json::json!(false), "{early}");

    up.store(true, Ordering::SeqCst);
    let later = state.chat_provider().await;
    server.abort();
    assert!(
        later.is_some(),
        "an early /health poll must not lock the chat provider in as None"
    );
}

/// #9030: once a chat request has decided the provider, `/health` reads it.
///
/// Why: the empty-cell path probes; the filled-cell path must not, and must
/// agree with what chat decided.
/// What: resolves the provider through `chat_provider` first (key only, local
/// probe off), then asserts `health_report` says `true`.
/// Test: this function IS the test.
#[tokio::test]
async fn health_reads_a_provider_the_cell_already_holds() {
    let mut state = SearchAppState::new(crate::core::registry::IndexRegistry::new())
        .with_openrouter_api_key("sk-test-9030");
    state.local_model.enabled = false;
    let state = std::sync::Arc::new(state);
    assert!(state.chat_provider().await.is_some());
    let report = health_report(state).await;
    assert_eq!(
        report["chat_available"],
        serde_json::json!(true),
        "{report}"
    );
}
