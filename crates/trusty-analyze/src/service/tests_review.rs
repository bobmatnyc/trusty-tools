//! Integration tests for review, deep analysis, synthesis helpers, and webhook handlers.
//!
//! Why: Split from `service/tests.rs` to keep both test files under the
//! 500-line cap. This file covers `POST /review`, `POST /review/github-pr`,
//! `POST /analyze/deep`, and `POST /webhooks/github`.
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

/// Secret injected through app state by every signed webhook test.
const TEST_WEBHOOK_SECRET: &str = "test-secret"; // pragma: allowlist secret

/// Build a `/webhooks/github` request carrying a valid `X-Hub-Signature-256`
/// for `TEST_WEBHOOK_SECRET`.
///
/// Why: since #5173 every delivery must authenticate, so the tests that assert
/// on downstream behaviour (event filtering, action filtering, payload
/// validation) have to get past the HMAC gate first.
/// What: HMAC-SHA256s `body` with `TEST_WEBHOOK_SECRET` and sets the header.
/// Test: this helper is exercised by `webhook_ignores_non_pr_event` and
/// friends; a mistake here makes them 401 instead of their expected status.
fn signed_webhook_request(event: &str, body: &str) -> Request<Body> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(TEST_WEBHOOK_SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    Request::builder()
        .method(Method::POST)
        .uri("/webhooks/github")
        .header("X-GitHub-Event", event)
        .header("X-Hub-Signature-256", sig)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn webhook_ignores_non_pr_event() {
    // A `push` event is acknowledged with 202 but triggers no analysis.
    let (state, _tmp) = make_state();
    let state = state.with_webhook_secret(Some(TEST_WEBHOOK_SECRET.to_string()));
    let app = build_router(state);
    let resp = app
        .oneshot(signed_webhook_request("push", "{}"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn webhook_ignores_non_actionable_pr_action() {
    // A `pull_request` event with action `labeled` is acknowledged but does not
    // trigger a review. (`closed` is covered by `webhook_accepts_valid_signature`.)
    let (state, _tmp) = make_state();
    let state = state.with_webhook_secret(Some(TEST_WEBHOOK_SECRET.to_string()));
    let app = build_router(state);
    let body = serde_json::json!({ "action": "labeled" }).to_string();
    let resp = app
        .oneshot(signed_webhook_request("pull_request", &body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn webhook_rejects_when_no_secret_configured() {
    // #5173 regression: with no secret configured the handler used to log a
    // warning and process the payload. An actionable, well-formed
    // `pull_request` delivery must now be rejected with 401 before any PR
    // coordinates reach the pipeline.
    let (state, _tmp) = make_state();
    assert!(
        state.webhook_secret.is_none(),
        "make_state must not preconfigure a webhook secret"
    );
    // The handler falls back to the env var, so an ambient value would send this
    // request down the signature-verification arm instead of the unset-secret
    // arm under test. Fail on the config, not on a confusing message mismatch.
    assert!(
        std::env::var_os("GITHUB_WEBHOOK_SECRET").is_none(),
        "this test requires GITHUB_WEBHOOK_SECRET to be unset in the environment"
    );
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
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Assert on the message too: a 401 from signature mismatch would mean an
    // ambient `GITHUB_WEBHOOK_SECRET` leaked in and the unset-secret arm was
    // never exercised.
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "webhook secret not configured");
}

#[tokio::test]
async fn webhook_rejects_bad_signature() {
    // With a secret injected via app state, a wrong signature must 401.
    // Injecting through state (not env) keeps the test hermetic and
    // free of cross-test env-var races.
    let (state, _tmp) = make_state();
    let state = state.with_webhook_secret(Some(TEST_WEBHOOK_SECRET.to_string()));
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/webhooks/github")
                .header("X-GitHub-Event", "pull_request")
                .header("X-Hub-Signature-256", "sha256=deadbeef")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_accepts_valid_signature() {
    // With a secret injected and a correctly-computed signature, the
    // request passes verification.
    let (state, _tmp) = make_state();
    let state = state.with_webhook_secret(Some(TEST_WEBHOOK_SECRET.to_string()));
    let app = build_router(state);
    let body = serde_json::json!({ "action": "closed" }).to_string();
    let resp = app
        .oneshot(signed_webhook_request("pull_request", &body))
        .await
        .unwrap();
    // Valid signature → past auth; `closed` action → 202 (ignored).
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn webhook_rejects_malformed_pr_payload() {
    // pull_request + opened, but no PR number / repo → 400 (authenticated).
    let (state, _tmp) = make_state();
    let state = state.with_webhook_secret(Some(TEST_WEBHOOK_SECRET.to_string()));
    let app = build_router(state);
    let body = serde_json::json!({ "action": "opened" }).to_string();
    let resp = app
        .oneshot(signed_webhook_request("pull_request", &body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
