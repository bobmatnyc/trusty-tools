//! Hermetic tests for the Google Chat API layer (#9448 S1).
//!
//! Why: the layer handles a service-account private key and bearer tokens,
//! so the key-file mode rule, the token cache, the assertion claims and the
//! redaction of every credential type each need a test a wrong
//! implementation fails.
//! What: every case points `GchatClient` at a local wiremock server. The RSA
//! key is generated at run time with aws-lc-rs and written to a temp file, so
//! no PEM is committed and no test reaches Google.
//! Test: this file is the test.

// Key files are refused off Unix, where the mode cannot be checked.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine as _;
use serde_json::{json, Value};
use trusty_channels::gchat::api::auth::{Clock, ServiceAccountKey};
use trusty_channels::gchat::api::client::{
    CreateMessage, Endpoints, GchatClient, MessageReplyOption,
};
use trusty_channels::gchat::api::constants::{
    JWT_BEARER_GRANT_TYPE, SCOPE_CHAT_BOT, SCOPE_PUBSUB, TOKEN_AUDIENCE, TOKEN_REFRESH_MARGIN_SECS,
};
use trusty_channels::gchat::api::error::{EventParseError, GchatError};
use trusty_channels::gchat::api::events::ChatEvent;
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SA_EMAIL: &str = "bot@test-project.iam.gserviceaccount.com";
const TOKEN: &str = "ya29.test-access-token-0123456789";
const SUBSCRIPTION: &str = "projects/test-project/subscriptions/chat-in";

/// A throwaway RSA-2048 key: (PKCS#8 private PEM, SPKI public PEM).
fn throwaway_key() -> &'static (String, String) {
    static KEY: OnceLock<(String, String)> = OnceLock::new();
    KEY.get_or_init(|| {
        let pair = KeyPair::generate(KeySize::Rsa2048).expect("generate RSA key");
        let private = pair.as_der().expect("PKCS#8 DER");
        let public = pair.public_key().as_der().expect("SPKI DER");
        (
            pem("PRIVATE KEY", private.as_ref()),
            pem("PUBLIC KEY", public.as_ref()),
        )
    })
}

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let body: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        body.join("\n")
    )
}

/// Write a Google-shaped JSON key file with permission bits `mode`.
fn write_key_file(dir: &Path, mode: u32) -> PathBuf {
    let path = dir.join("sa.json");
    let key = json!({
        "type": "service_account",
        "project_id": "test-project",
        "private_key_id": "kid-1",
        "private_key": throwaway_key().0,
        "client_email": SA_EMAIL,
        "token_uri": "https://oauth2.googleapis.com/token",
    });
    std::fs::write(&path, key.to_string()).expect("write key file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

async fn mount_token(server: &MockServer, expires_in: u64, expect: u64) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": TOKEN, "expires_in": expires_in, "token_type": "Bearer"
        })))
        .expect(expect)
        .mount(server)
        .await;
}

fn client(server: &MockServer, dir: &Path) -> GchatClient {
    let key = write_key_file(dir, 0o600);
    GchatClient::with_endpoints(&key, Endpoints::single_host(&server.uri())).expect("client")
}

async fn token_requests(server: &MockServer) -> Vec<wiremock::Request> {
    let all = server.received_requests().await.expect("recording on");
    all.into_iter()
        .filter(|r| r.url.path() == "/token")
        .collect()
}

#[tokio::test]
async fn key_file_mode_0644_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let key = write_key_file(dir.path(), 0o644);

    let err = GchatClient::with_endpoints(&key, Endpoints::single_host(&server.uri()))
        .expect_err("a 0644 key file must be refused");
    assert!(
        matches!(err, GchatError::KeyFilePermissions { mode: 0o644, .. }),
        "{err:?}"
    );
    let requests = server.received_requests().await.expect("recording on");
    assert!(requests.is_empty(), "made {} request(s)", requests.len());
}

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
    mount_token(&server, 3600, 2).await;
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
    mount_token(&server, 3600, 1).await;
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
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant", "error_description": "Invalid JWT Signature."
        })))
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
}

