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

/// A client wired to the mock server with a fixed bot token (no user token).
fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some("xoxb-test".to_string()))
        .expect("construct client")
}

/// A client wired to the mock server with both a bot and a user token.
fn client_with_user_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint_tokens(
        server.uri(),
        Some("xoxb-test".to_string()),
        Some("xoxp-test".to_string()),
    )
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
async fn search_messages_with_user_token_returns_matches() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "search.messages",
        json!({
            "ok": true,
            "messages": {
                "total": 1,
                "matches": [
                    {
                        "channel": { "id": "C1", "name": "general" },
                        "user": "U1",
                        "ts": "1700000000.000100",
                        "text": "<!channel> deploy & ship",
                        "permalink": "https://x.slack.com/archives/C1/p1700000000000100"
                    }
                ]
            }
        }),
    )
    .await;

    let client = client_with_user_for(&server);
    let out = dispatch(
        &client,
        "slack_search_messages",
        json!({ "query": "deploy", "count": 5 }),
    )
    .await
    .expect("search should succeed");

    assert_eq!(out["query"], "deploy");
    assert_eq!(out["count"], 1);
    assert_eq!(out["matches"][0]["channel_id"], "C1");
    assert_eq!(out["matches"][0]["channel_name"], "general");
    // Hostile broadcast/mention markup in result text must be neutralised.
    assert_eq!(
        out["matches"][0]["text"],
        "&lt;!channel&gt; deploy &amp; ship"
    );
    assert!(!out["matches"][0]["text"].as_str().unwrap().contains('<'));
    assert_eq!(
        out["matches"][0]["permalink"],
        "https://x.slack.com/archives/C1/p1700000000000100"
    );
}

#[tokio::test]
async fn search_messages_without_user_token_errors() {
    // A server that fails the test if it is ever hit — the missing user token
    // must be caught before any network call, and must NOT fall back to the bot
    // token that `client_for` provides.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search.messages"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server); // bot token only, no user token
    let err = dispatch(
        &client,
        "slack_search_messages",
        json!({ "query": "anything" }),
    )
    .await
    .expect_err("missing user token must error");
    match err {
        ToolCallError::Slack(inner) => {
            let msg = inner.to_string();
            assert!(
                msg.contains("user token required for search") && msg.contains("SLACK_USER_TOKEN"),
                "clear, actionable typed error, got: {msg}"
            );
        }
        other => panic!("expected a Slack MissingUserToken error, got {other:?}"),
    }
}

#[tokio::test]
async fn search_channels_filters_by_query() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.list",
        json!({
            "ok": true,
            "channels": [
                { "id": "C1", "name": "backend-alerts", "is_private": false },
                { "id": "C2", "name": "random", "topic": { "value": "ALERTS live here" } },
                { "id": "C3", "name": "general", "is_private": false },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_search_channels",
        json!({ "query": "alert" }),
    )
    .await
    .expect("search channels should succeed");

    assert_eq!(out["query"], "alert");
    // C1 by name, C2 by topic (case-insensitive); C3 excluded.
    assert_eq!(out["count"], 2);
    let ids: Vec<&str> = out["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"C1"));
    assert!(ids.contains(&"C2"));
    assert!(!ids.contains(&"C3"));
}

#[tokio::test]
async fn add_reaction_posts_and_confirms() {
    let server = MockServer::start().await;
    mount_ok(&server, "reactions.add", json!({ "ok": true })).await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_add_reaction",
        json!({ "channel": "C1", "timestamp": "1700000000.000100", "name": "thumbsup" }),
    )
    .await
    .expect("add reaction should succeed");

    assert_eq!(out["ok"], true);
    assert_eq!(out["channel"], "C1");
    assert_eq!(out["timestamp"], "1700000000.000100");
    assert_eq!(out["name"], "thumbsup");
}

#[tokio::test]
async fn add_reaction_missing_arg_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reactions.add"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_add_reaction",
        json!({ "channel": "C1", "timestamp": "1.1" }),
    )
    .await
    .expect_err("missing name should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
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
