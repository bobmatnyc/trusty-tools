//! Unit tests for `InferenceClient` and `shared_inference_enabled`.
//!
//! Why: kept in its own file so `mod.rs` stays under the 500-SLOC production
//! cap. Covers the flag reader, the #2245 deferred-credential contract, adapter
//! caching, and — the highest-value regression here — that the fixed
//! `OPENROUTER_ROUTING_SLUG` can never resolve to the Anthropic-direct
//! adapter no matter what other credentials happen to be configured.
//! Test: this IS the test module.

use super::*;
use serial_test::serial;
use trusty_common::inference::ChatMessage;
use trusty_common::inference::credentials::MemoryKeyStore;
use trusty_common::inference::test_support::MockInferenceServer;

/// Why: `dispatch_turn` and any future call site must agree on the exact
/// value that turns the seam on; only `"1"` counts, everything else (unset,
/// `"true"`, `"0"`) must keep the legacy path.
/// Test: itself.
#[test]
#[serial]
fn shared_inference_enabled_reads_flag() {
    let _g = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var(SHARED_INFERENCE_ENV);
    }
    assert!(!shared_inference_enabled(), "unset must be OFF");
    unsafe {
        std::env::set_var(SHARED_INFERENCE_ENV, "true");
    }
    assert!(!shared_inference_enabled(), "\"true\" must NOT enable it");
    unsafe {
        std::env::set_var(SHARED_INFERENCE_ENV, "1");
    }
    assert!(shared_inference_enabled(), "\"1\" must enable it");
    unsafe {
        std::env::remove_var(SHARED_INFERENCE_ENV);
    }
}

/// Why: the #2245 deferred-failure contract — building an `InferenceClient`
/// must never touch credentials, so it can never fail at construction time.
/// What: constructs against an empty `MemoryKeyStore` (no factories even
/// registered) and asserts the constructor itself is infallible (it returns
/// `Self`, not a `Result`).
/// Test: itself.
#[test]
fn constructs_without_touching_credentials() {
    let _client =
        InferenceClient::with_configurator(Configurator::new(), Box::new(MemoryKeyStore::new()));
    // No panic / no Result to unwrap — construction is infallible by type.
}

/// Why: with no OpenRouter credential resolvable anywhere, `chat()` must fail
/// with `MissingCredential` — never a panic or a silent empty response — and
/// only at call time, not at construction.
/// What: skips (does not fail) when the ambient environment happens to carry
/// a real `OPENROUTER_API_KEY`, mirroring `trusty-code`'s identical guard.
/// Test: itself.
#[tokio::test]
async fn missing_openrouter_key_errors_at_chat_time() {
    if std::env::var("OPENROUTER_API_KEY").is_ok_and(|v| !v.is_empty()) {
        return; // real key present locally — don't assert MissingCredential
    }
    let mut configurator = Configurator::new();
    trusty_common::inference::register_default_factories(&mut configurator);
    let client = InferenceClient::with_configurator(configurator, Box::new(MemoryKeyStore::new()));
    let req = ChatRequest::new(
        "unused", // Step 1+2 always routes on the fixed slug, never this value.
        vec![ChatMessage::user("hi")],
    );
    let err = client
        .chat(&req)
        .await
        .expect_err("must fail without a key");
    assert!(
        matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::OpenRouter
            }
        ),
        "expected MissingCredential{{OpenRouter}}, got: {err:?}"
    );
}

/// Why: adapter construction builds a `reqwest::Client`; a second `chat()`
/// call must reuse the SAME adapter (pointer-identical) rather than rebuild
/// it, preserving connection pooling across turns.
/// What: seeds a store with a fake OpenRouter key (adapter construction never
/// makes a network call, so this succeeds without a live server), calls
/// `adapter()` twice, and asserts `Arc::ptr_eq`.
/// Test: itself.
#[tokio::test]
async fn adapter_is_cached_across_calls() {
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").expect("seed store"); // pragma: allowlist secret
    let mut configurator = Configurator::new();
    trusty_common::inference::register_default_factories(&mut configurator);
    let client = InferenceClient::with_configurator(configurator, Box::new(store));

    let first = client.adapter().await.expect("first build");
    let second = client.adapter().await.expect("cached build");
    assert!(
        Arc::ptr_eq(&first, &second),
        "second call must reuse the cached adapter"
    );
}

