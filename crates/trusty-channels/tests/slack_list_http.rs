//! Why: #7391 exposed list parameters silently ignored when sent as POST JSON;
//! #9919 found the same defect on `files.info` and the other read methods.
//! What: exercise dispatch and the shared client against strict GET/query routes.
//! Test: this file covers list and read arguments, encoding, retries, auth
//! errors, and that a write method still POSTs JSON.

use serde_json::json;
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::api::error::SlackError;
use trusty_channels::slack::handlers::dispatch;
use trusty_channels::slack::server::ToolCallError;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some("xoxb-test".into())).unwrap()
}

#[tokio::test]
async fn list_channels_sends_limit_and_types_as_get_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(query_param("limit", "1"))
        .and(query_param("types", "public_channel,private_channel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C1", "name": "secret", "is_private": true}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let result = dispatch(
        &client_for(&server),
        "slack_list_channels",
        json!({"limit": 1, "types": "public_channel,private_channel"}),
    )
    .await
    .expect("list parameters must reach Slack's query parser");
    assert_eq!(result["count"], 1);
    assert_eq!(result["channels"][0]["is_private"], true);
    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].body.is_empty());
    assert_eq!(
        requests[0].url.query(),
        Some("limit=1&types=public_channel%2Cprivate_channel")
    );
}

#[tokio::test]
async fn list_users_sends_limit_as_get_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users.list"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "members": [{"id": "U1", "name": "alice"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let result = dispatch(
        &client_for(&server),
        "slack_list_users",
        json!({"limit": 1}),
    )
    .await
    .expect("user list limit must reach Slack's query parser");
    assert_eq!(result["count"], 1);
    assert_eq!(result["users"][0]["id"], "U1");
}

