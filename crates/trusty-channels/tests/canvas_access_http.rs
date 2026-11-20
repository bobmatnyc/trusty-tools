//! Hermetic tests for `slack_canvas_access_set` (#10890).
//!
//! Why: the tool hands out edit rights on a canvas, so its request shape and
//! its refusals (`owner`, DM ids, a missing or doubled target list) need
//! pinning.
//! What: each case points a `BaseClient` at a local `wiremock` server and
//! drives the public `dispatch` entry point. Refusal cases mount a route that
//! must receive zero calls.
//! Test: this file is the test.

use serde_json::{json, Value};
use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::api::error::SlackError;
use trusty_channels::slack::handlers::dispatch;
use trusty_channels::slack::server::ToolCallError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOOL: &str = "slack_canvas_access_set";

/// A client wired to the mock server with a fixed bot token.
fn client_for(server: &MockServer) -> BaseClient {
    BaseClient::with_endpoint(server.uri(), Some("xoxb-test".to_string()))
        .expect("construct client")
}

/// Expect exactly one `canvases.access.set` call carrying `expected` as its body.
async fn expect_body(server: &MockServer, expected: Value) {
    Mock::given(method("POST"))
        .and(path("/canvases.access.set"))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(server)
        .await;
}

/// Assert `args` is refused as invalid with no network call.
async fn assert_refused(args: Value) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(0)
        .mount(&server)
        .await;
    let err = dispatch(&client_for(&server), TOOL, args.clone())
        .await
        .expect_err("must be refused");
    assert!(
        matches!(err, ToolCallError::InvalidArgs(_)),
        "expected InvalidArgs for {args}, got {err:?}"
    );
}

#[tokio::test]
async fn access_set_posts_user_ids_and_level() {
    let server = MockServer::start().await;
    expect_body(
        &server,
        json!({ "canvas_id": "F1", "access_level": "write", "user_ids": ["U1", "U2"] }),
    )
    .await;
    let out = dispatch(
        &client_for(&server),
        TOOL,
        json!({ "canvas_id": "F1", "access_level": "write", "user_ids": ["U1", "U2"] }),
    )
    .await
    .expect("grant should succeed");
    assert_eq!(
        out,
        json!({ "ok": true, "canvas_id": "F1", "access_level": "write" })
    );
}

#[tokio::test]
async fn access_set_posts_channel_ids() {
    let server = MockServer::start().await;
    expect_body(
        &server,
        json!({ "canvas_id": "F1", "access_level": "read", "channel_ids": ["C1"] }),
    )
    .await;
    dispatch(
        &client_for(&server),
        TOOL,
        json!({ "canvas_id": "F1", "access_level": "read", "channel_ids": ["C1"] }),
    )
    .await
    .expect("channel grant should succeed");
}

#[tokio::test]
async fn access_set_rejects_owner_and_unknown_levels() {
    for level in ["owner", "admin", "WRITE", ""] {
        assert_refused(json!({ "canvas_id": "F1", "access_level": level, "user_ids": ["U1"] }))
            .await;
    }
    assert_refused(json!({ "canvas_id": "F1", "user_ids": ["U1"] })).await;
}

#[tokio::test]
async fn access_set_requires_exactly_one_target_list() {
    assert_refused(json!({ "canvas_id": "F1", "access_level": "write" })).await;
    assert_refused(json!({ "canvas_id": "F1", "access_level": "write", "user_ids": [] })).await;
    assert_refused(json!({
        "canvas_id": "F1", "access_level": "write",
        "user_ids": ["U1"], "channel_ids": ["C1"],
    }))
    .await;
    let too_many: Vec<String> = (0..21).map(|i| format!("U{i}")).collect();
    assert_refused(json!({ "canvas_id": "F1", "access_level": "write", "user_ids": too_many }))
        .await;
}

#[tokio::test]
async fn access_set_rejects_dm_channel_ids() {
    assert_refused(json!({
        "canvas_id": "F1", "access_level": "write", "channel_ids": ["C1", "D1"],
    }))
    .await;
}

#[tokio::test]
async fn access_set_maps_ok_false_to_slack_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.access.set"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "ok": false, "error": "restricted_action" })),
        )
        .mount(&server)
        .await;
    let err = dispatch(
        &client_for(&server),
        TOOL,
        json!({ "canvas_id": "F1", "access_level": "write", "user_ids": ["U1"] }),
    )
    .await
    .expect_err("ok:false must surface as an error");
    match err {
        ToolCallError::Slack(SlackError::Api(slug)) => {
            assert_eq!(slug, "restricted_action (from canvases.access.set)")
        }
        other => panic!("expected ToolCallError::Slack(SlackError::Api(_)), got {other:?}"),
    }
}
