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
fn parse_embeds_body() {
    // Fix 2 (#599): the issue body is trimmed, truncated, and embedded.
    let body = r#"{
        "items": [
            {"number": 5, "title": "Login bug", "state": "open", "html_url": "u",
             "body": "  The login form rejects valid passwords.  "}
        ]
    }"#;
    let section = GithubIssuesSource::parse_section(body).unwrap();
    assert_eq!(
        section.snippets[0].body.as_deref(),
        Some("The login form rejects valid passwords.")
    );
}

#[test]
fn parse_truncates_long_body() {
    let long = "x".repeat(SNIPPET_BODY_CHARS + 100);
    let body = format!(
        r#"{{"items":[{{"number":1,"title":"t","state":"open","html_url":"u","body":"{long}"}}]}}"#
    );
    let section = GithubIssuesSource::parse_section(&body).unwrap();
    assert_eq!(
        section.snippets[0].body.as_deref().unwrap().chars().count(),
        SNIPPET_BODY_CHARS
    );
}

#[test]
fn parse_no_body_when_empty() {
    // An empty / whitespace-only body yields no snippet body.
    let body = r#"{"items":[{"number":1,"title":"t","state":"open","html_url":"u","body":"   "}]}"#;
    let section = GithubIssuesSource::parse_section(body).unwrap();
    assert!(section.snippets[0].body.is_none());
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

// ─── cap_query / build_query truncation tests (#675) ─────────────────────────

#[test]
fn query_short_unchanged() {
    // A query already within the 256-char limit must pass through unmodified.
    let q = cap_query("repo:acme/backend is:issue fix login");
    assert_eq!(q, "repo:acme/backend is:issue fix login");
    assert!(q.chars().count() <= GITHUB_QUERY_MAX_CHARS);
}

#[test]
fn query_capped_at_256_chars() {
    // A query longer than 256 chars must be truncated to at most 256 chars.
    let long_keywords = "word ".repeat(60); // 300 chars of keywords
    let subj = ReviewSubject {
        owner: "acme".to_string(),
        repo: "backend".to_string(),
        title: long_keywords.trim().to_string(),
        ..Default::default()
    };
    let q = GithubIssuesSource::build_query(&subj).expect("signal");
    assert!(
        q.chars().count() <= GITHUB_QUERY_MAX_CHARS,
        "query was {} chars (>256): {:?}",
        q.chars().count(),
        q
    );
}

#[test]
fn query_capped_at_word_boundary() {
    // The truncation must not split mid-word: the result must not end with a
    // partial token (i.e. the last char must be a non-space complete word, or
    // the cut landed exactly on a space which is stripped).
    // Build a query that is just over 256 chars with word-aligned tokens so we
    // can verify the boundary.
    let prefix = "repo:acme/backend is:issue "; // 26 chars
    let filler = "abcde ".repeat(40); // 240 chars of 6-char "word " tokens
    let full = format!("{prefix}{filler}extra");
    assert!(
        full.chars().count() > GITHUB_QUERY_MAX_CHARS,
        "test precondition: full query must exceed 256 chars"
    );
    let capped = cap_query(&full);
    assert!(
        capped.chars().count() <= GITHUB_QUERY_MAX_CHARS,
        "capped query too long: {} chars",
        capped.chars().count()
    );
    // Must not end with a space (the whitespace boundary is the trim point).
    assert!(
        !capped.ends_with(' '),
        "capped query must not end with a space: {:?}",
        capped
    );
}

#[test]
fn build_query_long_body_stays_under_256() {
    // Exercises the full path: a subject whose keyword_query output would
    // produce a >256-char assembled query is still capped by build_query.
    let long_body = "important context word ".repeat(30); // 660 chars
    let subj = ReviewSubject {
        owner: "acme".to_string(),
        repo: "backend".to_string(),
        title: "Fix authentication flow".to_string(),
        body: long_body,
        identifiers: vec![
            "authenticate".to_string(),
            "TokenStore".to_string(),
            "refresh_token".to_string(),
            "validate_session".to_string(),
        ],
        ..Default::default()
    };
    let q = GithubIssuesSource::build_query(&subj).expect("signal");
    assert!(
        q.chars().count() <= GITHUB_QUERY_MAX_CHARS,
        "build_query returned {} chars (>256): {:?}",
        q.chars().count(),
        q
    );
    // Must still start with the required qualifiers.
    assert!(
        q.starts_with("repo:acme/backend is:issue "),
        "qualifiers stripped: {:?}",
        q
    );
}