/// Why: `InferenceClient::with_store` must honour the SAME `OPENROUTER_BASE_URL`
/// override the legacy `async-openai` client reads
/// (`llm::helpers::create_client`, `llm::adapter::openrouter_endpoint`) — the
/// property the offline e2e test's byte-parity claim depends on.
/// What: points `OPENROUTER_BASE_URL` at a loopback `MockInferenceServer`,
/// builds a client via `with_store` (the production constructor path, not
/// `with_configurator`), sends one request, and asserts the mock actually
/// received it — proving the override reached the registered OpenRouter
/// factory, not just a test-only bypass.
/// Test: itself.
#[tokio::test]
#[serial]
async fn with_store_honours_openrouter_base_url_override() {
    // NOTE: no `ENV_LOCK` guard here — it is a `std::sync::Mutex` and this
    // test holds env-var state across `.await` points, which clippy's
    // `await_holding_lock` correctly forbids for a sync mutex. `#[serial]`
    // (unnamed group) alone is sufficient: it serializes against every other
    // unnamed `#[serial]` test in this binary, and no other test in this file
    // touches `OPENROUTER_BASE_URL`.
    let server = MockInferenceServer::spawn(
        200,
        serde_json::json!({
            "id": "gen-mock",
            "choices": [{"message": {"role": "assistant", "content": "pong-mock", "tool_calls": []},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        }),
    )
    .await
    .expect("spawn mock");
    unsafe {
        std::env::set_var(OPENROUTER_BASE_URL_ENV, server.url());
    }

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").expect("seed store"); // pragma: allowlist secret
    let client = InferenceClient::with_store(Box::new(store));
    let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("hi")]);
    let resp = client
        .chat(&req)
        .await
        .expect("chat via overridden base url");
    assert_eq!(resp.first_text().as_deref(), Some("pong-mock"));
    assert!(
        server.last_request().is_some(),
        "the mock must have received the request"
    );

    unsafe {
        std::env::remove_var(OPENROUTER_BASE_URL_ENV);
    }
}

/// Why: `shared_client()` must hand out the SAME process-wide instance on
/// every call so its adapter cache (and the underlying `reqwest` connection
/// pool) is actually shared across turns, mirroring `http_client_returns_same_instance`
/// in `llm::http::tests`.
/// Test: itself.
#[test]
fn shared_client_returns_same_instance() {
    let a = shared_client();
    let b = shared_client();
    assert!(std::ptr::eq(a, b));
}

/// The regression this module's design exists to prevent: the fixed
/// [`OPENROUTER_ROUTING_SLUG`] must resolve to `ProviderId::OpenRouter` — and
/// ONLY ever fail with `MissingCredential{OpenRouter}` otherwise — even when
/// an `anthropic` credential IS present in the store (simulating a developer
/// with `ANTHROPIC_API_KEY` set, the common case `llm::credentials::pick_credentials`
/// prefers). If this ever resolved to `ProviderId::Anthropic` instead, traffic
/// this client is meant to send to OpenRouter would silently defect onto the
/// out-of-scope Anthropic-direct path.
///
/// Why: this is the single highest-value correctness test in this module —
/// see the module doc's 🔴 scope-boundary note.
/// What: seeds an `anthropic` key in the store (no `openrouter` key in the
/// store), builds against the real registered factories, and asserts the
/// outcome is EITHER `adapter.name() == "openrouter"` (when the ambient
/// process env — e.g. a real `OPENROUTER_API_KEY` in `.env.local` on a dev
/// machine — resolves OpenRouter's own credential) OR
/// `MissingCredential{OpenRouter}` (when it does not). Neither branch may
/// ever name Anthropic — that's the property under test, checked
/// unconditionally so this test is meaningful in every environment rather
/// than skipping like the sibling credential tests.
/// Test: itself.
#[tokio::test]
async fn openrouter_route_never_resolves_to_anthropic_even_when_anthropic_key_present() {
    let store = MemoryKeyStore::new();
    store.set("anthropic", "sk-ant-fake").expect("seed store"); // pragma: allowlist secret
    let mut configurator = Configurator::new();
    trusty_common::inference::register_default_factories(&mut configurator);
    let client = InferenceClient::with_configurator(configurator, Box::new(store));

    // `.expect`/`.expect_err` would require `Arc<dyn InferenceAdapter>: Debug`,
    // which it is not (trait object for a non-Debug trait) — match manually.
    match client.adapter().await {
        Ok(adapter) => assert_eq!(
            adapter.name(),
            "openrouter",
            "must never silently build the Anthropic-direct adapter"
        ),
        Err(InferenceError::MissingCredential { provider }) => assert_eq!(
            provider,
            ProviderId::OpenRouter,
            "must name OpenRouter, never silently resolve to Anthropic"
        ),
        Err(other) => {
            panic!("expected Ok(openrouter) or MissingCredential{{OpenRouter}}, got: {other:?}")
        }
    }
}
