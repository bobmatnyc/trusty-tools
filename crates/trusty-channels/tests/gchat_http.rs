//! Hermetic tests for the Google Chat API layer (#9448 S1).
//!
//! Why: the layer handles a service-account private key and bearer tokens,
//! so the key-file mode rule and the pull/acknowledge wire shapes each need
//! a test a wrong implementation fails.
//! What: every case points `GchatClient` at a local wiremock server. The RSA
//! key is generated at run time with aws-lc-rs and written to a temp file, so
//! no PEM is committed and no test reaches Google. The `create_message` and
//! token-source tests live in `src/gchat/tests/client_send.rs` and
//! `src/gchat/tests/auth.rs`, because those calls are crate-private so every
//! send and every `chat.bot` token sits behind the route check (ruling 7).
//! Test: this file is the test.

// Key files are refused off Unix, where the mode cannot be checked.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use base64::Engine as _;
use serde_json::{json, Value};
use trusty_channels::gchat::api::client::{Endpoints, GchatClient};
use trusty_channels::gchat::api::error::{EventParseError, GchatError};
use trusty_channels::gchat::api::events::ChatEvent;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SA_EMAIL: &str = "bot@test-project.iam.gserviceaccount.com";
const SUBSCRIPTION: &str = "projects/test-project/subscriptions/chat-in";

/// The mock access token, built at run time so no `ya29.` literal is
/// committed for a credential scanner to match (#9448 review).
fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| ["ya29", "test-access-token-0123456789"].join("."))
}

/// A throwaway RSA-2048 PKCS#8 private key PEM, generated once per run. The
/// armour lines are assembled at run time (#9448 review).
fn throwaway_key_pem() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let pair = KeyPair::generate(KeySize::Rsa2048).expect("generate RSA key");
        let der = pair.as_der().expect("PKCS#8 DER");
        let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
        let body: Vec<&str> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
            .collect();
        let line = |edge: &str| format!("-----{edge} {}-----", "PRIVATE KEY");
        format!("{}\n{}\n{}\n", line("BEGIN"), body.join("\n"), line("END"))
    })
}

/// Write a Google-shaped JSON key file with permission bits `mode`.
fn write_key_file(dir: &Path, mode: u32) -> PathBuf {
    let path = dir.join("sa.json");
    let key = json!({
        "type": "service_account",
        "project_id": "test-project",
        "private_key_id": "kid-1",
        "private_key": throwaway_key_pem(),
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
            "access_token": token(), "expires_in": expires_in, "token_type": "Bearer"
        })))
        .expect(expect)
        .mount(server)
        .await;
}

fn client(server: &MockServer, dir: &Path) -> GchatClient {
    let key = write_key_file(dir, 0o600);
    GchatClient::with_endpoints(&key, Endpoints::single_host(&server.uri())).expect("client")
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
        .and(header("authorization", format!("Bearer {}", token()).as_str()))
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
        .and(header(
            "authorization",
            format!("Bearer {}", token()).as_str(),
        ))
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
