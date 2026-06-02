//! Tests for the GitHub Issues context source.
//!
//! Why: extracted from `github_issues.rs` to keep that file under the 500-line
//! cap while preserving full coverage (query construction, parse + PR filtering,
//! the semantic-mode error path, and the fail-open token/transport seams).
//! What: query/parse unit tests plus fake-driven `gather` tests (no network).
//! Test: included as `#[cfg(test)] mod tests` from `github_issues.rs`.

use super::*;

struct FakeToken(Result<String, ()>);
#[async_trait]
impl IssueTokenResolver for FakeToken {
    async fn resolve(&self, _owner: &str) -> Result<String, ContextSourceError> {
        self.0
            .clone()
            .map_err(|_| ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: "no token".to_string(),
            })
    }
}

struct FakeSearch(Result<String, ()>);
#[async_trait]
impl IssueSearchTransport for FakeSearch {
    async fn search(&self, _t: &str, _q: &str, _n: u32) -> Result<String, ContextSourceError> {
        self.0.clone().map_err(|_| ContextSourceError::Api {
            src: SOURCE_NAME,
            status: 403,
            body: "rate limited".to_string(),
        })
    }
}

fn subject() -> ReviewSubject {
    ReviewSubject {
        owner: "acme".to_string(),
        repo: "backend".to_string(),
        title: "Fix login".to_string(),
        identifiers: vec!["login".to_string()],
        ..Default::default()
    }
}

#[test]
fn query_builds_search() {
    let q = GithubIssuesSource::build_query(&subject()).expect("signal");
    assert!(q.starts_with("repo:acme/backend is:issue "));
    assert!(q.contains("Fix login"));
}

#[test]
fn query_none_for_local_diff() {
    let subj = ReviewSubject {
        owner: "local".to_string(),
        repo: String::new(),
        title: "x".to_string(),
        ..Default::default()
    };
    assert!(GithubIssuesSource::build_query(&subj).is_none());
}

#[test]
fn parse_issues_to_section() {
    let body = r#"{
        "items": [
            {"number": 42, "title": "Login broken", "state": "open",
             "html_url": "https://github.com/acme/backend/issues/42"}
        ]
    }"#;
    let section = GithubIssuesSource::parse_section(body).unwrap();
    assert_eq!(section.heading, "Related GitHub issues");
    assert_eq!(section.snippets.len(), 1);
    assert_eq!(section.snippets[0].title, "#42 — Login broken");
    assert_eq!(section.snippets[0].subtitle.as_deref(), Some("open"));
    assert_eq!(
        section.snippets[0].link.as_deref(),
        Some("https://github.com/acme/backend/issues/42")
    );
}

#[test]
fn parse_filters_pull_requests() {
    let body = r#"{
        "items": [
            {"number": 1, "title": "real issue", "state": "open", "html_url": "u1"},
            {"number": 2, "title": "a PR", "state": "open", "html_url": "u2",
             "pull_request": {"url": "x"}}
        ]
    }"#;
    let section = GithubIssuesSource::parse_section(body).unwrap();
    // The PR item is dropped.
    assert_eq!(section.snippets.len(), 1);
    assert_eq!(section.snippets[0].title, "#1 — real issue");
}

#[test]
fn parse_error_on_garbage() {
    assert!(matches!(
        GithubIssuesSource::parse_section("nope"),
        Err(ContextSourceError::Parse { .. })
    ));
}

#[test]
fn from_config_respects_explicit_disable() {
    let cfg = super::super::SourceConfig {
        enabled: Some(false),
        mode: RetrievalMode::Live,
    };
    let src = GithubIssuesSource::from_config(&cfg, RunMode::Cli, ReviewConfig::load(None));
    assert!(!src.is_enabled());
}

#[tokio::test]
async fn disabled_without_token() {
    let src = GithubIssuesSource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Err(()))),
        Box::new(FakeSearch(Ok("{}".into()))),
    );
    let r = src.gather(&subject()).await;
    assert!(matches!(r, Err(ContextSourceError::NotConfigured { .. })));
}

#[tokio::test]
async fn semantic_mode_errors() {
    let src = GithubIssuesSource::new(
        true,
        RetrievalMode::Semantic,
        Box::new(FakeToken(Ok("t".into()))),
        Box::new(FakeSearch(Ok("{}".into()))),
    );
    let r = src.gather(&subject()).await;
    assert!(matches!(
        r,
        Err(ContextSourceError::SemanticNotImplemented {
            src: "github_issues"
        })
    ));
}

#[tokio::test]
async fn gather_with_fakes() {
    let body = r#"{"items":[{"number":7,"title":"bug","state":"closed","html_url":"u"}]}"#;
    let src = GithubIssuesSource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".into()))),
        Box::new(FakeSearch(Ok(body.to_string()))),
    );
    let section = src.gather(&subject()).await.expect("ok");
    assert_eq!(section.snippets.len(), 1);
    assert_eq!(section.snippets[0].title, "#7 — bug");
    assert_eq!(section.snippets[0].subtitle.as_deref(), Some("closed"));
}
