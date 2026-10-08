//! Service-account key and token-source tests (#9448 S1), moved from
//! `tests/gchat_http.rs` when `GchatClient::token_source`,
//! `TokenSource::new` and `ServiceAccountKey::from_file` became crate-private,
//! so no public call hands out a `chat.bot` token without the route check
//! (ruling 7). Bodies are unchanged apart from the shared fixtures.

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    client, mount_token_expect, throwaway_key, token as mock_token, token_requests, write_key_file,
    SA_EMAIL,
};
use crate::gchat::api::auth::{Clock, ServiceAccountKey};
use crate::gchat::api::constants::{
    JWT_BEARER_GRANT_TYPE, SCOPE_CHAT_BOT, SCOPE_PUBSUB, TOKEN_AUDIENCE, TOKEN_REFRESH_MARGIN_SECS,
};
use crate::gchat::api::error::GchatError;

#[test]
fn key_file_that_is_not_a_service_account_key_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.json");
    std::fs::write(&path, r#"{"client_email":"x","private_key":"not a pem"}"#).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let err = ServiceAccountKey::from_file(&path).expect_err("bad PEM must be refused");
    assert!(matches!(err, GchatError::KeyFileInvalid { .. }), "{err:?}");
    assert!(
        !err.to_string().contains("not a pem"),
        "error echoes key content"
    );
}

#[tokio::test]
async fn token_is_cached_within_expiry_and_refreshed_past_margin() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token_expect(&server, 3600, 2).await;
    let now = Arc::new(AtomicU64::new(1_000_000));
    let clock_now = Arc::clone(&now);
    let clock: Clock = Arc::new(move || clock_now.load(Ordering::SeqCst));
    let client = client(&server, dir.path()).with_clock(clock);
    let tokens = client.token_source();

    tokens.access_token().await.expect("first token");
    now.fetch_add(60, Ordering::SeqCst);
    tokens.access_token().await.expect("cached token");
    assert_eq!(
        token_requests(&server).await.len(),
        1,
        "second call within expiry hit the endpoint"
    );

    // Exactly the refresh margin before expiry: the cached token is stale.
    now.store(
        1_000_000 + 3600 - TOKEN_REFRESH_MARGIN_SECS,
        Ordering::SeqCst,
    );
    tokens.access_token().await.expect("refreshed token");
    assert_eq!(
        token_requests(&server).await.len(),
        2,
        "no refresh past the margin"
    );
}

#[tokio::test]
async fn jwt_assertion_verifies_with_the_public_key_and_carries_the_claims() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token_expect(&server, 3600, 1).await;
    let client = client(&server, dir.path());
    client.token_source().access_token().await.expect("token");

    let request = token_requests(&server).await.remove(0);
    let form = String::from_utf8(request.body).expect("utf-8 form");
    let field = |name: &str| -> String {
        form.split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("form lacks {name}: {form}"))
            .to_string()
    };
    // The grant type is URL-encoded; the JWT alphabet needs no encoding.
    assert_eq!(
        field("grant_type").replace("%3A", ":"),
        JWT_BEARER_GRANT_TYPE
    );
    let assertion = field("assertion");

    let public =
        jsonwebtoken::DecodingKey::from_rsa_pem(throwaway_key().1.as_bytes()).expect("public key");
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[TOKEN_AUDIENCE]);
    validation.set_issuer(&[SA_EMAIL]);
    let decoded = jsonwebtoken::decode::<Value>(&assertion, &public, &validation)
        .expect("assertion verifies with the throwaway public key");
    let claims = decoded.claims;
    assert_eq!(decoded.header.kid.as_deref(), Some("kid-1"));
    assert_eq!(claims["iss"], SA_EMAIL);
    assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(claims["scope"], format!("{SCOPE_CHAT_BOT} {SCOPE_PUBSUB}"));
    let iat = claims["iat"].as_u64().expect("iat");
    assert_eq!(claims["exp"].as_u64(), Some(iat + 3600));
}

#[tokio::test]
async fn token_endpoint_refusal_is_typed_and_redacted() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    // A hostile endpoint that echoes the signed assertion back in its error.
    let echoed = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = Arc::clone(&echoed);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |req: &wiremock::Request| {
            let form = String::from_utf8_lossy(&req.body).into_owned();
            let assertion = form
                .split('&')
                .find_map(|kv| kv.strip_prefix("assertion="))
                .unwrap_or_default()
                .to_string();
            *seen.lock().expect("lock") = assertion.clone();
            ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": format!("Invalid JWT Signature: {assertion}")
            }))
        })
        .mount(&server)
        .await;
    let err = client(&server, dir.path())
        .token_source()
        .access_token()
        .await
        .expect_err("refused grant");
    assert!(
        matches!(&err, GchatError::TokenEndpoint { status: 400, message } if message.starts_with("invalid_grant")),
        "{err:?}"
    );
    let assertion = echoed.lock().expect("lock").clone();
    assert!(
        assertion.len() > 100,
        "mock saw no assertion: {assertion:?}"
    );
    // The message is truncated to 300 chars, which can cut the full assertion
    // short, so check for its leading 60 chars.
    let prefix = &assertion[..60];
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(prefix), "assertion leaked: {text}");
    }
}

#[tokio::test]
async fn debug_output_never_contains_key_material_or_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token_expect(&server, 3600, 1).await;
    let key_path = write_key_file(dir.path(), 0o600);
    let key = ServiceAccountKey::from_file(&key_path).expect("key");
    let client = client(&server, dir.path());
    let token = client.token_source().access_token().await.expect("token");

    let private_pem = &throwaway_key().0;
    // A line from the middle of the PEM body: unique key material.
    let key_line = private_pem.lines().nth(5).expect("PEM body line");
    let rendered = [
        format!("{key:?}"),
        format!("{:?}", client.token_source()),
        format!("{client:?}"),
        format!("{token:?}"),
    ];
    for text in &rendered {
        assert!(!text.contains("PRIVATE KEY"), "PEM header in {text}");
        assert!(!text.contains(key_line), "key material in {text}");
        assert!(!text.contains(mock_token()), "token in {text}");
    }
    assert_eq!(token.secret(), mock_token());
}
