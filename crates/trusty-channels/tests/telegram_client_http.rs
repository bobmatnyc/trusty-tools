//! Hermetic tests for the Telegram `BaseClient`: credential resolution + HTTP
//! hardening.
//!
//! Why: `BaseClient` must resolve its token through the shared credential
//! resolver (env → `.env.local` → store) and must classify Bot API responses
//! correctly — never silently retrying an auth failure anonymously, and
//! honouring `429` retry hints with a bounded backoff. These are exactly the
//! behaviours issue #2641 wires, so they need coverage that runs in CI with no
//! live Telegram token.
//! What: the credential cases use `resolve_key_with` + a `MemoryKeyStore` fake
//! (no filesystem, no keychain, no network); the HTTP cases point the client at
//! a local `wiremock` server returning 200 / 401 / `ok:false` / 429. The Bot API
//! embeds the token in the URL path (`/bot<token>/<method>`), so the mocked
//! paths include the fixed test token.
//! Test: this file is the test.

use serial_test::serial;
use trusty_channels::telegram::api::client::BaseClient;
use trusty_channels::telegram::api::constants::{TELEGRAM_PROVIDER, TELEGRAM_TOKEN_ENV};
use trusty_channels::telegram::api::error::TelegramError;
use trusty_common::inference::credentials::{
    env_var_for, resolve_key_with, KeyStore, MemoryKeyStore,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// The fixed token every HTTP case uses; the Bot API puts it in the URL path.
const TEST_TOKEN: &str = "123456:test-token";

// ── Credential resolution (fake KeyStore — no live token) ─────────────────

/// The provider identifier the client uses must resolve from the store tier
/// when no env var is set.
#[test]
#[serial(telegram_credential_env)]
fn telegram_provider_resolves_from_store() {
    // SAFETY (edition 2021): set/remove_var is safe; #[serial] prevents races
    // with the env-setting tests in the same group.
    std::env::remove_var(TELEGRAM_TOKEN_ENV);
    let store = MemoryKeyStore::new();
    store.set(TELEGRAM_PROVIDER, "fake-store-token").unwrap();
    // Fake store injected directly — no process env, no filesystem, no network.
    assert_eq!(
        resolve_key_with(TELEGRAM_PROVIDER, &store),
        Some("fake-store-token".to_string())
    );
}

/// The Telegram provider is registered in the resolver's env-var map, so the
/// process-env / `.env.local` tier applies (not just the store tier).
#[test]
fn telegram_env_var_is_registered() {
    assert_eq!(env_var_for(TELEGRAM_PROVIDER), Some(TELEGRAM_TOKEN_ENV));
}

/// The env tier wins over the store tier for the Telegram provider — proving
/// the documented precedence holds for a non-inference token too.
#[test]
#[serial(telegram_credential_env)]
fn env_token_beats_store() {
    // SAFETY (edition 2021): set_var is safe; #[serial] prevents env races.
    std::env::set_var(TELEGRAM_TOKEN_ENV, "from-env-token");
    let store = MemoryKeyStore::new();
    store.set(TELEGRAM_PROVIDER, "from-store-token").unwrap();
    assert_eq!(
        resolve_key_with(TELEGRAM_PROVIDER, &store),
        Some("from-env-token".to_string())
    );
    std::env::remove_var(TELEGRAM_TOKEN_ENV);
}

/// `BaseClient::new()` picks up the token via the real resolver's env tier.
#[test]
#[serial(telegram_credential_env)]
fn base_client_new_resolves_env_token() {
    std::env::set_var(TELEGRAM_TOKEN_ENV, "from-env-token");
    let client = BaseClient::new().expect("construct client");
    assert!(
        client.has_token(),
        "token should resolve from TELEGRAM_BOT_TOKEN"
    );
    std::env::remove_var(TELEGRAM_TOKEN_ENV);
}

// ── HTTP hardening (wiremock — no live Telegram) ──────────────────────────

/// A client wired to the mock server with a fixed token.
async fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some(TEST_TOKEN.to_string())).expect("construct client")
}

/// The URL path the mock must match for `method` (token is in the path).
fn method_path(m: &str) -> String {
    format!("/bot{TEST_TOKEN}/{m}")
}

#[tokio::test]
async fn send_ok_returns_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(method_path("sendMessage")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 42 },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let value = client
        .call_method(
            "sendMessage",
            &serde_json::json!({"chat_id": "123", "text": "hi"}),
        )
        .await
        .expect("ok response");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["message_id"], 42);
}

