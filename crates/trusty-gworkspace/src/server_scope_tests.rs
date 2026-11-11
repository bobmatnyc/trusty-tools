//! Dispatch-level tests for the read-only profile refusal (#10746 ok-T20).
//!
//! Why: the guard is only useful if `handle_tool_call` consults it before any
//! request; a unit test of `scope_guard` alone cannot prove that.
//! What: a `wiremock` server stands in for Google and answers every request
//! with Google's real insufficient-scope 403. A write tool on a read-only
//! profile must come back with an error naming the missing scope and must
//! send nothing; a read tool, and a write tool on a full profile, must still
//! reach the server. Uses only APIs that predate the guard, so the
//! fail-before run compiles against the pre-guard commit.
//! Test: this file.

use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{AppState, handle_tool_call};
use crate::api::client::BaseClient;

/// Google's body for a token that lacks the needed scope. It names no scope.
const GOOGLE_403: &str = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT","domain":"googleapis.com"}]}}"#;

const READONLY: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/drive.readonly",
    "https://www.googleapis.com/auth/documents.readonly",
];

async fn google_403() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(403).set_body_string(GOOGLE_403))
        .mount(&server)
        .await;
    server
}

/// A state whose one default profile `ro` records `scopes`.
fn state(server: &MockServer, scopes: &[&str]) -> AppState {
    let client = BaseClient::for_test_with_token("ro")
        .with_loopback_root(&server.uri())
        .unwrap();
    let mut all = client.storage().load().unwrap();
    all.get_mut("ro").unwrap().token.scopes = scopes.iter().map(|s| s.to_string()).collect();
    client.storage().save(&all).unwrap();
    AppState {
        client: Arc::new(client),
    }
}

fn send_args() -> Value {
    json!({ "to": "x@example.com", "subject": "s", "body": "b" })
}

/// Why: the error arm. Before the guard, the call reached Google and came
/// back with a 403 body that names no scope.
/// What: `compose_email` on a read-only profile returns an error naming
/// `gmail.send` and the fake Google receives no request.
/// Test: this test.
#[tokio::test]
async fn write_tool_on_readonly_profile_fails_naming_the_scope_and_sends_nothing() {
    let server = google_403().await;
    let st = state(&server, READONLY);
    let out = handle_tool_call(&st, "compose_email", send_args()).await;
    let err = out["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("https://www.googleapis.com/auth/gmail.send"),
        "error must name the missing scope: {out}"
    );
    let sent = server.received_requests().await.unwrap_or_default();
    assert!(sent.is_empty(), "no request may reach Google: {sent:?}");
}

/// Why: the guard must not break reads on a read-only profile, nor writes on
/// a full or legacy profile.
/// What: each case reaches the fake Google (one request at least).
/// Test: this test.
#[tokio::test]
async fn allowed_calls_still_reach_google() {
    let full: Vec<&str> = crate::api::constants::OAUTH_SCOPES.to_vec();
    let cases: [(&[&str], &str, Value); 3] = [
        (
            READONLY,
            "search_gmail_messages",
            json!({ "query": "in:sent" }),
        ),
        (&full, "compose_email", send_args()),
        (&[], "compose_email", send_args()),
    ];
    for (scopes, tool, args) in cases {
        let server = google_403().await;
        let st = state(&server, scopes);
        let out = handle_tool_call(&st, tool, args).await;
        let sent = server.received_requests().await.unwrap_or_default();
        assert!(
            !sent.is_empty(),
            "{tool} with {scopes:?} sent nothing: {out}"
        );
    }
}
