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

// ─── cap_keywords / build_query truncation tests (#675) ─────────────────────────

#[test]
fn query_short_unchanged() {
    // #9503: cap_keywords takes the free text only; a short one passes through
    // unchanged apart from whitespace collapse.
    assert_eq!(cap_keywords("fix login"), "fix login");
    assert_eq!(cap_keywords("  fix\n\n login\t now "), "fix login now");
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
        q.chars().count() <= 256,
        "query was {} chars (>256): {:?}",
        q.chars().count(),
        q
    );
}

#[test]
fn query_capped_at_word_boundary() {
    // #9503: the cut lands on a term boundary (no partial token) and the free
    // text stays within GitHub's cost budget.  Was a raw-256-char cap on the
    // whole query, which let GitHub's 422 through.
    let filler = "abcde ".repeat(60);
    let capped = cap_keywords(&filler);
    assert!(
        free_text_cost(&capped) <= GITHUB_FREE_TEXT_BUDGET,
        "{capped:?}"
    );
    assert!(
        capped.split(' ').all(|t| t == "abcde"),
        "partial token: {capped:?}"
    );
    // A single giant token is hard-cut inside the budget.
    let giant = cap_keywords(&"x".repeat(400));
    assert!(free_text_cost(&giant) <= GITHUB_FREE_TEXT_BUDGET);
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
        q.chars().count() <= 256,
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

/// A search transport that counts its calls (#9194).
struct CountingSearch(std::sync::Arc<std::sync::atomic::AtomicUsize>);
#[async_trait]
impl IssueSearchTransport for CountingSearch {
    async fn search(&self, _t: &str, _q: &str, _n: u32) -> Result<String, ContextSourceError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(r#"{"items":[]}"#.to_string())
    }
}

/// GitHub search calls one `gather` over `subject` makes.
async fn search_calls(subject: &ReviewSubject) -> usize {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let source = GithubIssuesSource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("t".into()))),
        Box::new(CountingSearch(calls.clone())),
    );
    source
        .gather(subject)
        .await
        .expect("an empty section, not an error");
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

/// #9194 E5 (owner comment 2026-10-07): a local diff has no repository, so
/// `github_issues` makes no GitHub search call for the local owner.
#[tokio::test]
async fn github_issues_makes_no_search_call_for_the_local_owner() {
    let local = ReviewSubject {
        owner: crate::config::constants::LOCAL_OWNER.to_string(),
        repo: "stdin".to_string(),
        ..subject()
    };
    assert_eq!(search_calls(&local).await, 0);
}

/// #9194: a real owner still searches, once.
#[tokio::test]
async fn github_issues_still_searches_for_a_real_owner() {
    assert_eq!(search_calls(&subject()).await, 1);
}

// ─── free-text query cost (#9503) ────────────────────────────────────────────

/// GitHub's cost of the free-text part of `q`: its characters plus about 2 per
/// space-separated term. Qualifier tokens (`repo:…`, `is:issue`) are excluded.
fn free_text_cost(free_text: &str) -> usize {
    free_text.chars().count() + 2 * free_text.split(' ').count()
}

#[test]
fn query_free_text_cost_within_budget_and_single_line_9503() {
    // #9503: the real PR #9486 title + body, as `keyword_query` folds them.
    let subj = ReviewSubject {
        owner: "bobmatnyc".to_string(),
        repo: "trusty-tools".to_string(),
        title: "feat(trusty-review): fetch_linked_issues fetches the issues a PR body links"
            .to_string(),
        body: concat!(
            "review_pr gains a strict fetch_linked_issues boolean and run gains\n",
            "--fetch-linked-issues. The review reads the keyword-linked refs of the\n",
            "raw PR body, fetches at most 5 same-repository issues with the diff\n",
            "read's token, and renders them under ## Linked issues after any\n\n",
            "issue_docs, for the reviewer and the [gh:] corpus only. Supplied docs\n",
            "are kept first; the fetched tail is dropped whole. Every failure is an\n",
            "item or row state, and error text reaches the ledger only through\n",
            "cap_detail. The issues row now reads its worst item."
        )
        .to_string(),
        identifiers: vec!["fetch_linked_issues".to_string(), "cap_detail".to_string()],
        ..Default::default()
    };
    let q = GithubIssuesSource::build_query(&subj).expect("signal");
    let free_text = q
        .strip_prefix("repo:bobmatnyc/trusty-tools is:issue ")
        .expect("qualifier prefix kept");
    assert!(
        !q.contains(['\n', '\r', '\t']) && !free_text.contains("  "),
        "whitespace not collapsed: {q:?}"
    );
    // 200 = the budget; GitHub's real limit is 256 (422 above it).
    let cost = free_text_cost(free_text);
    assert!(cost <= 200, "free-text cost {cost} > 200: {free_text:?}");
}
