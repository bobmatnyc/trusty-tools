//! Hermetic tests for `BaseClient`: credential resolution + HTTP hardening.
//!
//! Why: `BaseClient` must resolve its token through the shared credential
//! resolver (env → `.env.local` → store) and must classify Slack responses
//! correctly — never silently retrying an auth failure as anonymous, and
//! honouring `429 Retry-After` with a bounded backoff. These are exactly the
//! behaviours issue #2638 hardens, so they need coverage that runs in CI with
//! no live Slack token.
//! What: the credential cases use `resolve_key_with` + a `MemoryKeyStore` fake
//! (no filesystem, no keychain, no network); the HTTP cases point the client at
//! a local `wiremock` server returning 200 / 401 / `ok:false` / 429.
//! Test: this file is the test.

use serial_test::serial;
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::api::constants::{SLACK_PROVIDER, SLACK_TOKEN_ENV};
use trusty_channels::slack::api::error::SlackError;
use trusty_common::inference::credentials::{
    env_var_for, resolve_key_with, KeyStore, MemoryKeyStore,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Credential resolution (fake KeyStore — no live token) ─────────────────

/// The provider identifier the client uses must resolve from the store tier
/// when no env var is set.
#[test]
#[serial(slack_credential_env)]
fn slack_provider_resolves_from_store() {
    // SAFETY (edition 2021): set/remove_var is safe; #[serial] prevents races
    // with the env-setting tests in the same group.
    std::env::remove_var(SLACK_TOKEN_ENV);
    let store = MemoryKeyStore::new();
    store.set(SLACK_PROVIDER, "xoxb-fake-store-token").unwrap();
    // Fake store injected directly — no process env, no filesystem, no network.
    assert_eq!(
        resolve_key_with(SLACK_PROVIDER, &store),
        Some("xoxb-fake-store-token".to_string())
    );
}

/// The Slack provider is registered in the resolver's env-var map, so the
/// process-env / `.env.local` tier applies (not just the store tier).
#[test]
fn slack_env_var_is_registered() {
    assert_eq!(env_var_for(SLACK_PROVIDER), Some(SLACK_TOKEN_ENV));
}

/// The env tier wins over the store tier for the Slack provider — proving the
/// documented precedence holds for a non-inference token too.
#[test]
#[serial(slack_credential_env)]
fn env_token_beats_store() {
    // SAFETY (edition 2021): set_var is safe; #[serial] prevents env races.
    std::env::set_var(SLACK_TOKEN_ENV, "xoxb-from-env");
    let store = MemoryKeyStore::new();
    store.set(SLACK_PROVIDER, "xoxb-from-store").unwrap();
    assert_eq!(
        resolve_key_with(SLACK_PROVIDER, &store),
        Some("xoxb-from-env".to_string())
    );
    std::env::remove_var(SLACK_TOKEN_ENV);
}

/// `BaseClient::new()` picks up the token via the real resolver's env tier.
#[test]
#[serial(slack_credential_env)]
fn base_client_new_resolves_env_token() {
    std::env::set_var(SLACK_TOKEN_ENV, "xoxb-from-env");
    let client = BaseClient::new().expect("construct client");
    assert!(
        client.has_token(),
        "token should resolve from SLACK_BOT_TOKEN"
    );
    std::env::remove_var(SLACK_TOKEN_ENV);
}

// ── HTTP hardening (wiremock — no live Slack) ─────────────────────────────

/// A client wired to the mock server with a fixed token.
async fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some("xoxb-test".to_string()))
        .expect("construct client")
}

#[tokio::test]
async fn send_ok_returns_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "channel": "C123",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let value = client
        .call_method("chat.postMessage", &serde_json::json!({"channel": "C123"}))
        .await
        .expect("ok response");
    assert_eq!(value["ok"], true);
    assert_eq!(value["channel"], "C123");
}

#[tokio::test]
async fn auth_401_is_typed_error_no_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth.test"))
        // expect exactly one hit — an auth failure must NOT be retried.
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("auth.test", &serde_json::json!({}))
        .await
        .expect_err("401 should error");
    match err {
        SlackError::Auth { status, .. } => assert_eq!(status, 401),
        other => panic!("expected Auth, got {other:?}"),
    }
    // `expect(1)` is verified on drop: no anonymous retry occurred.
}

#[tokio::test]
async fn auth_ok_false_maps_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error": "invalid_auth",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("auth.test", &serde_json::json!({}))
        .await
        .expect_err("invalid_auth should error");
    match err {
        SlackError::Auth { reason, .. } => assert_eq!(reason, "invalid_auth"),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn api_ok_false_maps_to_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error": "channel_not_found",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("chat.postMessage", &serde_json::json!({"channel": "nope"}))
        .await
        .expect_err("channel_not_found should error");
    match err {
        SlackError::Api(slug) => assert_eq!(slug, "channel_not_found"),
        other => panic!("expected Api, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_retries_then_succeeds() {
    let server = MockServer::start().await;
    // Fallback 200 (mounted first → lower precedence).
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    // One 429 with Retry-After: 0 (mounted last → matched first, once only).
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let value = client
        .call_method("chat.postMessage", &serde_json::json!({}))
        .await
        .expect("should succeed after honoring Retry-After");
    assert_eq!(value["ok"], true);
}

#[tokio::test]
async fn rate_limit_exhausted_returns_typed_error() {
    let server = MockServer::start().await;
    // Persistent 429 (Retry-After: 0 keeps the test fast).
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("chat.postMessage", &serde_json::json!({}))
        .await
        .expect_err("persistent 429 should error");
    match err {
        SlackError::RateLimited { retries, .. } => assert_eq!(retries, 3),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_token_errors_before_network() {
    // No token, and a server that would fail the test if ever hit.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = BaseClient::with_endpoint(server.uri(), None).expect("construct client");
    let err = client
        .call_method("chat.postMessage", &serde_json::json!({}))
        .await
        .expect_err("no token should error");
    assert!(matches!(err, SlackError::MissingToken));
}
