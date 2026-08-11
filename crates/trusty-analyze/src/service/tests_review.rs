//! Integration tests for review, deep analysis, and synthesis helpers.
//!
//! Why: Split from `service/tests.rs` to keep both test files under the
//! 500-line cap. This file covers `POST /review`, `POST /review/github-pr`,
//! and `POST /analyze/deep`, plus the regression test pinning that
//! `POST /webhooks/github` is gone (#5181).
//!
//! What: Each test boots the router with a stub `TrustySearchClient` pointing
//! at port 1 so any path through trusty-search returns 502, keeping tests
//! hermetic without a live daemon.
//!
//! Test: `cargo test -p trusty-analyze` runs all tests in this module.

use axum::body::Body;
use axum::http::StatusCode;
use axum::http::{Method, Request};
use tower::ServiceExt;

use crate::service::routes::build_router;
use crate::service::tests::make_state;

#[tokio::test]
async fn review_endpoint_requires_index_id() {
    // Why: review is backed by trusty-search and needs an index to query;
    // POSTing without ?index_id= must fail fast with 400 before any work.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let diff = "+++ b/src/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review")
                .header("content-type", "text/x-patch")
                .body(Body::from(diff))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn review_endpoint_surfaces_search_failure_as_502() {
    // With index_id supplied but the search daemon down (stub at port 1),
    // the chunk fetch fails and the endpoint reports 502 BAD_GATEWAY.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let diff = "+++ b/src/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review?index_id=my-idx")
                .header("content-type", "text/x-patch")
                .body(Body::from(diff))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn review_endpoint_rejects_malformed_diff() {
    // A malformed hunk header is caught during parse, before any search
    // call, so the endpoint returns 400 even though index_id is present.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let diff = "+++ b/x.rs\n@@ totally bogus @@\n+fn x() {}\n";
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review?index_id=my-idx")
                .header("content-type", "text/x-patch")
                .body(Body::from(diff))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deep_endpoint_requires_index_id() {
    // POST /analyze/deep with an empty `index_id` must 400 before any
    // network or LLM work.
    let (state, _tmp) = make_state();
    let state = state.with_api_key(Some("test-key".into()));
    let app = build_router(state);
    let body = serde_json::json!({ "index_id": "" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/analyze/deep")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deep_endpoint_requires_api_key() {
    // POST /analyze/deep with no API key configured must 400 — the daemon
    // can't run the LLM call without a key.
    let (state, _tmp) = make_state();
    let state = state.with_api_key(None);
    let app = build_router(state);
    let body = serde_json::json!({ "index_id": "my-idx" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/analyze/deep")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn synthesise_review_from_chunks_groups_by_file() {
    // Synthesis should produce one FileReview per distinct chunk.file,
    // with NewFile source and no spurious recommendations.
    use crate::core::review::ReviewSource;
    use crate::types::CodeChunk;
    let chunks = vec![
        CodeChunk {
            id: "a:1:5".into(),
            file: "src/a.rs".into(),
            start_line: 1,
            end_line: 5,
            content: "fn a() {}".into(),
            ..Default::default()
        },
        CodeChunk {
            id: "a:10:20".into(),
            file: "src/a.rs".into(),
            start_line: 10,
            end_line: 20,
            content: "fn aa() {}".into(),
            ..Default::default()
        },
        CodeChunk {
            id: "b:1:3".into(),
            file: "src/b.rs".into(),
            start_line: 1,
            end_line: 3,
            content: "fn b() {}".into(),
            ..Default::default()
        },
    ];
    let report = crate::service::handlers::deep::synthesise_review_from_chunks(&chunks);
    assert_eq!(report.files.len(), 2);
    let paths: Vec<&str> = report.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"src/a.rs"));
    assert!(paths.contains(&"src/b.rs"));
    for f in &report.files {
        assert_eq!(f.source, ReviewSource::NewFile);
        assert!(f.recommendations.is_empty());
    }
}

#[test]
fn synthesise_review_from_chunks_empty_corpus_is_grade_a() {
    let report = crate::service::handlers::deep::synthesise_review_from_chunks(&[]);
    assert!(report.files.is_empty());
    assert_eq!(report.overall_grade, crate::types::ComplexityGrade::A);
    assert_eq!(report.smell_count, 0);
}

#[test]
fn lookup_frameworks_reads_stored_facts() {
    // record_frameworks → lookup_frameworks round-trip: the deep handler
    // must be able to read back the framework names that registry.rs
    // recorded under the (`index_id`, `uses_framework`, ...) triple.
    use crate::core::facts::new_fact;
    let (state, _tmp) = make_state();
    for fw in ["React", "Next.js"] {
        let f = new_fact(
            "my-idx".to_string(),
            "uses_framework".to_string(),
            fw.to_string(),
            "my-idx".to_string(),
        );
        state.facts.upsert(f).unwrap();
    }
    let mut got = crate::service::handlers::deep::lookup_frameworks(&state, "my-idx");
    got.sort();
    assert_eq!(got, vec!["Next.js".to_string(), "React".to_string()]);
}

/// The retired webhook route answers 404, not 200-and-discard (#5181).
///
/// Why: this is the failure this deletion exists to avoid. A route removed by
/// gutting its handler — left registered, answering 202, doing nothing — would
/// look identical to a working webhook from GitHub's delivery log while every
/// delivery vanished. Asserting the handler function is gone proves nothing
/// about what the router does with the path, so this drives the real router
/// and asserts the real response.
///
/// What: sends the request GitHub actually sends (POST, `X-GitHub-Event`, no
/// `Origin`) at the retired path and requires 404. Before #5181 the same
/// request reached `github_webhook_handler` and returned 401 (unset secret) —
/// never 404 — so this test fails against the pre-fix state.
/// Test: this is the test.
#[tokio::test]
async fn retired_webhook_route_is_not_registered() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let body = serde_json::json!({
        "action": "opened",
        "pull_request": { "number": 7, "head": { "sha": "abc123" } },
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/webhooks/github")
                .header("X-GitHub-Event", "pull_request")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the retired webhook route must be unreachable, not silently accepting"
    );
}