#[tokio::test]
async fn list_query_preserves_encoding_and_retry_parameters() {
    let server = MockServer::start().await;
    let cursor = "next+/=& page";
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(query_param("cursor", cursor))
        .and(query_param("exclude_archived", "true"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    client_for(&server)
        .call_method(
            "conversations.list",
            &json!({
                "cursor": cursor, "exclude_archived": true, "limit": 1
            }),
        )
        .await
        .expect("GET retry succeeds with the same arguments");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, requests[1].url);
    assert!(requests.iter().all(|request| request.body.is_empty()));
}

#[tokio::test]
async fn list_get_auth_errors_are_not_retried() {
    for response in [
        ResponseTemplate::new(401),
        ResponseTemplate::new(200).set_body_json(json!({"ok": false, "error": "invalid_auth"})),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/conversations.list"))
            .and(header("authorization", "Bearer xoxb-test"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = client_for(&server)
            .call_method("conversations.list", &json!({}))
            .await
            .expect_err("authentication failure stays typed");
        assert!(matches!(error, SlackError::Auth { .. }));
    }
}

#[tokio::test]
async fn list_query_rejects_nested_parameters_before_network() {
    let server = MockServer::start().await;
    let error = client_for(&server)
        .call_method("conversations.list", &json!({"limit": {"value": 1}}))
        .await
        .expect_err("query parameters must be scalar values");
    assert!(matches!(error, SlackError::Transport(ref error) if error.is_builder()));
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Mount a strict `GET /files.info?file=<id>` route plus a POST route that
/// answers the way live Slack does when the JSON body is ignored (#9919).
async fn mount_files_info_get_only(server: &MockServer, file_id: &str, file: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/files.info"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(query_param("file", file_id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "file": file})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/files.info"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": false, "error": "invalid_arguments"})),
        )
        .mount(server)
        .await;
}

/// Assert the one recorded `files.info` request was a body-less GET whose
/// query is exactly `file=<id>`.
async fn assert_files_info_get(server: &MockServer, file_id: &str) {
    let requests = server.received_requests().await.unwrap();
    let info: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/files.info")
        .collect();
    assert_eq!(info.len(), 1, "exactly one files.info call");
    assert_eq!(info[0].method.as_str(), "GET");
    assert!(info[0].body.is_empty(), "files.info must carry no body");
    assert_eq!(
        info[0].url.query(),
        Some(format!("file={file_id}").as_str())
    );
}

#[tokio::test]
async fn files_info_sends_file_as_get_query() {
    let server = MockServer::start().await;
    let download = format!("{}/files/F0C9LCB7ERG/download", server.uri());
    mount_files_info_get_only(
        &server,
        "F0C9LCB7ERG",
        json!({"id": "F0C9LCB7ERG", "name": "notes.md", "url_private_download": download}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/files/F0C9LCB7ERG/download"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&server)
        .await;
    let out = dispatch(
        &client_for(&server),
        "slack_read_file",
        json!({"file": "F0C9LCB7ERG"}),
    )
    .await
    .expect("read_file must succeed against the GET-only files.info route");
    assert_eq!(out["content"], "hello");
    assert_files_info_get(&server, "F0C9LCB7ERG").await;
}

#[tokio::test]
async fn read_canvas_sends_files_info_as_get_query() {
    let server = MockServer::start().await;
    let download = format!("{}/files/F0C9LCB7ERG/download", server.uri());
    mount_files_info_get_only(
        &server,
        "F0C9LCB7ERG",
        json!({"id": "F0C9LCB7ERG", "title": "Plan", "url_private_download": download}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/files/F0C9LCB7ERG/download"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>body</p>"))
        .mount(&server)
        .await;
    let out = dispatch(
        &client_for(&server),
        "slack_read_canvas",
        json!({"canvas_id": "F0C9LCB7ERG"}),
    )
    .await
    .expect("read_canvas must succeed against the GET-only files.info route");
    assert_eq!(out["title"], "Plan");
    assert_eq!(out["content"], "&lt;p&gt;body&lt;/p&gt;");
    assert_files_info_get(&server, "F0C9LCB7ERG").await;
}

#[tokio::test]
async fn files_info_error_names_the_method() {
    for (tool, args) in [
        ("slack_read_file", json!({"file": "F1"})),
        ("slack_read_canvas", json!({"canvas_id": "F1"})),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files.info"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": false, "error": "invalid_arguments"})),
            )
            .mount(&server)
            .await;
        let error = dispatch(&client_for(&server), tool, args)
            .await
            .expect_err("files.info ok:false must error");
        match error {
            ToolCallError::Slack(SlackError::Api(message)) => {
                assert_eq!(message, "invalid_arguments (from files.info)", "{tool}");
            }
            other => panic!("{tool}: expected SlackError::Api, got {other:?}"),
        }
    }
}

/// One read-method case: Slack method, request params, and the decoded query
/// pairs expected on the wire, in order.
type ReadCase = (
    &'static str,
    serde_json::Value,
    &'static [(&'static str, &'static str)],
);

#[tokio::test]
async fn read_methods_send_params_as_get_query() {
    let cases: [ReadCase; 8] = [
        ("files.info", json!({"file": "F1"}), &[("file", "F1")]),
        (
            "conversations.info",
            json!({"channel": "C1"}),
            &[("channel", "C1")],
        ),
        (
            "conversations.history",
            json!({"channel": "C1", "cursor": "a+b/= c", "inclusive": true, "limit": 5}),
            &[
                ("channel", "C1"),
                ("cursor", "a+b/= c"),
                ("inclusive", "true"),
                ("limit", "5"),
            ],
        ),
        (
            "conversations.replies",
            json!({"channel": "C1", "limit": 20, "ts": "1616703000.216300"}),
            &[
                ("channel", "C1"),
                ("limit", "20"),
                ("ts", "1616703000.216300"),
            ],
        ),
        (
            "conversations.members",
            json!({"channel": "C1", "cursor": "cGFnZTI=", "limit": 100}),
            &[("channel", "C1"), ("cursor", "cGFnZTI="), ("limit", "100")],
        ),
        ("users.info", json!({"user": "U1"}), &[("user", "U1")]),
        (
            "reactions.get",
            json!({"channel": "C1", "timestamp": "1.2"}),
            &[("channel", "C1"), ("timestamp", "1.2")],
        ),
        ("emoji.list", json!({}), &[]),
    ];
    for (slack_method, params, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{slack_method}")))
            .and(header("authorization", "Bearer xoxb-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;
        client_for(&server)
            .call_method(slack_method, &params)
            .await
            .unwrap_or_else(|e| panic!("{slack_method} must go out as GET: {e:?}"));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{slack_method}");
        assert!(
            requests[0].body.is_empty(),
            "{slack_method} carries no body"
        );
        let pairs: Vec<(String, String)> = requests[0].url.query_pairs().into_owned().collect();
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(pairs, expected, "{slack_method} query");
    }
}

#[tokio::test]
async fn write_method_still_posts_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(body_partial_json(json!({"channel": "C1", "text": "hi"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "ts": "1.1"})))
        .expect(1)
        .mount(&server)
        .await;
    client_for(&server)
        .call_method("chat.postMessage", &json!({"channel": "C1", "text": "hi"}))
        .await
        .expect("write methods keep the POST JSON transport");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(requests[0].url.query(), None);
}

/// The query pairs of one received request, in wire order.
fn query_pairs(request: &wiremock::Request) -> Vec<(String, String)> {
    request.url.query_pairs().into_owned().collect()
}

/// Owned `(key, value)` pairs for comparing against [`query_pairs`].
fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// A caller walks every channel the token can see: page 1's next cursor comes
// back in the tool result, and the follow-up call sends it to Slack.
#[tokio::test]
async fn list_channels_pages_with_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(query_param("cursor", "cGFnZTI="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C2", "name": "second", "is_private": true}],
            "response_metadata": {"next_cursor": ""}
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C1", "name": "first", "is_private": false}],
            "response_metadata": {"next_cursor": "cGFnZTI="}
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = client_for(&server);

    let first = dispatch(&client, "slack_list_channels", json!({"limit": 1}))
        .await
        .expect("page 1");
    assert_eq!(first["channels"][0]["id"], "C1");
    assert_eq!(first["next_cursor"], "cGFnZTI=");

    let cursor = first["next_cursor"].clone();
    let second = dispatch(
        &client,
        "slack_list_channels",
        json!({"limit": 1, "cursor": cursor}),
    )
    .await
    .expect("page 2");
    assert_eq!(second["channels"][0]["id"], "C2");
    assert!(second["next_cursor"].is_null(), "last page has no cursor");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.body.is_empty()));
    assert!(!query_pairs(&requests[0]).iter().any(|(k, _)| k == "cursor"));
    assert!(query_pairs(&requests[1]).contains(&("cursor".into(), "cGFnZTI=".into())));
}

// With no arguments the listing covers private channels the bot is in, not
// Slack's public-only default; an explicit `types` still goes through as is.
#[tokio::test]
async fn list_channels_default_types_include_private_and_explicit_types_win() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "G1", "name": "private-room", "is_private": true}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let client = client_for(&server);

    let out = dispatch(&client, "slack_list_channels", json!({}))
        .await
        .expect("default listing");
    assert_eq!(out["channels"][0]["is_private"], true);
    dispatch(&client, "slack_list_channels", json!({"types": "im"}))
        .await
        .expect("explicit types");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        query_pairs(&requests[0]),
        pairs(&[("types", "public_channel,private_channel")])
    );
    assert_eq!(query_pairs(&requests[1]), pairs(&[("types", "im")]));
}

