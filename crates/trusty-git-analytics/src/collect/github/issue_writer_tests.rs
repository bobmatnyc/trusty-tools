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

/// Why: one email can be a SUBSTRING of another — `jones@x.com` is contained in
/// `bob.jones@x.com` — and the two share every token, so a search for the
/// shorter one plausibly returns both. An unanchored `title.contains(marker)`
/// then appends Jones's performance profile onto Bob Jones's issue. The write
/// goes to a live tracker and re-running does not take it back.
/// What: builds exactly the critic's reproduction — Bob's title, the shorter
/// marker — and asserts the anchored match rejects it while still finding the
/// contributor's own thread.
/// Test: this test itself.
#[test]
fn find_thread_by_marker_rejects_an_email_that_contains_the_marker() {
    let bob: GitHubIssue =
        serde_json::from_value(issue_json(3, "[dev-profile] Bob Jones <bob.jones@x.com>"))
            .expect("parse");

    // The unanchored predicate the anchoring replaced would accept this.
    assert!(
        bob.title.contains("jones@x.com"),
        "precondition: the shorter email IS a substring of the longer one"
    );

    assert!(
        find_thread_by_marker(std::slice::from_ref(&bob), "jones@x.com").is_none(),
        "a marker that is merely a substring of another contributor's email must \
         NOT match — this would comment on the wrong person's thread"
    );
    assert!(
        find_thread_by_marker(std::slice::from_ref(&bob), "bob.jones@x.com").is_some(),
        "the contributor's own marker must still match"
    );

    // The same inclusion in the other direction: a longer marker must not match
    // the shorter title.
    let jones: GitHubIssue =
        serde_json::from_value(issue_json(4, "[dev-profile] Jones <jones@x.com>")).expect("parse");
    assert!(
        find_thread_by_marker(std::slice::from_ref(&jones), "bob.jones@x.com").is_none(),
        "an issue for the shorter email must not answer a query for the longer one"
    );
}

/// Why: the anchor is only correct if the title actually carries it — an issue
/// opened under a title without `<marker>` is invisible to the next run, so
/// every run would open another.
/// What: asserts `issue_search_query`'s companion anchor is the bracketed form.
/// Test: this test itself.
#[test]
fn thread_marker_anchor_is_bracketed() {
    assert_eq!(
        thread_marker_anchor("alice@example.com"),
        "<alice@example.com>"
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
        .and(query_param("per_page", "100"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1,
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
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"total_count": 0, "items": []})),
        )
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

/// Why: `in:title alice@example.com` is a TOKEN match, so it also matches every
/// colleague at `example.com`. In an org with hundreds of profiled contributors
/// the real thread sits past page one, and a lookup that read only the first
/// page would conclude "no thread" and open a duplicate on a live tracker.
/// What: page one returns a full page of other contributors' threads, page two
/// carries Alice's; asserts the walk reaches it and comments rather than
/// creating.
/// Test: this test itself.
#[tokio::test]
async fn upsert_walks_past_page_one_to_find_the_thread() {
    let server = MockServer::start().await;

    // A full page of colleagues at the same domain — every one a token match,
    // none the marker.
    let page_one: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            issue_json(
                1000 + i,
                &format!("[dev-profile] Dev {i} <dev{i}@example.com>"),
            )
        })
        .collect();

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 101, "items": page_one
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 101,
            "items": [issue_json(77, "[dev-profile] Alice Smith <alice@example.com>")]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues/77/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 5})))
        .expect(1)
        .mount(&server)
        .await;

    // No create mock: reaching POST /repos/acme/profiles/issues fails the run.
    let out = client_at(&server.uri())
        .upsert_issue_thread(
            "acme",
            "profiles",
            "dev-profile",
            "[dev-profile] Alice Smith <alice@example.com>",
            "alice@example.com",
            "## profile",
        )
        .await
        .expect("the walk must reach page two");

    assert!(
        !out.created,
        "the thread exists on page two — this must append"
    );
    assert_eq!(out.number, 77);
}

/// Why: GitHub caps search at 1000 results, so `total_count` can exceed what
/// paging will ever return. Reading that as "no thread exists" opens a duplicate
/// issue that no re-run undoes, so an unreadable remainder is an error.
/// What: a short page reporting far more matches than it returned; asserts the
/// upsert errors and never posts.
/// Test: this test itself.
#[tokio::test]
async fn upsert_refuses_to_create_when_the_search_could_not_be_exhausted() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 900,
            "items": [issue_json(1, "[dev-profile] Someone <someone@example.com>")]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(2, "should not happen")))
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
        .expect_err("an unexhausted search must not fall through to create");

    let rendered = err.to_string();
    assert!(
        rendered.contains("inconclusive") && rendered.contains("900"),
        "the error must say what was missed: {rendered}"
    );
}

/// Why: the next run finds this thread by searching for `<marker>` in the title.
/// A title without it produces an issue that is invisible to every later run, so
/// each one opens another — a duplicate generator rather than a thread.
/// What: passes a title missing the angle-bracketed marker; asserts the upsert
/// refuses before issuing any request.
/// Test: this test itself.
#[tokio::test]
async fn upsert_refuses_a_title_that_the_next_run_could_not_find() {
    let server = MockServer::start().await;
    // Nothing is mounted: any HTTP call at all fails this test.

    let err = client_at(&server.uri())
        .upsert_issue_thread(
            "acme",
            "profiles",
            "dev-profile",
            "[dev-profile] Alice Smith",
            "alice@example.com",
            "body",
        )
        .await
        .expect_err("an unanchored title must be refused");

    assert!(
        err.to_string().contains("<alice@example.com>"),
        "the error must name the anchor the title is missing: {err}"
    );
}
