//! Hermetic tests for `slack_canvas_push` (issue #3744 slice 2).
//!
//! Why: split out of `tests/tools_http.rs` — adding this tool's request-
//! sequence, retry, and validation coverage pushed that file past the
//! workspace's 1500-SLOC test-file cap. `slack_canvas_push` is also the one
//! tool with a genuinely multi-request behaviour (`replace_all`'s
//! lookup-then-sequential-edits sequence, plus the `canvas_editing_locked`
//! retry loop), so it reads more clearly as its own focused file rather than
//! folded into the general canvas section of `tools_http.rs`.
//! What: each case points a `BaseClient` at a local `wiremock` server and
//! drives the handler through the public `dispatch` entry point, exactly like
//! `tools_http.rs`'s existing canvas tests — `append`'s single-call shape,
//! `replace_all`'s lookup → sequential-delete → insert ordering (asserted via
//! `MockServer::received_requests`), the no-header-sections fallback warning,
//! `ok:false` error mapping, the bounded `canvas_editing_locked` retry (both
//! the succeeds-after-one-retry and exhausts-the-budget paths), and the
//! argument-validation paths (invalid `mode`, missing required args, an
//! over-cap table) that must fail before any network call.
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

/// Mount a single POST route returning a 200 + JSON body.
async fn mount_ok(server: &MockServer, method_path: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(format!("/{method_path}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn canvas_push_append_single_edit_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/canvases.sections.lookup"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F1",
            "changes": [{ "operation": "insert_at_end" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "hello world", "mode": "append" }),
    )
    .await
    .expect("append push should succeed");

    assert_eq!(out["ok"], true);
    assert_eq!(out["canvas_id"], "F1");
    assert_eq!(out["operations_applied"], 1);
    assert_eq!(out["warnings"], json!([]));
}

#[tokio::test]
async fn canvas_push_replace_all_deletes_then_inserts_sequentially() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.sections.lookup",
        json!({ "ok": true, "sections": [{ "id": "S1" }, { "id": "S2" }] }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F1",
            "changes": [{ "operation": "delete", "section_id": "S1" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F1",
            "changes": [{ "operation": "delete", "section_id": "S2" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F1",
            "changes": [{ "operation": "insert_at_end" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "# New content", "mode": "replace_all" }),
    )
    .await
    .expect("replace_all push should succeed");

    assert_eq!(out["ok"], true);
    assert_eq!(out["operations_applied"], 3);
    assert_eq!(out["warnings"], json!([]));

    // Request-sequence assertion: lookup first, then the two deletes (in the
    // order sections.lookup returned them), then the final insert — never
    // batched into one call, and never re-ordered.
    let requests = server
        .received_requests()
        .await
        .expect("request recording enabled by default");
    let paths_and_ops: Vec<(String, Option<String>)> = requests
        .iter()
        .map(|r| {
            let path = r.url.path().to_string();
            let op = r
                .body_json::<serde_json::Value>()
                .ok()
                .and_then(|b| b["changes"][0]["operation"].as_str().map(str::to_string));
            (path, op)
        })
        .collect();
    assert_eq!(
        paths_and_ops,
        vec![
            ("/canvases.sections.lookup".to_string(), None),
            ("/canvases.edit".to_string(), Some("delete".to_string())),
            ("/canvases.edit".to_string(), Some("delete".to_string())),
            (
                "/canvases.edit".to_string(),
                Some("insert_at_end".to_string())
            ),
        ]
    );
}

#[tokio::test]
async fn canvas_push_replace_all_with_no_sections_only_inserts() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.sections.lookup",
        json!({ "ok": true, "sections": [] }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .and(body_partial_json(json!({
            "canvas_id": "F1",
            "changes": [{ "operation": "insert_at_end" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "content", "mode": "replace_all" }),
    )
    .await
    .expect("replace_all with no sections should still succeed");

    assert_eq!(out["operations_applied"], 1);
    let warnings = out["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0]
        .as_str()
        .unwrap()
        .contains("no header-delimited sections"));
}

#[tokio::test]
async fn canvas_push_surfaces_ok_false_as_slack_error() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        "canvases.edit",
        json!({ "ok": false, "error": "canvas_not_found" }),
    )
    .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "hi", "mode": "append" }),
    )
    .await
    .expect_err("ok:false must surface as an error");
    match err {
        ToolCallError::Slack(SlackError::Api(slug)) => assert_eq!(slug, "canvas_not_found"),
        other => panic!("expected ToolCallError::Slack(SlackError::Api(_)), got {other:?}"),
    }
}

#[tokio::test]
async fn canvas_push_retries_editing_locked_then_succeeds() {
    let server = MockServer::start().await;
    // Fallback success (mounted first → lower precedence).
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    // One editing_locked response (mounted last → matched first, once only).
    Mock::given(method("POST"))
        .and(path("/canvases.edit"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "ok": false, "error": "canvas_editing_locked" })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "hi", "mode": "append" }),
    )
    .await
    .expect("editing_locked should be retried transparently");
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn canvas_push_editing_locked_exhausts_retries() {
    let server = MockServer::start().await;
    // Persistent editing_locked: the bounded retry budget must eventually
    // give up rather than retry forever.
    mount_ok(
        &server,
        "canvases.edit",
        json!({ "ok": false, "error": "canvas_editing_locked" }),
    )
    .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "hi", "mode": "append" }),
    )
    .await
    .expect_err("persistent editing_locked must eventually error");
    match err {
        ToolCallError::Slack(SlackError::Api(slug)) => assert_eq!(slug, "canvas_editing_locked"),
        other => panic!("expected ToolCallError::Slack(SlackError::Api(_)), got {other:?}"),
    }
}

#[tokio::test]
async fn canvas_push_rejects_invalid_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": "hi", "mode": "sideways" }),
    )
    .await
    .expect_err("invalid mode must error before any network call");
    assert!(matches!(err, ToolCallError::InvalidArgs(_)));
}

#[tokio::test]
async fn canvas_push_requires_canvas_id_markdown_and_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let client = client_for(&server);

    for args in [
        json!({ "markdown": "hi", "mode": "append" }),
        json!({ "canvas_id": "F1", "mode": "append" }),
        json!({ "canvas_id": "F1", "markdown": "hi" }),
    ] {
        let err = dispatch(&client, "slack_canvas_push", args)
            .await
            .expect_err("missing required argument must error before any network call");
        assert!(matches!(err, ToolCallError::InvalidArgs(_)));
    }
}

#[tokio::test]
async fn canvas_push_table_over_cap_is_invalid_args() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut markdown = String::from("| a | b |\n| --- | --- |\n");
    for i in 0..151 {
        markdown.push_str(&format!("| r{i}c0 | r{i}c1 |\n"));
    }

    let client = client_for(&server);
    let err = dispatch(
        &client,
        "slack_canvas_push",
        json!({ "canvas_id": "F1", "markdown": markdown, "mode": "append" }),
    )
    .await
    .expect_err("over-cap table must error before any network call");
    match err {
        ToolCallError::InvalidArgs(msg) => assert!(msg.contains("300"), "{msg}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}
