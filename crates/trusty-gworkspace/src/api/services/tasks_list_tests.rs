//! `manage_tasks` action `list` request and response tests (#10709).
//!
//! Why: `list` sent a bare `GET lists/{id}/tasks`, so callers got the first
//! page only, with hidden and deleted tasks excluded. These tests pin that
//! each new param reaches the API only when supplied.
//! What: Drives `manage_tasks_at` against a `wiremock` server and reads the
//! exact query string each request carried.
//! Test: This file.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount a GET mock at `at` answering `body`, then run `args` through
/// `manage_tasks_at` and return the result plus every request's query string.
async fn run_list(at: &str, body: Value, args: Value) -> (Value, Vec<Option<String>>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let client = BaseClient::for_test_with_token("a");
    let out = manage_tasks_at(&client, args, &server.uri())
        .await
        .expect("list succeeds");
    let queries = server
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|r| r.url.query().map(String::from))
        .collect();
    (out, queries)
}

#[tokio::test]
async fn list_without_params_sends_no_query_10709() {
    let args = json!({ "action": "list", "tasklist_id": "L1" });
    let (_, queries) = run_list("/lists/L1/tasks", json!({ "items": [] }), args).await;
    // The pre-#10709 request: no query string at all.
    assert_eq!(queries, vec![None]);
}

#[tokio::test]
async fn list_sends_each_param_only_when_set_10709() {
    // (arg, value, expected query): each param alone must produce exactly its
    // own API param and nothing else.
    let cases = [
        ("show_hidden", json!(true), "showHidden=true"),
        ("show_hidden", json!(false), "showHidden=false"),
        ("show_deleted", json!(true), "showDeleted=true"),
        ("max_results", json!(50), "maxResults=50"),
        // The token is percent-encoded, so `+`, `/` and `=` survive the trip.
        ("page_token", json!("a+b/c="), "pageToken=a%2Bb%2Fc%3D"),
    ];
    for (arg, value, expected) in cases {
        let mut args = json!({ "action": "list" });
        args[arg] = value;
        let (_, queries) = run_list("/lists/@default/tasks", json!({}), args).await;
        assert_eq!(queries, vec![Some(expected.to_string())], "arg {arg}");
    }

    // All together, in the builder's fixed order.
    let args = json!({
        "action": "list",
        "show_hidden": true,
        "show_deleted": true,
        "max_results": 100,
        "page_token": "tok",
    });
    let (_, queries) = run_list("/lists/@default/tasks", json!({}), args).await;
    let all = "showHidden=true&showDeleted=true&maxResults=100&pageToken=tok";
    assert_eq!(queries, vec![Some(all.to_string())]);
}

#[tokio::test]
async fn list_rejects_mistyped_params_and_skips_absent_ones_10709() {
    // A present param of the wrong JSON type is an error naming the param and
    // the expected type, and no request reaches the API.
    let cases = [
        ("show_hidden", json!("true"), "'show_hidden' must be a JSON boolean"),
        ("show_hidden", json!("yes"), "'show_hidden' must be a JSON boolean"),
        ("show_deleted", json!(1), "'show_deleted' must be a JSON boolean"),
        ("max_results", json!("ten"), "'max_results' must be a positive integer"),
        ("max_results", json!(-5), "'max_results' must be a positive integer"),
        ("max_results", json!(2.5), "'max_results' must be a positive integer"),
        ("page_token", json!(42), "'page_token' must be a string"),
    ];
    for (arg, value, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let client = BaseClient::for_test_with_token("a");
        let mut args = json!({ "action": "list" });
        args[arg] = value.clone();
        let err = manage_tasks_at(&client, args, &server.uri())
            .await
            .expect_err(&format!("{arg}={value} must be rejected"));
        assert!(err.to_string().contains(expected), "{arg}={value}: {err}");
        // `expect(0)` is checked when the server drops; assert it here too.
        server.verify().await;
    }

    // Absent, JSON `null` and an empty `page_token` add nothing.
    let args = json!({
        "action": "list",
        "page_token": "",
        "show_hidden": null,
        "show_deleted": null,
        "max_results": null,
    });
    let (_, queries) = run_list("/lists/@default/tasks", json!({}), args).await;
    assert_eq!(queries, vec![None]);
}

#[tokio::test]
async fn list_returns_next_page_token_10709() {
    let page = json!({
        "kind": "tasks#tasks",
        "items": [{ "id": "t1", "title": "A" }],
        "nextPageToken": "next-1",
    });
    let args = json!({ "action": "list" });
    let (out, _) = run_list("/lists/@default/tasks", page, args).await;
    // The raw API fields stay; the snake_case alias is added beside them.
    assert_eq!(out["nextPageToken"], "next-1");
    assert_eq!(out["next_page_token"], "next-1");
    assert_eq!(out["items"][0]["id"], "t1");
    assert_eq!(out["kind"], "tasks#tasks");

    // The last page carries no token, and no alias is invented.
    let args = json!({ "action": "list" });
    let (out, _) = run_list("/lists/@default/tasks", json!({ "items": [] }), args).await;
    assert!(out.get("next_page_token").is_none(), "{out}");
}

#[tokio::test]
async fn other_actions_ignore_list_params_10709() {
    // `get` with every list param still sends a bare request: the crate's
    // convention is that an action ignores params it does not use.
    let args = json!({
        "action": "get",
        "task_id": "t1",
        "show_hidden": true,
        "show_deleted": true,
        "max_results": 5,
        "page_token": "tok",
    });
    let (_, queries) = run_list("/lists/@default/tasks/t1", json!({ "id": "t1" }), args).await;
    assert_eq!(queries, vec![None]);
}
