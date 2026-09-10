//! Why: #7391 exposed list parameters silently ignored when sent as POST JSON.
//! What: exercise dispatch and the shared client against strict GET/query routes.
//! Test: this file covers list arguments, encoding, retries, and auth errors.

use serde_json::json;
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::api::error::SlackError;
use trusty_channels::slack::handlers::dispatch;
use wiremock::matchers::{header, method, path, query_param};
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
