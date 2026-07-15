//! Hermetic tests for the send + read Slack tool handlers (issue #2639).
//!
//! Why: the send/read/list handlers must POST the correct Slack method, shape a
//! compact result, and — critically — markup-escape untrusted inbound text so a
//! hostile message cannot inject a `<!channel>` broadcast span into the
//! model-facing output. These behaviours need coverage that runs in CI with no
//! live Slack token.
//! What: each case points a `BaseClient` at a local `wiremock` server that
//! returns a canned Slack envelope, then drives the handler through the public
//! `dispatch` entry point and asserts the cleaned/escaped result. An `ok:false`
//! body and a missing-argument case pin the error mapping.
//! Test: this file is the test.

use serde_json::json;
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::handlers::dispatch;
use trusty_channels::slack::server::ToolCallError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A client wired to the mock server with a fixed token.
fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some("xoxb-test".to_string()))
        .expect("construct client")
}

/// Mount a single POST route returning a 200 + JSON body.
async fn mount_ok(server: &MockServer, method_path: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(format!("/{method_path}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn send_message_posts_and_returns_ts() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "chat.postMessage",
        json!({ "ok": true, "channel": "C123", "ts": "1700000000.000100" }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_send_message",
        json!({ "channel": "C123", "text": "hello world" }),
    )
    .await
    .expect("send should succeed");

    assert_eq!(out["ok"], true);
    assert_eq!(out["channel"], "C123");
    assert_eq!(out["ts"], "1700000000.000100");
}

#[tokio::test]
async fn read_channel_escapes_message_text() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.history",
        json!({
            "ok": true,
            "messages": [
                { "user": "U1", "ts": "1.1", "text": "<!channel> ping & <@U2>" },
                { "user": "U2", "ts": "2.2", "text": "normal message" },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_channel", json!({ "channel": "C1" }))
        .await
        .expect("read should succeed");

    assert_eq!(out["channel"], "C1");
    assert_eq!(out["count"], 2);
    // Hostile broadcast/mention markup must be neutralised.
    assert_eq!(
        out["messages"][0]["text"],
        "&lt;!channel&gt; ping &amp; &lt;@U2&gt;"
    );
    assert!(!out["messages"][0]["text"].as_str().unwrap().contains('<'));
    assert_eq!(out["messages"][1]["text"], "normal message");
    assert_eq!(out["messages"][0]["user"], "U1");
}

#[tokio::test]
async fn read_thread_returns_replies() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.replies",
        json!({
            "ok": true,
            "messages": [
                { "user": "U1", "ts": "1.1", "text": "parent" },
                { "user": "U2", "ts": "1.2", "text": "reply" },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_read_thread",
        json!({ "channel": "C1", "thread_ts": "1.1" }),
    )
    .await
    .expect("read thread should succeed");

    assert_eq!(out["thread_ts"], "1.1");
    assert_eq!(out["count"], 2);
    assert_eq!(out["messages"][1]["text"], "reply");
}

#[tokio::test]
async fn list_channels_returns_entries() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.list",
        json!({
            "ok": true,
            "channels": [
                { "id": "C1", "name": "general", "is_private": false },
                { "id": "C2", "name": "secret", "is_private": true },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_list_channels",
        json!({ "types": "public_channel,private_channel" }),
    )
    .await
    .expect("list channels should succeed");

    assert_eq!(out["count"], 2);
    assert_eq!(out["channels"][0]["id"], "C1");
    assert_eq!(out["channels"][0]["name"], "general");
    assert_eq!(out["channels"][1]["is_private"], true);
}

#[tokio::test]
async fn list_users_escapes_display_names() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "users.list",
        json!({
            "ok": true,
            "members": [
                { "id": "U1", "name": "alice", "real_name": "Alice <A>" },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_list_users", json!({}))
        .await
        .expect("list users should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["users"][0]["id"], "U1");
    assert_eq!(out["users"][0]["real_name"], "Alice &lt;A&gt;");
}

#[tokio::test]
async fn get_user_returns_profile() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "users.info",
        json!({
            "ok": true,
            "user": { "id": "U1", "name": "alice", "profile": { "real_name": "Alice & Co" } }
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_get_user", json!({ "user": "U1" }))
        .await
        .expect("get user should succeed");

    assert_eq!(out["user"]["id"], "U1");
    assert_eq!(out["user"]["real_name"], "Alice &amp; Co");
}

#[tokio::test]
async fn ok_false_maps_to_slack_error() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "chat.postMessage",
        json!({ "ok": false, "error": "channel_not_found" }),
    )
    .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_send_message",
        json!({ "channel": "nope", "text": "hi" }),
    )
    .await
    .expect_err("ok:false should error");
    assert!(matches!(err, ToolCallError::Slack(_)));
}

#[tokio::test]
async fn missing_required_arg_errors_before_network() {
    // A server that fails the test if it is ever hit — validation must happen first.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(&client, "slack_send_message", json!({ "channel": "C1" }))
        .await
        .expect_err("missing text should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}
