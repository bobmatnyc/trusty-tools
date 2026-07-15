//! Unit tests for [`super::super::ExternalSourceResolver::warm_cache`]
//! (issue #2719 code-critic HIGH finding): dedupe must happen on the
//! extracted ticket KEY, not the raw commit message.

use std::collections::HashMap;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::ExternalSourceResolver;
use crate::classify::sources::{JiraFieldMappings, JiraSourceConfig, SourceConfig};

/// Why: this is the resolver-level counterpart of the pipeline integration
/// test — it proves the property directly against `warm_cache` rather than
/// through the whole classification pipeline, so a regression here is
/// pinpointed to the resolver instead of surfacing as a pipeline-level
/// failure.
/// What: two DISTINCT messages both referencing `PROJ-7`; a wiremock JIRA
/// mock permits exactly one fetch (`.expect(1)`); after `warm_cache`, a
/// direct `resolve` call on each message must return the same signal with no
/// further HTTP call (the mock would fail on drop otherwise).
/// Test: wiremock; no pipeline/DB involved.
#[tokio::test]
async fn warm_cache_dedupes_ticket_key_across_distinct_messages() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "key": "PROJ-7",
        "fields": { "issuetype": {"name": "Bug"}, "labels": [], "components": [] }
    });
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    unsafe { std::env::set_var("JIRA_TOKEN_WARM_TEST", "test-token") };

    let mut issue_type_map = HashMap::new();
    issue_type_map.insert("Bug".to_string(), "bug_fix".to_string());
    let jira = JiraSourceConfig {
        base_url: server.uri(),
        token_env: "JIRA_TOKEN_WARM_TEST".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings {
            issue_type: issue_type_map,
            labels: HashMap::new(),
            components: HashMap::new(),
        },
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(jira)])
        .with_jira_base_url(0, server.uri());

    let messages = vec![
        "PROJ-7 fix the crash".to_string(),
        "background info: PROJ-7 root cause".to_string(),
    ];
    resolver.warm_cache(&messages, 8).await;

    // Both messages must now resolve to the same signal, cache-only.
    let s1 = resolver.resolve(&messages[0]).await.expect("signal 1");
    let s2 = resolver.resolve(&messages[1]).await.expect("signal 2");
    assert_eq!(s1, s2);
    assert_eq!(s1.category, "bug_fix");

    unsafe { std::env::remove_var("JIRA_TOKEN_WARM_TEST") };
    // Dropping the server asserts `.expect(1)`.
    drop(server);
}

/// Why: `warm_cache` must be a safe no-op when nothing references any ticket
/// — it must not panic or attempt any HTTP call.
/// What: warm with messages that contain no JIRA keys; assert no panic and
/// `resolve` still returns `None`.
/// Test: no HTTP expected (no keys extracted → nothing to fetch).
#[tokio::test]
async fn warm_cache_is_noop_when_no_keys_present() {
    let config = JiraSourceConfig {
        base_url: "https://acme.atlassian.net".to_string(),
        token_env: "JIRA_TOKEN_WARM_NOOP".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings::default(),
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)]);
    let messages = vec!["chore: tidy up formatting".to_string()];
    resolver.warm_cache(&messages, 8).await;
    assert!(resolver.resolve(&messages[0]).await.is_none());
}
