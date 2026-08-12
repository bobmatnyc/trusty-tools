//! Unit tests for the GitHub issue write path (#5465).
//!
//! Loaded via `#[path]` from `issue_writer.rs`. Every HTTP test runs against a
//! local `wiremock` server reached through `GitHubClient::with_api_base` — no
//! test in this file can reach github.com, which is the only acceptable
//! arrangement for methods that create issues.

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::core::config::GithubConfig;

/// An authenticated client pointed at `base`.
fn client_at(base: &str) -> GitHubClient {
    let cfg = GithubConfig {
        token: Some("test-token".to_string()),
        ..Default::default()
    };
    GitHubClient::new_for_reviews(&cfg)
        .expect("client builds")
        .with_api_base(base)
}

/// A minimal `GitHubIssue`-shaped search hit.
fn issue_json(number: u64, title: &str) -> serde_json::Value {
    json!({
        "number": number,
        "title": title,
        "state": "open",
        "html_url": format!("https://github.com/acme/profiles/issues/{number}"),
        "labels": [{"name": "dev-profile"}],
        "body": "previous run"
    })
}

// ─── Pure helpers ─────────────────────────────────────────────────────────────

/// Why: an unscoped search matches the marker in every repository the token can
/// see, and the upsert would then comment on a stranger's issue.
/// What: asserts the query pins repo, label, title position, and issue type.
/// Test: this test itself.
#[test]
fn issue_search_query_scopes_to_repo_and_label() {
    let q = issue_search_query("acme", "profiles", "dev-profile", "alice@example.com");
    assert!(
        q.contains("repo:acme/profiles"),
        "must pin the repository: {q}"
    );
    assert!(q.contains("label:dev-profile"), "must pin the label: {q}");
    assert!(
        q.contains("in:title"),
        "marker is matched in the title: {q}"
    );
    assert!(
        q.contains("alice@example.com"),
        "marker must be present: {q}"
    );
    assert!(
        q.contains("type:issue"),
        "pull requests are not threads: {q}"
    );
}

/// Why: GitHub search is a text index and returns near misses; accepting one
/// would append Alice's profile to Alicia's thread.
/// What: offers two issues whose titles share tokens with the marker and
/// asserts only the literal substring match is selected.
/// Test: this test itself.
#[test]
fn find_thread_by_marker_ignores_a_near_miss() {
    let items: Vec<GitHubIssue> = vec![
        serde_json::from_value(issue_json(
            1,
            "[dev-profile] Alicia Stone <alicia@example.com>",
        ))
        .expect("parse"),
        serde_json::from_value(issue_json(
            2,
            "[dev-profile] Alice Smith <alice@example.com>",
        ))
        .expect("parse"),
    ];

    let hit = find_thread_by_marker(&items, "alice@example.com").expect("marker must match");
    assert_eq!(
        hit.number, 2,
        "the literal marker match wins, not the prefix"
    );

    assert!(
        find_thread_by_marker(&items, "bob@example.com").is_none(),
        "an absent marker must not fall back to the first result"
    );
}

// ─── Upsert over a mock GitHub ────────────────────────────────────────────────

/// Why: this is the closure condition of the write path — a second profile run
/// must APPEND to the contributor's thread. Opening a fresh issue each quarter
/// scatters the longitudinal record across four issues, which is precisely what
/// the thread exists to prevent.
/// What: the mock returns one matching issue from `/search/issues`, and the
/// test asserts the comment endpoint was hit with the report body while the
/// create endpoint was never called at all.
/// Test: this test itself.
#[tokio::test]
async fn upsert_comments_on_the_existing_thread_instead_of_opening_a_second() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(query_param("per_page", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [issue_json(42, "[dev-profile] Alice Smith <alice@example.com>")]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues/42/comments"))
        .and(body_json(json!({"body": "## profile run 2"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 7})))
        .expect(1)
        .mount(&server)
        .await;

    // No `create issue` mock is registered: reaching POST /repos/.../issues
    // returns 404 from wiremock and fails the assertions below.
    let out = client_at(&server.uri())
        .upsert_issue_thread(
            "acme",
            "profiles",
            "dev-profile",
            "[dev-profile] Alice Smith <alice@example.com>",
            "alice@example.com",
            "## profile run 2",
        )
        .await
        .expect("upsert must succeed against the existing thread");

    assert!(
        !out.created,
        "the second run must comment, not create — got a freshly created issue"
    );
    assert_eq!(out.number, 42, "must reuse the contributor's own thread");
    assert_eq!(out.html_url, "https://github.com/acme/profiles/issues/42");
}

/// Why: the first run for a contributor has nothing to append to, and the issue
/// it opens must carry the label the NEXT run searches by — otherwise every run
/// is a first run.
/// What: empty search results, then asserts the create body carries title, body,
/// and the label.
/// Test: this test itself.
#[tokio::test]
async fn upsert_creates_when_no_thread_exists() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues"))
        .and(body_json(json!({
            "title": "[dev-profile] Alice Smith <alice@example.com>",
            "body": "## profile run 1",
            "labels": ["dev-profile"]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(
            9,
            "[dev-profile] Alice Smith <alice@example.com>",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let out = client_at(&server.uri())
        .upsert_issue_thread(
            "acme",
            "profiles",
            "dev-profile",
            "[dev-profile] Alice Smith <alice@example.com>",
            "alice@example.com",
            "## profile run 1",
        )
        .await
        .expect("upsert must open the first thread");

    assert!(out.created, "no existing thread means this run opened one");
    assert_eq!(out.number, 9);
}

/// Why: a 403 on a write is almost always a token-scope problem, and
/// `error_for_status()` throws away the sentence that says so. Surfacing
/// GitHub's own message is what tells an operator whether their PAT is missing
/// `issues: write` rather than leaving them with a bare status code.
/// What: the mock denies the create with GitHub's real wording; the test
/// asserts the message survives into the error.
/// Test: this test itself.
#[tokio::test]
async fn a_denied_write_keeps_githubs_own_explanation() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "Resource not accessible by personal access token"
        })))
        .mount(&server)
        .await;

    let err = client_at(&server.uri())
        .create_issue("acme", "profiles", "t", "b", &["dev-profile".to_string()])
        .await
        .expect_err("a 403 must be an error");

    let rendered = err.to_string();
    assert!(
        rendered.contains("Resource not accessible by personal access token"),
        "GitHub's explanation must survive into the error: {rendered}"
    );
    assert!(
        rendered.contains("403"),
        "the status must be named: {rendered}"
    );
}

/// Why: a transient 5xx from search must not be read as "this contributor has
/// no thread" — that path would open a duplicate issue every time GitHub
/// hiccups, which is unrecoverable without manual cleanup.
/// What: search fails with 502; asserts the upsert errors and never posts.
/// Test: this test itself.
#[tokio::test]
async fn a_failed_search_aborts_rather_than_creating_a_duplicate() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(1, "should not happen")))
        .expect(0)
        .mount(&server)
        .await;

    let err = client_at(&server.uri())
        .upsert_issue_thread(
            "acme",
            "profiles",
            "dev-profile",
            "[dev-profile] Alice Smith <alice@example.com>",
            "alice@example.com",
            "body",
        )
        .await
        .expect_err("a failed search must abort the upsert");

    assert!(
        err.to_string().contains("502"),
        "the search failure must be reported as itself: {err}"
    );
}
