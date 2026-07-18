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
use wiremock::matchers::{body_partial_json, method, path};
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
async fn read_channel_paginates_with_cursor() {
    // A caller walking full history passes `cursor`/`oldest`/`latest` through
    // to conversations.history and reads back `next_cursor`/`has_more` to
    // decide whether to fetch another page (issue #2996).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.history"))
        .and(body_partial_json(json!({
            "channel": "C1",
            "cursor": "dXNlcjpVMDYxTkZUVDI=",
            "oldest": "1600000000.000000",
            "latest": "1700000000.000000",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [ { "user": "U1", "ts": "1650000000.000100", "text": "older" } ],
            "has_more": true,
            "response_metadata": { "next_cursor": "bmV4dC1wYWdl" },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_read_channel",
        json!({
            "channel": "C1",
            "cursor": "dXNlcjpVMDYxTkZUVDI=",
            "oldest": "1600000000.000000",
            "latest": "1700000000.000000",
        }),
    )
    .await
    .expect("paginated read should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["next_cursor"], "bmV4dC1wYWdl");
    assert_eq!(out["has_more"], true);
}

#[tokio::test]
async fn read_channel_last_page_has_null_next_cursor() {
    // Slack signals "no more pages" with an empty-string next_cursor; the
    // handler must surface that as `null`, not an empty-string cursor a naive
    // caller might try to replay.
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.history",
        json!({
            "ok": true,
            "messages": [ { "user": "U1", "ts": "1.1", "text": "last" } ],
            "has_more": false,
            "response_metadata": { "next_cursor": "" },
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_channel", json!({ "channel": "C1" }))
        .await
        .expect("read should succeed");

    assert!(out["next_cursor"].is_null());
    assert_eq!(out["has_more"], false);
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
    // A single-page thread with no response_metadata still shapes a clean
    // null next_cursor / false has_more rather than erroring or defaulting.
    assert!(out["next_cursor"].is_null());
    assert_eq!(out["has_more"], false);
}

#[tokio::test]
async fn read_thread_paginates_with_cursor() {
    // A long thread walked page-by-page: `cursor` is forwarded to
    // conversations.replies and `next_cursor`/`has_more` come back so the
    // caller knows to fetch another page (issue #2996).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.replies"))
        .and(body_partial_json(json!({
            "channel": "C1",
            "ts": "1616703000.216300",
            "cursor": "dGhyZWFkLXBhZ2Uy",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [ { "user": "U3", "ts": "1616703100.000200", "text": "later reply" } ],
            "has_more": true,
            "response_metadata": { "next_cursor": "dGhyZWFkLXBhZ2Uz" },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_read_thread",
        json!({
            "channel": "C1",
            "thread_ts": "1616703000.216300",
            "cursor": "dGhyZWFkLXBhZ2Uy",
        }),
    )
    .await
    .expect("paginated thread read should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["next_cursor"], "dGhyZWFkLXBhZ2Uz");
    assert_eq!(out["has_more"], true);
}

#[tokio::test]
async fn read_thread_rejects_malformed_thread_ts() {
    // A `thread_ts` that lost precision (e.g. round-tripped through a float)
    // or is otherwise not `seconds.microseconds` fails fast with a clear
    // InvalidArgs message instead of reaching Slack and coming back with an
    // opaque `invalid_arguments` (issue #2996, item 2b).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.replies"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_read_thread",
        json!({ "channel": "C1", "thread_ts": "1616703000" }),
    )
    .await
    .expect_err("malformed thread_ts must error before any network call");
    match err {
        ToolCallError::InvalidArgs(msg) => {
            assert!(msg.contains("thread_ts"), "names the field: {msg}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn read_thread_rejects_malformed_oldest_or_latest() {
    // slack_read_thread now advertises + forwards oldest/latest (issue #2996
    // review); a malformed value must fail fast pre-network exactly like
    // thread_ts, never reach conversations.replies.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.replies"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);

    let err = dispatch(
        &client,
        "slack_read_thread",
        json!({
            "channel": "C1",
            "thread_ts": "1616703000.216300",
            "oldest": "1616703000",
        }),
    )
    .await
    .expect_err("malformed oldest must error before any network call");
    match err {
        ToolCallError::InvalidArgs(msg) => assert!(msg.contains("oldest"), "{msg}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }

    let err = dispatch(
        &client,
        "slack_read_thread",
        json!({
            "channel": "C1",
            "thread_ts": "1616703000.216300",
            "latest": "not-a-ts",
        }),
    )
    .await
    .expect_err("malformed latest must error before any network call");
    match err {
        ToolCallError::InvalidArgs(msg) => assert!(msg.contains("latest"), "{msg}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn read_channel_rejects_malformed_oldest_or_latest() {
    // Same pre-network validation applies to slack_read_channel, which shares
    // apply_pagination_args with slack_read_thread.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.history"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_read_channel",
        json!({ "channel": "C1", "oldest": "1616703000" }),
    )
    .await
    .expect_err("malformed oldest must error before any network call");
    match err {
        ToolCallError::InvalidArgs(msg) => assert!(msg.contains("oldest"), "{msg}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn read_thread_forwards_oldest_and_latest_to_conversations_replies() {
    // The tool schema advertises oldest/latest on read_thread; this pins that
    // the handler actually forwards them (not just accepts and drops them).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.replies"))
        .and(body_partial_json(json!({
            "channel": "C1",
            "ts": "1616703000.216300",
            "oldest": "1600000000.000000",
            "latest": "1700000000.000000",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [ { "user": "U1", "ts": "1650000000.000100", "text": "windowed reply" } ],
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_read_thread",
        json!({
            "channel": "C1",
            "thread_ts": "1616703000.216300",
            "oldest": "1600000000.000000",
            "latest": "1700000000.000000",
        }),
    )
    .await
    .expect("windowed thread read should succeed");

    assert_eq!(out["count"], 1);
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