#[tokio::test]
async fn auth_401_is_typed_error_no_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(method_path("getMe")))
        // expect exactly one hit — an auth failure must NOT be retried.
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("getMe", &serde_json::json!({}))
        .await
        .expect_err("401 should error");
    match err {
        TelegramError::Auth { status, .. } => assert_eq!(status, 401),
        other => panic!("expected Auth, got {other:?}"),
    }
    // `expect(1)` is verified on drop: no anonymous retry occurred.
}

#[tokio::test]
async fn auth_ok_false_maps_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(method_path("getMe")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 401,
            "description": "Unauthorized",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("getMe", &serde_json::json!({}))
        .await
        .expect_err("error_code 401 should error");
    match err {
        TelegramError::Auth { reason, .. } => assert_eq!(reason, "Unauthorized"),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn api_ok_false_maps_to_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(method_path("getChat")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: chat not found",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("getChat", &serde_json::json!({"chat_id": "nope"}))
        .await
        .expect_err("chat not found should error");
    match err {
        TelegramError::Api(desc) => assert_eq!(desc, "Bad Request: chat not found"),
        other => panic!("expected Api, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_retries_then_succeeds() {
    let server = MockServer::start().await;
    // Fallback 200 (mounted first → lower precedence).
    Mock::given(method("POST"))
        .and(path(method_path("sendMessage")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    // One 429 with a body retry_after: 0 (mounted last → matched first, once).
    Mock::given(method("POST"))
        .and(path(method_path("sendMessage")))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests",
            "parameters": { "retry_after": 0 },
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let value = client
        .call_method("sendMessage", &serde_json::json!({}))
        .await
        .expect("should succeed after honoring retry_after");
    assert_eq!(value["ok"], true);
}

#[tokio::test]
async fn rate_limit_exhausted_returns_typed_error() {
    let server = MockServer::start().await;
    // Persistent 429 (retry_after: 0 keeps the test fast).
    Mock::given(method("POST"))
        .and(path(method_path("sendMessage")))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests",
            "parameters": { "retry_after": 0 },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .call_method("sendMessage", &serde_json::json!({}))
        .await
        .expect_err("persistent 429 should error");
    match err {
        TelegramError::RateLimited { retries, .. } => assert_eq!(retries, 3),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_token_errors_before_network() {
    // No token, and a server that would fail the test if ever hit.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = BaseClient::with_endpoint(server.uri(), None).expect("construct client");
    let err = client
        .call_method("sendMessage", &serde_json::json!({}))
        .await
        .expect_err("no token should error");
    assert!(matches!(err, TelegramError::MissingToken));
}

/// Regression test for the HIGH finding: the Bot API puts the token in the
/// URL PATH (`/bot<token>/<method>`), and `reqwest::Error`'s `Display` *and*
/// derived `Debug` both render the attached request URL verbatim (reqwest 0.12
/// `with_url`). If `TelegramError::Transport` ever goes back to holding (or
/// `#[from]`-converting) the raw `reqwest::Error`, a transport-class failure
/// would leak the live bot token into any log line or `?`-propagated error
/// message. This forces a genuine connect-class transport error against a
/// token-bearing URL and asserts the sentinel token appears in NEITHER
/// `Display` NOR `Debug` of the resulting error.
#[tokio::test]
async fn transport_error_display_and_debug_never_contain_the_bot_token() {
    // Bind an ephemeral local port, then immediately drop the listener so
    // nothing is listening — the next connect attempt fails fast with
    // "connection refused" (a genuine transport-class reqwest::Error), with no
    // live network dependency and no flakiness.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let sentinel_token = "123456:SECRET-SENTINEL-TOKEN";
    let client =
        BaseClient::with_endpoint(format!("http://{addr}"), Some(sentinel_token.to_string()))
            .expect("construct client");

    let err = client
        .call_method("sendMessage", &serde_json::json!({}))
        .await
        .expect_err("connecting to a closed local port must fail");

    assert!(
        matches!(&err, TelegramError::Transport { .. }),
        "expected Transport, got {err:?}"
    );

    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(
        !display.contains(sentinel_token),
        "Display leaked the bot token: {display}"
    );
    assert!(
        !debug.contains(sentinel_token),
        "Debug leaked the bot token: {debug}"
    );
    // Broader signal: neither rendering should carry the request's host:port
    // either — a hint that no raw request/URL context leaked through at all.
    let addr_str = addr.to_string();
    assert!(
        !display.contains(&addr_str),
        "Display leaked the request URL: {display}"
    );
    assert!(
        !debug.contains(&addr_str),
        "Debug leaked the request URL: {debug}"
    );
}
