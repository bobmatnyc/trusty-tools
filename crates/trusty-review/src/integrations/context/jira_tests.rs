//! Unit tests for `JiraSource` and related helpers.
//!
//! Why: split from `jira.rs` to keep that file under the 500-line cap
//! (issue #610).  All tests use fake transports — no network required.
//! What: covers JQL construction, ticket-key extraction, gather flow,
//! fail-open behaviour, and disabled-when-no-creds path.
//! Test: each function is a self-contained unit or async unit test.

use async_trait::async_trait;

use super::*;

fn creds() -> AtlassianCreds {
    AtlassianCreds {
        email: "bob@acme.com".to_string(),
        token: "tok".to_string(), // pragma: allowlist secret
        base_url: "https://acme.atlassian.net".to_string(),
    }
}

/// A `JiraTransport` returning a fixed body (or error) without network.
struct FakeJira {
    body: Result<String, ()>,
}

#[async_trait]
impl JiraTransport for FakeJira {
    async fn search_jql(
        &self,
        _creds: &AtlassianCreds,
        _jql: &str,
        _max: u32,
    ) -> Result<String, ContextSourceError> {
        self.body.clone().map_err(|_| ContextSourceError::Api {
            src: SOURCE_NAME,
            status: 500,
            body: "boom".to_string(),
        })
    }
}

fn subject() -> ReviewSubject {
    ReviewSubject {
        owner: "acme".to_string(),
        repo: "backend".to_string(),
        title: "Add token refresh".to_string(),
        identifiers: vec!["TokenStore".to_string()],
        ..Default::default()
    }
}

#[test]
fn query_builds_jql_keyword() {
    // No ticket key in title/body → keyword fallback path.
    let jql = JiraSource::build_jql(&subject()).expect("has signal");
    assert!(jql.contains("text ~ \"Add token refresh TokenStore\""));
    assert!(jql.contains("ORDER BY updated DESC"));
}

#[test]
fn query_builds_jql_ticket_ids() {
    // Fix 1: a ticket key in the title → exact issueKey lookup, NOT keyword.
    let subj = ReviewSubject {
        title: "PROJ-42 add token refresh".to_string(),
        identifiers: vec!["TokenStore".to_string()],
        ..Default::default()
    };
    let jql = JiraSource::build_jql(&subj).expect("has signal");
    assert_eq!(jql, "issueKey in (PROJ-42) ORDER BY updated DESC");
    assert!(!jql.contains("text ~"));
}

#[test]
fn query_ticket_ids_scan_body_too() {
    // A key only in the PR body is still found (title has no key).
    let subj = ReviewSubject {
        title: "Add token refresh".to_string(),
        body: "Implements PROJ-7 and PROJ-8.".to_string(),
        ..Default::default()
    };
    let jql = JiraSource::build_jql(&subj).expect("has signal");
    assert_eq!(jql, "issueKey in (PROJ-7, PROJ-8) ORDER BY updated DESC");
}

#[test]
fn query_ticket_ids_dedup_and_capped() {
    // Duplicates collapse; the list is capped at MAX_RESULTS keys.
    let ids: Vec<String> = (1..=10).map(|n| format!("PROJ-{n}")).collect();
    let subj = ReviewSubject {
        title: format!("{} PROJ-1", ids.join(" ")),
        ..Default::default()
    };
    let jql = JiraSource::build_jql(&subj).expect("has signal");
    // Exactly MAX_RESULTS (5) keys, comma-separated.
    let inner = jql
        .trim_start_matches("issueKey in (")
        .split(')')
        .next()
        .unwrap();
    assert_eq!(inner.split(", ").count(), MAX_RESULTS as usize);
}

#[test]
fn query_strips_quotes() {
    let subj = ReviewSubject {
        title: "Add \"quoted\" thing".to_string(),
        ..Default::default()
    };
    let jql = JiraSource::build_jql(&subj).unwrap();
    // No raw double-quotes inside the keyword payload would break the JQL.
    assert!(!jql.contains("\"quoted\""));
}

#[test]
fn query_none_without_signal() {
    let subj = ReviewSubject::default();
    assert!(JiraSource::build_jql(&subj).is_none());
}

#[tokio::test]
async fn disabled_when_no_creds() {
    // Forced-on but no creds → NotConfigured (fail-open at orchestrator).
    let src = JiraSource::new(
        true,
        RetrievalMode::Live,
        None,
        Box::new(FakeJira {
            body: Ok("{}".into()),
        }),
    );
    let r = src.gather(&subject()).await;
    assert!(matches!(r, Err(ContextSourceError::NotConfigured { .. })));
}

#[tokio::test]
async fn semantic_mode_errors() {
    let src = JiraSource::new(
        true,
        RetrievalMode::Semantic,
        Some(creds()),
        Box::new(FakeJira {
            body: Ok("{}".into()),
        }),
    );
    let r = src.gather(&subject()).await;
    assert!(matches!(
        r,
        Err(ContextSourceError::SemanticNotImplemented { src: "jira" })
    ));
}

#[tokio::test]
async fn gather_with_fake_transport() {
    let body =
        r#"{"issues":[{"key":"PROJ-7","fields":{"summary":"Fix","status":{"name":"Open"}}}]}"#;
    let src = JiraSource::new(
        true,
        RetrievalMode::Live,
        Some(creds()),
        Box::new(FakeJira {
            body: Ok(body.to_string()),
        }),
    );
    let section = src.gather(&subject()).await.expect("ok");
    assert_eq!(section.snippets.len(), 1);
    assert_eq!(section.snippets[0].title, "PROJ-7 — Fix");
}

#[tokio::test]
async fn gather_propagates_api_error_for_logging() {
    let src = JiraSource::new(
        true,
        RetrievalMode::Live,
        Some(creds()),
        Box::new(FakeJira { body: Err(()) }),
    );
    let r = src.gather(&subject()).await;
    assert!(matches!(
        r,
        Err(ContextSourceError::Api { status: 500, .. })
    ));
}
