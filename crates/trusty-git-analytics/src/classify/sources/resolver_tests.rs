use std::collections::HashMap;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::sources::{
    GithubIssuesSourceConfig, JiraFieldMappings, JiraSourceConfig, SourceConfig,
};

/// Why: the resolver must work with no sources configured (e.g. no
/// `sources:` block in the rules file) and return `None` without
/// panicking.
/// What: assert `resolve` on an empty resolver returns `None`.
/// Test: no HTTP, pure unit.
#[tokio::test]
async fn resolver_builds_from_empty_sources() {
    let resolver = ExternalSourceResolver::new(&[]);
    assert!(resolver.resolve("feat: add login").await.is_none());
}

/// Why: commits with no ticket keys must not trigger any HTTP calls and
/// must return `None` cleanly.
/// What: configure a JIRA source and resolve a message without a ticket key.
/// Test: no HTTP (no keys → no fetch).
#[tokio::test]
async fn resolver_returns_none_for_messages_without_keys() {
    let config = JiraSourceConfig {
        base_url: "https://acme.atlassian.net".to_string(),
        token_env: "JIRA_API_TOKEN".to_string(),
        username: None,
        email_env: None,
        project_keys: vec!["PROJ".to_string()],
        field_mappings: JiraFieldMappings::default(),
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)]);
    let result = resolver.resolve("feat: add login flow").await;
    assert!(result.is_none(), "no keys → no signal");
}

/// Why: the resolver must correctly route JIRA keys to the JIRA source
/// and return the mapped category.
/// What: stand up a wiremock server that returns a JIRA `Bug` issue type,
/// configure a mapping `Bug → bug_fix`, and assert the signal comes back.
/// Test: requires `wiremock` dev-dep; mocks one JIRA HTTP call.
#[tokio::test]
async fn resolver_returns_jira_signal_for_bug_issue_type() {
    let server = MockServer::start().await;

    let body = serde_json::json!({
        "key": "PROJ-1234",
        "fields": {
            "issuetype": {"name": "Bug"},
            "labels": [],
            "components": []
        }
    });

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    // Set token env var for this test.
    unsafe { std::env::set_var("JIRA_API_TOKEN_TEST_BUG", "test-token") };

    let mut issue_type_map = HashMap::new();
    issue_type_map.insert("Bug".to_string(), "bug_fix".to_string());

    let config = JiraSourceConfig {
        base_url: server.uri(),
        token_env: "JIRA_API_TOKEN_TEST_BUG".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings {
            issue_type: issue_type_map,
            labels: HashMap::new(),
            components: HashMap::new(),
        },
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)])
        .with_jira_base_url(0, server.uri());

    let signal = resolver
        .resolve("PROJ-1234 fix null pointer")
        .await
        .expect("should have signal");
    assert_eq!(signal.category, "bug_fix");
    assert!(signal.source.contains("issue_type"));

    unsafe { std::env::remove_var("JIRA_API_TOKEN_TEST_BUG") };
}

/// Why: the cache must prevent duplicate HTTP calls for the same ticket
/// key on multiple commits.
/// What: mount a JIRA mock that expects exactly one call, then resolve
/// the same key twice. If the second call hits the server, the test fails
/// because wiremock will see 2 calls.
/// Test: wiremock with `expect(1)`.
#[tokio::test]
async fn resolver_caches_jira_result_across_calls() {
    let server = MockServer::start().await;

    let body = serde_json::json!({
        "key": "PROJ-99",
        "fields": {
            "issuetype": {"name": "Story"},
            "labels": [],
            "components": []
        }
    });

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-99"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        // Exactly one HTTP call allowed — second call must be cache hit.
        .expect(1)
        .mount(&server)
        .await;

    unsafe { std::env::set_var("JIRA_API_TOKEN_CACHE_TEST", "test-token") };

    let mut issue_type_map = HashMap::new();
    issue_type_map.insert("Story".to_string(), "new_feature".to_string());

    let config = JiraSourceConfig {
        base_url: server.uri(),
        token_env: "JIRA_API_TOKEN_CACHE_TEST".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings {
            issue_type: issue_type_map,
            labels: HashMap::new(),
            components: HashMap::new(),
        },
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)])
        .with_jira_base_url(0, server.uri());

    // First call — should fetch from mock.
    let s1 = resolver.resolve("PROJ-99 add widget").await;
    assert!(s1.is_some());

    // Second call — must use the cache (wiremock will fail if it sees a
    // second request).
    let s2 = resolver.resolve("PROJ-99 related commit").await;
    assert_eq!(s1, s2);

    unsafe { std::env::remove_var("JIRA_API_TOKEN_CACHE_TEST") };
}

/// Why: when the JIRA token env var is unset the resolver must return
/// `None` rather than panicking or making unauthenticated requests.
/// What: configure a JIRA source with a token env var that is definitely
/// not set, resolve a message with a matching key, assert `None`.
/// Test: no HTTP expected (token check happens before fetch).
#[tokio::test]
async fn resolver_skips_jira_when_token_unset() {
    // Guarantee the env var is absent for this test.
    unsafe { std::env::remove_var("JIRA_TOKEN_DEFINITELY_NOT_SET_XYZ") };

    let config = JiraSourceConfig {
        base_url: "https://acme.atlassian.net".to_string(),
        token_env: "JIRA_TOKEN_DEFINITELY_NOT_SET_XYZ".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings::default(),
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)]);
    let result = resolver.resolve("PROJ-1234 update").await;
    assert!(result.is_none(), "missing token must yield None, not panic");
}

/// Why: the GitHub Issues resolver must correctly map labels to categories
/// via wiremock.
/// What: stand up a GitHub mock returning a `bug`-labelled issue and
/// assert the resolver returns `bug_fix`.
/// Test: wiremock mock of GitHub Issues REST v3.
#[tokio::test]
async fn resolver_returns_github_signal_for_bug_label() {
    let server = MockServer::start().await;

    let body = serde_json::json!({
        "number": 42,
        "labels": [{"name": "bug"}, {"name": "help wanted"}]
    });

    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    unsafe { std::env::set_var("GITHUB_TOKEN_TEST_BUG", "test-token") };

    let mut label_map = HashMap::new();
    label_map.insert("bug".to_string(), "bug_fix".to_string());

    let config = GithubIssuesSourceConfig {
        repo: "acme/widgets".to_string(),
        token_env: "GITHUB_TOKEN_TEST_BUG".to_string(),
        label_mappings: label_map,
    };

    let resolver = ExternalSourceResolver::new(&[SourceConfig::GithubIssues(config)])
        .with_github_api_base(0, server.uri());

    let signal = resolver
        .resolve("fix: closes #42")
        .await
        .expect("should have signal");
    assert_eq!(signal.category, "bug_fix");
    assert!(signal.source.contains("bug"));

    unsafe { std::env::remove_var("GITHUB_TOKEN_TEST_BUG") };
}
