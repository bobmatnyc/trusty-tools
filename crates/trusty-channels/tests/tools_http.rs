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
use trusty_channels::slack::api::error::SlackError;
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

// ── slack_get_reactions (issue #3614) ──────────────────────────────────────

#[tokio::test]
async fn get_reactions_returns_reaction_list() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "reactions.get",
        json!({
            "ok": true,
            "type": "message",
            "channel": "C1",
            "message": {
                "reactions": [
                    { "name": "thumbsup", "users": ["U1", "U2"], "count": 2 },
                    { "name": "eyes", "users": ["U3"], "count": 1 },
                ]
            }
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_get_reactions",
        json!({ "channel": "C1", "timestamp": "1700000000.000100" }),
    )
    .await
    .expect("get reactions should succeed");

    assert_eq!(out["channel"], "C1");
    assert_eq!(out["reactions"].as_array().unwrap().len(), 2);
    assert_eq!(out["reactions"][0]["name"], "thumbsup");
    assert_eq!(out["reactions"][0]["count"], 2);
}

#[tokio::test]
async fn get_reactions_missing_arg_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reactions.get"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(&client, "slack_get_reactions", json!({ "channel": "C1" }))
        .await
        .expect_err("missing timestamp should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

// ── slack_schedule_message (issue #3616) ───────────────────────────────────

#[tokio::test]
async fn schedule_message_returns_id() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "chat.scheduleMessage",
        json!({
            "ok": true,
            "channel": "C1",
            "scheduled_message_id": "Q1298393284",
            "post_at": 1700000100,
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_schedule_message",
        json!({ "channel": "C1", "text": "reminder", "post_at": 1700000100 }),
    )
    .await
    .expect("schedule should succeed");

    assert_eq!(out["ok"], true);
    assert_eq!(out["scheduled_message_id"], "Q1298393284");
    assert_eq!(out["post_at"], 1700000100);
}

#[tokio::test]
async fn schedule_message_missing_post_at_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.scheduleMessage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_schedule_message",
        json!({ "channel": "C1", "text": "reminder" }),
    )
    .await
    .expect_err("missing post_at should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

// ── slack_create_conversation / slack_list_channel_members (issue #3613) ──

#[tokio::test]
async fn create_conversation_returns_channel() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.create"))
        .and(body_partial_json(
            json!({ "name": "incident-42", "is_private": true }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channel": { "id": "C99", "name": "incident-42", "is_private": true },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_create_conversation",
        json!({ "name": "incident-42", "is_private": true }),
    )
    .await
    .expect("create conversation should succeed");

    assert_eq!(out["channel"]["id"], "C99");
    assert_eq!(out["channel"]["name"], "incident-42");
    assert_eq!(out["channel"]["is_private"], true);
}

#[tokio::test]
async fn create_conversation_missing_name_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.create"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(&client, "slack_create_conversation", json!({}))
        .await
        .expect_err("missing name should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

#[tokio::test]
async fn list_channel_members_returns_page_and_cursor() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "conversations.members",
        json!({
            "ok": true,
            "members": ["U1", "U2", "U3"],
            "response_metadata": { "next_cursor": "bmV4dA==" },
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_list_channel_members",
        json!({ "channel": "C1" }),
    )
    .await
    .expect("list members should succeed");

    assert_eq!(out["channel"], "C1");
    assert_eq!(out["count"], 3);
    assert_eq!(out["members"][0], "U1");
    assert_eq!(out["next_cursor"], "bmV4dA==");
}

#[tokio::test]
async fn list_channel_members_paginates_with_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/conversations.members"))
        .and(body_partial_json(
            json!({ "channel": "C1", "cursor": "cGFnZTI=" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "members": ["U4"],
            "response_metadata": { "next_cursor": "" },
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_list_channel_members",
        json!({ "channel": "C1", "cursor": "cGFnZTI=" }),
    )
    .await
    .expect("paginated list members should succeed");

    assert_eq!(out["count"], 1);
    assert!(out["next_cursor"].is_null());
    assert_eq!(out["has_more"], false);
}

// ── slack_create_canvas / slack_update_canvas / slack_read_canvas (#3612) ──

#[tokio::test]
async fn create_canvas_returns_id() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.create",
        json!({ "ok": true, "canvas_id": "F123" }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_create_canvas", json!({}))
        .await
        .expect("create canvas should succeed");

    assert_eq!(out["canvas_id"], "F123");
}

#[tokio::test]
async fn create_canvas_with_channel_and_markdown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.create"))
        .and(body_partial_json(json!({
            "title": "Runbook",
            "channel_id": "C1",
            "document_content": { "type": "markdown", "markdown": "# Runbook" },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "canvas_id": "F456",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_create_canvas",
        json!({ "title": "Runbook", "channel_id": "C1", "markdown": "# Runbook" }),
    )
    .await
    .expect("create canvas with content should succeed");

    assert_eq!(out["canvas_id"], "F456");
}

#[tokio::test]
async fn update_canvas_replaces_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F123",
            "changes": [
                { "operation": "replace", "document_content": { "type": "markdown", "markdown": "new content" } }
            ],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_update_canvas",
        json!({ "canvas_id": "F123", "markdown": "new content" }),
    )
    .await
    .expect("update canvas should succeed");

    assert_eq!(out["canvas_id"], "F123");
}

#[tokio::test]
async fn update_canvas_missing_markdown_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_update_canvas",
        json!({ "canvas_id": "F123" }),
    )
    .await
    .expect_err("missing markdown should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

#[tokio::test]
async fn read_canvas_downloads_and_escapes_content() {
    let server = MockServer::start().await;
    let download_url = format!("{}/files/F123/download", server.uri());
    mount_ok(
        &server,
        "files.info",
        json!({
            "ok": true,
            "file": {
                "id": "F123",
                "title": "<b>Runbook</b>",
                "url_private_download": download_url,
            }
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/files/F123/download"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<h1>Runbook</h1> & steps"))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_canvas", json!({ "canvas_id": "F123" }))
        .await
        .expect("read canvas should succeed");

    assert_eq!(out["canvas_id"], "F123");
    assert_eq!(out["title"], "&lt;b&gt;Runbook&lt;/b&gt;");
    assert_eq!(out["content"], "&lt;h1&gt;Runbook&lt;/h1&gt; &amp; steps");
}

#[tokio::test]
async fn read_canvas_without_download_url_returns_empty_content() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "files.info",
        json!({ "ok": true, "file": { "id": "F999", "title": "Empty" } }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_canvas", json!({ "canvas_id": "F999" }))
        .await
        .expect("read canvas with no download url should still succeed");

    assert_eq!(out["content"], "");
}

// ── slack_canvas_create / slack_canvas_lookup_sections (issue #3744 slice 1) ─

#[tokio::test]
async fn canvas_create_requires_markdown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.create"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_create",
        json!({ "title": "Runbook" }),
    )
    .await
    .expect_err("missing markdown should error before any network call");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

#[tokio::test]
async fn canvas_create_posts_document_content_and_channel() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.create"))
        .and(body_partial_json(json!({
            "title": "Runbook",
            "channel_id": "C1",
            "document_content": { "type": "markdown", "markdown": "# Runbook" },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "canvas_id": "F789",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_create",
        json!({ "title": "Runbook", "channel_id": "C1", "markdown": "# Runbook" }),
    )
    .await
    .expect("canvas_create with markdown should succeed");

    assert_eq!(out["canvas_id"], "F789");
}

#[tokio::test]
async fn canvas_create_surfaces_slack_api_error() {
    // Slack errors like `missing_scope` / `canvas_creation_failed` /
    // `canvas_disabled_user_team` / `free_teams_cannot_create_non_tabbed_canvases`
    // must surface through the existing SlackError::Api path unchanged.
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.create",
        json!({ "ok": false, "error": "free_teams_cannot_create_non_tabbed_canvases" }),
    )
    .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_create",
        json!({ "markdown": "# Runbook" }),
    )
    .await
    .expect_err("Slack ok:false must surface as an error");
    match err {
        ToolCallError::Slack(SlackError::Api(slug)) => {
            assert_eq!(slug, "free_teams_cannot_create_non_tabbed_canvases");
        }
        other => panic!("expected ToolCallError::Slack(SlackError::Api(_)), got {other:?}"),
    }
}

#[tokio::test]
async fn lookup_sections_posts_criteria_and_returns_ids() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.sections.lookup"))
        .and(body_partial_json(json!({
            "canvas_id": "F123",
            "criteria": { "section_types": ["h1", "any_header"], "contains_text": "status" },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "sections": [{ "id": "Sc001" }, { "id": "Sc002" }],
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_lookup_sections",
        json!({
            "canvas_id": "F123",
            "section_types": ["h1", "any_header"],
            "contains_text": "status",
        }),
    )
    .await
    .expect("lookup_sections should succeed");

    assert_eq!(out["canvas_id"], "F123");
    assert_eq!(out["section_ids"], json!(["Sc001", "Sc002"]));
    // Slack's raw `sections` envelope must never be forwarded — only the
    // allow-listed `section_ids` field is returned (code-critic finding 1 on
    // PR #3749: a raw passthrough could carry unescaped workspace-authored
    // text from an undocumented future response field).
    assert!(out.get("sections").is_none());
}

#[tokio::test]
async fn lookup_sections_omits_absent_criteria_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.sections.lookup"))
        .and(body_partial_json(
            json!({ "canvas_id": "F123", "criteria": {} }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "sections": [],
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_lookup_sections",
        json!({ "canvas_id": "F123" }),
    )
    .await
    .expect("lookup_sections with no filters should still succeed");

    assert_eq!(out["section_ids"], json!([]));
}

#[tokio::test]
async fn lookup_sections_requires_canvas_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.sections.lookup"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_lookup_sections",
        json!({ "section_types": ["h1"] }),
    )
    .await
    .expect_err("missing canvas_id should error before any network call");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

#[tokio::test]
async fn lookup_sections_surfaces_slack_api_error() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.sections.lookup",
        json!({ "ok": false, "error": "canvas_not_found" }),
    )
    .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_lookup_sections",
        json!({ "canvas_id": "F999" }),
    )
    .await
    .expect_err("Slack ok:false must surface as an error");
    match err {
        ToolCallError::Slack(SlackError::Api(slug)) => {
            assert_eq!(slug, "canvas_not_found");
        }
        other => panic!("expected ToolCallError::Slack(SlackError::Api(_)), got {other:?}"),
    }
}

// ── slack_read_file (issue #3615) ──────────────────────────────────────────

#[tokio::test]
async fn read_file_returns_text_content() {
    let server = MockServer::start().await;
    let download_url = format!("{}/files/F1/download", server.uri());
    mount_ok(
        &server,
        "files.info",
        json!({
            "ok": true,
            "file": {
                "id": "F1",
                "name": "notes.md",
                "mimetype": "text/markdown",
                "size": 42,
                "permalink": "https://x.slack.com/files/F1",
                "url_private_download": download_url,
            }
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/files/F1/download"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello & <world>"))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_file", json!({ "file": "F1" }))
        .await
        .expect("read file should succeed");

    assert_eq!(out["file"]["id"], "F1");
    assert_eq!(out["file"]["name"], "notes.md");
    assert_eq!(out["binary"], false);
    assert_eq!(out["content"], "hello &amp; &lt;world&gt;");
}

#[tokio::test]
async fn read_file_reports_binary_without_embedding_bytes() {
    let server = MockServer::start().await;
    let download_url = format!("{}/files/F2/download", server.uri());
    mount_ok(
        &server,
        "files.info",
        json!({
            "ok": true,
            "file": {
                "id": "F2",
                "name": "image.png",
                "mimetype": "image/png",
                "size": 4,
                "permalink": "https://x.slack.com/files/F2",
                "url_private_download": download_url,
            }
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/files/F2/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0xd8, 0xff, 0xe0]))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_read_file", json!({ "file": "F2" }))
        .await
        .expect("read binary file should still succeed");

    assert_eq!(out["binary"], true);
    assert!(out["content"].is_null());
    assert_eq!(out["file"]["name"], "image.png");
}

#[tokio::test]
async fn read_file_missing_arg_errors_before_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/files.info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(&client, "slack_read_file", json!({}))
        .await
        .expect_err("missing file should error");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

// ── slack_search_users / slack_search_emojis (issue #3617) ────────────────

#[tokio::test]
async fn search_users_filters_by_query() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "users.list",
        json!({
            "ok": true,
            "members": [
                { "id": "U1", "name": "alice", "profile": { "real_name": "Alice A" } },
                { "id": "U2", "name": "bob", "profile": { "real_name": "Bob B" } },
            ]
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_search_users", json!({ "query": "alice" }))
        .await
        .expect("search users should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["users"][0]["id"], "U1");
}

#[tokio::test]
async fn search_emojis_filters_by_name() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "emoji.list",
        json!({
            "ok": true,
            "emoji": {
                "partyparrot": "https://x.slack.com/emoji/partyparrot.png",
                "shipit": "alias:squirrel",
            }
        }),
    )
    .await;

    let client = client_for(&server);
    let out = dispatch(&client, "slack_search_emojis", json!({ "query": "party" }))
        .await
        .expect("search emojis should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["emoji"][0]["name"], "partyparrot");
    assert_eq!(out["emoji"][0]["is_alias"], false);
}

// ── slack_search_messages scope split (issue #3617) ────────────────────────

#[tokio::test]
async fn search_messages_scope_public_excludes_private_matches() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "search.messages",
        json!({
            "ok": true,
            "messages": {
                "matches": [
                    {
                        "channel": { "id": "C1", "name": "general", "is_private": false },
                        "user": "U1", "ts": "1.1", "text": "public msg", "permalink": "https://x/1"
                    },
                    {
                        "channel": { "id": "C2", "name": "leadership", "is_private": true },
                        "user": "U2", "ts": "2.2", "text": "private msg", "permalink": "https://x/2"
                    },
                ]
            }
        }),
    )
    .await;

    let client = client_with_user_for(&server);
    let out = dispatch(
        &client,
        "slack_search_messages",
        json!({ "query": "msg", "scope": "public" }),
    )
    .await
    .expect("scoped search should succeed");

    assert_eq!(out["count"], 1);
    assert_eq!(out["matches"][0]["channel_id"], "C1");
}

#[tokio::test]
async fn search_messages_default_scope_includes_private_matches() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "search.messages",
        json!({
            "ok": true,
            "messages": {
                "matches": [
                    {
                        "channel": { "id": "C2", "name": "leadership", "is_private": true },
                        "user": "U2", "ts": "2.2", "text": "private msg", "permalink": "https://x/2"
                    },
                ]
            }
        }),
    )
    .await;

    let client = client_with_user_for(&server);
    let out = dispatch(&client, "slack_search_messages", json!({ "query": "msg" }))
        .await
        .expect("default-scope search should succeed");

    // Unchanged pre-#3617 behaviour: no scope argument means no filtering.
    assert_eq!(out["count"], 1);
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