#[tokio::test]
async fn create_message_sends_bearer_path_and_thread_key() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(query_param(
            "messageReplyOption",
            "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
        ))
        .and(query_param("requestId", "req-1"))
        .and(body_partial_json(
            json!({"text": "Ship it?", "thread": {"threadKey": "q-42"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "spaces/AAAA/messages/M1",
            "text": "Ship it?",
            "thread": {"name": "spaces/AAAA/threads/T1", "threadKey": "q-42"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let sent = client(&server, dir.path())
        .create_message(&CreateMessage {
            space: "spaces/AAAA".into(),
            text: "Ship it?".into(),
            thread_key: Some("q-42".into()),
            reply_option: Some(MessageReplyOption::ReplyFallbackToNewThread),
            request_id: Some("req-1".into()),
        })
        .await
        .expect("create message");
    assert_eq!(sent.name, "spaces/AAAA/messages/M1");
    assert_eq!(
        sent.thread.map(|t| t.name).as_deref(),
        Some("spaces/AAAA/threads/T1")
    );
}

#[tokio::test]
async fn create_message_non_2xx_is_typed_and_never_contains_the_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    // A hostile body that echoes the bearer token back.
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED",
                      "message": format!("caller {TOKEN} lacks access")}
        })))
        .mount(&server)
        .await;
    let request = CreateMessage {
        space: "spaces/AAAA".into(),
        text: "hi".into(),
        ..CreateMessage::default()
    };
    let err = client(&server, dir.path())
        .create_message(&request)
        .await
        .expect_err("403 must be an error");
    assert!(
        matches!(
            err,
            GchatError::Http {
                api: "chat",
                status: 403,
                ..
            }
        ),
        "{err:?}"
    );
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(TOKEN), "token leaked: {text}");
        assert!(
            text.contains("lacks access"),
            "Google's message dropped: {text}"
        );
    }
}

fn b64(v: &Value) -> String {
    base64::engine::general_purpose::STANDARD.encode(v.to_string())
}

#[tokio::test]
async fn pull_decodes_events_and_isolates_malformed_messages() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    let event = json!({
        "type": "MESSAGE",
        "eventTime": "2026-10-08T12:00:00Z",
        "space": {"name": "spaces/AAAA", "spaceType": "SPACE", "type": "ROOM"},
        "message": {
            "name": "spaces/AAAA/messages/M2",
            "text": "@bot yes, ship it",
            "argumentText": " yes, ship it",
            "sender": {"name": "users/123", "displayName": "Ann",
                       "email": "ann@example.com", "type": "HUMAN"},
            "thread": {"name": "spaces/AAAA/threads/T1"}
        },
        "user": {"name": "users/123", "email": "ann@example.com"},
        "threadKey": "q-42"
    });
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:pull")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({"maxMessages": 10})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"receivedMessages": [
            {"ackId": "ack-good", "message": {"data": b64(&event), "messageId": "1"}},
            {"ackId": "ack-not-b64", "message": {"data": "%%% not base64 %%%", "messageId": "2"}},
            {"ackId": "ack-not-json", "message": {
                "data": base64::engine::general_purpose::STANDARD.encode("plain text"),
                "messageId": "3"}}
        ]})))
        .expect(1)
        .mount(&server)
        .await;

    let pulled = client(&server, dir.path())
        .pull(SUBSCRIPTION, 10)
        .await
        .expect("a malformed message must not fail the batch");
    let ack_ids: Vec<&str> = pulled.iter().map(|m| m.ack_id.as_str()).collect();
    assert_eq!(ack_ids, ["ack-good", "ack-not-b64", "ack-not-json"]);

    let msg = match &pulled[0].event {
        Ok(ChatEvent::Message(m)) => m,
        other => panic!("expected MESSAGE, got {other:?}"),
    };
    assert_eq!(msg.message_name, "spaces/AAAA/messages/M2");
    assert_eq!(msg.text, "@bot yes, ship it");
    assert_eq!(msg.sender.name, "users/123");
    assert_eq!(msg.sender.email.as_deref(), Some("ann@example.com"));
    assert_eq!(msg.space.name, "spaces/AAAA");
    assert_eq!(msg.space.space_type.as_deref(), Some("SPACE"));
    assert_eq!(msg.thread_name.as_deref(), Some("spaces/AAAA/threads/T1"));
    assert_eq!(msg.thread_key.as_deref(), Some("q-42"));

    assert_eq!(pulled[1].event, Err(EventParseError::Base64));
    assert!(
        matches!(pulled[2].event, Err(EventParseError::Json(_))),
        "{:?}",
        pulled[2].event
    );
}

#[tokio::test]
async fn acknowledge_sends_exactly_the_given_ack_ids() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:acknowledge")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({"ackIds": ["ack-1", "ack-3"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, dir.path());
    client
        .acknowledge(SUBSCRIPTION, &["ack-1".to_string(), "ack-3".to_string()])
        .await
        .expect("acknowledge");
    // An empty list is a no-op: the API rejects it, so no request is sent.
    client
        .acknowledge(SUBSCRIPTION, &[])
        .await
        .expect("empty ack");
}

#[tokio::test]
async fn debug_output_never_contains_key_material_or_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
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
        assert!(!text.contains(TOKEN), "token in {text}");
    }
    assert_eq!(token.secret(), TOKEN);
}