/// One channel-read case: tool, Slack method, tool arguments, expected query.
type ToolReadCase = (
    &'static str,
    &'static str,
    serde_json::Value,
    &'static [(&'static str, &'static str)],
);

// #9931 moved these reads to GET + query after #9919 (`invalid_arguments` from
// a JSON POST). Pin the tool-level wire shape: `thread_ts` arrives as `ts`.
#[tokio::test]
async fn channel_reads_send_query_params_as_get() {
    let cases: [ToolReadCase; 2] = [
        (
            "slack_read_channel",
            "conversations.history",
            json!({"channel": "C1", "limit": 7, "cursor": "aGlzdA=="}),
            &[("channel", "C1"), ("limit", "7"), ("cursor", "aGlzdA==")],
        ),
        (
            "slack_read_thread",
            "conversations.replies",
            json!({
                "channel": "C1", "thread_ts": "1616703000.216300",
                "limit": 9, "cursor": "cmVwbA=="
            }),
            &[
                ("channel", "C1"),
                ("limit", "9"),
                ("ts", "1616703000.216300"),
                ("cursor", "cmVwbA=="),
            ],
        ),
    ];
    for (tool, slack_method, args, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{slack_method}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "messages": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        dispatch(&client_for(&server), tool, args)
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e:?}"));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{tool}");
        assert_eq!(requests[0].method.as_str(), "GET", "{tool}");
        assert!(requests[0].body.is_empty(), "{tool} carries no JSON body");
        let mut got = query_pairs(&requests[0]);
        let mut want = pairs(expected);
        got.sort();
        want.sort();
        assert_eq!(got, want, "{tool} query");
    }
}
