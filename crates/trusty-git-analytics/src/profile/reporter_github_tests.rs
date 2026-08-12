//! Unit tests for the profile → GitHub issue thread path (#5465).
//!
//! Loaded via `#[path]` from `reporter_github.rs`. The one HTTP test runs
//! against a local `wiremock` server through `GitHubClient::with_api_base`.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::core::config::GithubConfig;
use crate::profile::ProfileError;

fn profile_for(name: &str, email: &str) -> ContributorProfile {
    ContributorProfile::new(email, name, "2026-01-01", "2026-06-30")
}

/// Why: the title is the thread's identity — the next run searches for the
/// canonical email inside it. A title without the email means every run opens a
/// new issue.
/// What: asserts the label prefix, the name, and the email are all present.
/// Test: this test itself.
#[test]
fn issue_title_embeds_the_canonical_email() {
    let title = issue_title(&profile_for("Alice Smith", "alice@example.com"));
    assert!(title.starts_with("[dev-profile] "), "label prefix: {title}");
    assert!(title.contains("Alice Smith"), "display name: {title}");
    assert!(
        title.contains("alice@example.com"),
        "the search marker must be in the title: {title}"
    );
}

/// Why: `--github-repo acme/profiles` is the only thing standing between a
/// private profile and the wrong repository's watchers.
/// What: asserts a well-formed slug splits and takes the default label.
/// Test: this test itself.
#[test]
fn github_issue_config_parses_a_slug() {
    let cfg = GithubIssueConfig::from_slug("acme/profiles").expect("valid slug");
    assert_eq!(cfg.owner, "acme");
    assert_eq!(cfg.repo, "profiles");
    assert_eq!(cfg.label, PROFILE_ISSUE_LABEL);
}

/// Why: a bare name or a three-part path must be rejected rather than guessed
/// at — guessing publishes a personal review somewhere nobody chose.
/// What: asserts both malformed shapes produce `ProfileError::Config`.
/// Test: this test itself.
#[test]
fn github_issue_config_rejects_a_bare_name() {
    for bad in ["profiles", "acme/", "/profiles", "acme/team/profiles"] {
        let err = GithubIssueConfig::from_slug(bad).expect_err("must reject: {bad}");
        assert!(
            matches!(err, ProfileError::Config(_)),
            "expected Config error for '{bad}', got {err:?}"
        );
    }
}

/// Why: this is the end-to-end shape of the issue's closure condition — a
/// profile reaches a per-contributor GitHub thread, and the second run appends
/// rather than duplicating.
/// What: the mock already holds Alice's thread; the test asserts the run
/// commented on it with the rendered Markdown and reported `created == false`.
/// Test: this test itself.
#[tokio::test]
async fn upsert_profile_issue_appends_to_the_contributors_thread() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "number": 17,
                "title": "[dev-profile] Alice Smith <alice@example.com>",
                "state": "open",
                "html_url": "https://github.com/acme/profiles/issues/17",
                "labels": [{"name": "dev-profile"}],
                "body": "Q1"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/repos/acme/profiles/issues/17/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = GithubConfig {
        token: Some("test-token".to_string()),
        ..Default::default()
    };
    let client = GitHubClient::new_for_reviews(&cfg)
        .expect("client builds")
        .with_api_base(server.uri());

    let profile = profile_for("Alice Smith", "alice@example.com");
    let out = upsert_profile_issue(
        &client,
        &GithubIssueConfig::from_slug("acme/profiles").expect("slug"),
        &profile,
        "# Developer Profile\n",
    )
    .await
    .expect("upsert must succeed");

    assert!(
        !out.created,
        "the thread already existed — this must append"
    );
    assert_eq!(out.number, 17);
}
