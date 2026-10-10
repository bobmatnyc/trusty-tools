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
