//! Tests for the prior-PR / file change-history context source.
//!
//! Why: extracted from `pr_history.rs` to keep that file under the 500-line
//! cap while preserving full coverage (parse helpers, dedup logic, the
//! semantic-mode error path, default-disabled config, and the fail-open
//! token/transport seams).
//! What: query/parse unit tests plus fake-driven `gather` tests (no network).
//! Test: included as `#[cfg(test)] mod tests` from `pr_history.rs`.

use super::*;

// ─── Fake token resolver ─────────────────────────────────────────────────────

struct FakeToken(Result<String, ()>);

#[async_trait]
impl PrHistoryTokenResolver for FakeToken {
    async fn resolve(&self, _owner: &str) -> Result<String, ContextSourceError> {
        self.0
            .clone()
            .map_err(|_| ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: "no token".to_string(),
            })
    }
}

// ─── Fake transport ──────────────────────────────────────────────────────────

/// A fake transport that maps file paths to (commits_body, prs_body_per_sha).
///
/// Why: drives the full gather loop without network calls; supports
/// per-file commit responses and per-sha PR responses.
struct FakeTransport {
    /// Keyed by file path → raw JSON commits body (or Err for API failure).
    commits_by_file: std::collections::HashMap<String, Result<String, ()>>,
    /// Keyed by sha → raw JSON PRs body (or Err for API failure).
    prs_by_sha: std::collections::HashMap<String, Result<String, ()>>,
}

impl FakeTransport {
    fn new() -> Self {
        Self {
            commits_by_file: std::collections::HashMap::new(),
            prs_by_sha: std::collections::HashMap::new(),
        }
    }

    fn with_commits(mut self, file: &str, body: Result<&str, ()>) -> Self {
        self.commits_by_file
            .insert(file.to_string(), body.map(|s| s.to_string()));
        self
    }

    fn with_prs(mut self, sha: &str, body: Result<&str, ()>) -> Self {
        self.prs_by_sha
            .insert(sha.to_string(), body.map(|s| s.to_string()));
        self
    }
}

#[async_trait]
impl PrHistoryTransport for FakeTransport {
    async fn fetch_commits(
        &self,
        _token: &str,
        _owner: &str,
        _repo: &str,
        path: &str,
        _per_page: u32,
    ) -> Result<String, ContextSourceError> {
        match self.commits_by_file.get(path) {
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(())) => Err(ContextSourceError::Api {
                src: SOURCE_NAME,
                status: 500,
                body: "server error".to_string(),
            }),
            None => Ok("[]".to_string()), // no commits → empty array
        }
    }

    async fn fetch_prs_for_commit(
        &self,
        _token: &str,
        _owner: &str,
        _repo: &str,
        sha: &str,
    ) -> Result<String, ContextSourceError> {
        match self.prs_by_sha.get(sha) {
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(())) => Err(ContextSourceError::Api {
                src: SOURCE_NAME,
                status: 500,
                body: "server error".to_string(),
            }),
            None => Ok("[]".to_string()), // no PRs for this commit
        }
    }
}

// ─── Subject helper ──────────────────────────────────────────────────────────

fn subject_with_files(files: &[&str]) -> ReviewSubject {
    ReviewSubject {
        owner: "acme".to_string(),
        repo: "backend".to_string(),
        title: "Fix streaming".to_string(),
        changed_files: files.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

// ─── Parse tests ─────────────────────────────────────────────────────────────

#[test]
fn parse_commit_shas_extracts_shas() {
    // Why: verifies the commit JSON is parsed correctly into SHA strings.
    let body = r#"[{"sha":"abc123"},{"sha":"def456"}]"#;
    let shas = PrHistorySource::parse_commit_shas(body).unwrap();
    assert_eq!(shas, vec!["abc123", "def456"]);
}

#[test]
fn parse_commit_shas_empty_array() {
    // Why: an empty commits array must yield an empty vec (no panic).
    let shas = PrHistorySource::parse_commit_shas("[]").unwrap();
    assert!(shas.is_empty());
}

#[test]
fn parse_commit_shas_garbage_returns_parse_error() {
    // Why: malformed JSON must return a Parse error (no panic).
    assert!(matches!(
        PrHistorySource::parse_commit_shas("nope"),
        Err(ContextSourceError::Parse { .. })
    ));
}

#[test]
fn parse_pr_entries_extracts_fields() {
    // Why: verifies PR entry fields are parsed (number, title, state, url, body).
    let body = r#"[{
        "number": 42,
        "title": "Fix streaming regression",
        "state": "closed",
        "html_url": "https://github.com/acme/backend/pull/42",
        "body": "Closes the streaming bug introduced in #40."
    }]"#;
    let prs = PrHistorySource::parse_pr_entries(body).unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].number, 42);
    assert_eq!(prs[0].title, "Fix streaming regression");
    assert_eq!(prs[0].state, "closed");
    assert_eq!(prs[0].html_url, "https://github.com/acme/backend/pull/42");
    assert_eq!(
        prs[0].body.as_deref(),
        Some("Closes the streaming bug introduced in #40.")
    );
}

#[test]
fn parse_pr_entries_garbage_returns_parse_error() {
    // Why: malformed JSON must return a Parse error (no panic).
    assert!(matches!(
        PrHistorySource::parse_pr_entries("not json"),
        Err(ContextSourceError::Parse { .. })
    ));
}

// ─── build_section tests ─────────────────────────────────────────────────────

fn make_pr_entry(number: u64, title: &str, state: &str) -> PrEntry {
    PrEntry {
        number,
        title: title.to_string(),
        state: state.to_string(),
        html_url: format!("https://github.com/acme/backend/pull/{number}"),
        body: Some(format!("Description of PR {number}")),
    }
}

#[test]
fn build_section_heading() {
    // Why: the section heading must be exactly the expected constant string.
    let section = PrHistorySource::build_section(HashMap::new());
    assert_eq!(section.heading, "Prior PRs touching changed files");
}

#[test]
fn build_section_deduplicates_by_pr_number() {
    // Why: the same PR discovered via multiple files/commits must appear once.
    let mut prs = HashMap::new();
    prs.insert(42, make_pr_entry(42, "Fix streaming", "closed"));
    prs.insert(43, make_pr_entry(43, "Add context", "merged"));
    let section = PrHistorySource::build_section(prs);
    assert_eq!(section.snippets.len(), 2);
}

#[test]
fn build_section_most_recent_first() {
    // Why: snippets are ordered by descending PR number (most recent first).
    let mut prs = HashMap::new();
    for n in [10, 30, 20u64] {
        prs.insert(n, make_pr_entry(n, "PR title", "closed"));
    }
    let section = PrHistorySource::build_section(prs);
    let numbers: Vec<u64> = section
        .snippets
        .iter()
        .map(|s| {
            s.title
                .trim_start_matches("PR #")
                .split(':')
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .collect();
    assert_eq!(numbers, vec![30, 20, 10]);
}

#[test]
fn build_section_capped_at_max_prs() {
    // Why: output must be capped at MAX_PRS (5) even with more unique PRs.
    let mut prs = HashMap::new();
    for n in 1..=10u64 {
        prs.insert(n, make_pr_entry(n, "PR", "closed"));
    }
    let section = PrHistorySource::build_section(prs);
    assert_eq!(section.snippets.len(), MAX_PRS);
}

#[test]
fn build_section_snippet_title_format() {
    // Why: title must follow the "PR #N: title" format.
    let mut prs = HashMap::new();
    prs.insert(99, make_pr_entry(99, "Big refactor", "closed"));
    let section = PrHistorySource::build_section(prs);
    assert_eq!(section.snippets[0].title, "PR #99: Big refactor");
}

#[test]
fn build_section_snippet_body_truncated() {
    // Why: long PR bodies must be truncated to PR_BODY_CHARS.
    let long_body = "x".repeat(PR_BODY_CHARS + 100);
    let mut prs = HashMap::new();
    prs.insert(
        1,
        PrEntry {
            number: 1,
            title: "t".to_string(),
            state: "closed".to_string(),
            html_url: "u".to_string(),
            body: Some(long_body),
        },
    );
    let section = PrHistorySource::build_section(prs);
    let body_len = section.snippets[0].body.as_deref().unwrap().chars().count();
    assert_eq!(body_len, PR_BODY_CHARS);
}

#[test]
fn build_section_no_body_when_empty() {
    // Why: empty/whitespace-only PR bodies must not produce a snippet body.
    let mut prs = HashMap::new();
    prs.insert(
        1,
        PrEntry {
            number: 1,
            title: "t".to_string(),
            state: "closed".to_string(),
            html_url: "u".to_string(),
            body: Some("   ".to_string()),
        },
    );
    let section = PrHistorySource::build_section(prs);
    assert!(section.snippets[0].body.is_none());
}

// ─── gather integration tests ─────────────────────────────────────────────────

#[tokio::test]
async fn gather_deduplicates_prs_across_files() {
    // Why: the same PR associated with commits from two different files must
    // appear only once in the output.
    let commits_a = r#"[{"sha":"sha1"}]"#;
    let commits_b = r#"[{"sha":"sha2"}]"#;
    // Both commits map to the same PR number 42.
    let prs_for_sha1 = r#"[{"number":42,"title":"Fix streaming","state":"closed","html_url":"https://github.com/acme/backend/pull/42","body":"desc"}]"#;
    let prs_for_sha2 = r#"[{"number":42,"title":"Fix streaming","state":"closed","html_url":"https://github.com/acme/backend/pull/42","body":"desc"}]"#;

    let transport = FakeTransport::new()
        .with_commits("src/stream.rs", Ok(commits_a))
        .with_commits("src/context.rs", Ok(commits_b))
        .with_prs("sha1", Ok(prs_for_sha1))
        .with_prs("sha2", Ok(prs_for_sha2));

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let section = src
        .gather(&subject_with_files(&["src/stream.rs", "src/context.rs"]))
        .await
        .expect("gather should succeed");

    assert_eq!(section.heading, "Prior PRs touching changed files");
    assert_eq!(section.snippets.len(), 1, "PR #42 deduplicated");
    assert!(section.snippets[0].title.contains("42"));
}

#[tokio::test]
async fn gather_collects_multiple_prs() {
    // Why: when different files/commits yield different PR numbers, all
    // distinct PRs are included (up to MAX_PRS).
    let commits_a = r#"[{"sha":"sha1"}]"#;
    let commits_b = r#"[{"sha":"sha2"}]"#;
    let prs_sha1 =
        r#"[{"number":10,"title":"PR ten","state":"closed","html_url":"u10","body":"b10"}]"#;
    let prs_sha2 =
        r#"[{"number":20,"title":"PR twenty","state":"merged","html_url":"u20","body":"b20"}]"#;

    let transport = FakeTransport::new()
        .with_commits("a.rs", Ok(commits_a))
        .with_commits("b.rs", Ok(commits_b))
        .with_prs("sha1", Ok(prs_sha1))
        .with_prs("sha2", Ok(prs_sha2));

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let section = src
        .gather(&subject_with_files(&["a.rs", "b.rs"]))
        .await
        .expect("gather should succeed");

    assert_eq!(section.snippets.len(), 2);
    // Most recent first: PR #20 before PR #10.
    assert!(section.snippets[0].title.contains("20"));
    assert!(section.snippets[1].title.contains("10"));
}

#[tokio::test]
async fn gather_fail_open_on_commits_api_error() {
    // Why: when the commits fetch fails for a file, that file is skipped
    // (fail-open) and the source returns whatever PRs were found for other
    // files — the review is NOT blocked.
    let commits_good = r#"[{"sha":"sha_good"}]"#;
    let prs_good =
        r#"[{"number":7,"title":"Good PR","state":"closed","html_url":"u7","body":"desc"}]"#;

    let transport = FakeTransport::new()
        .with_commits("good.rs", Ok(commits_good))
        .with_commits("bad.rs", Err(())) // API error on this file
        .with_prs("sha_good", Ok(prs_good));

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let section = src
        .gather(&subject_with_files(&["good.rs", "bad.rs"]))
        .await
        .expect("gather should succeed (fail-open)");

    // The good file's PR survives; the bad file's error is swallowed.
    assert_eq!(section.snippets.len(), 1);
    assert!(section.snippets[0].title.contains("7"));
}

#[tokio::test]
async fn gather_fail_open_on_prs_for_commit_api_error() {
    // Why: when the PR-lookup for a specific commit fails, that commit is
    // skipped (fail-open) and other commits in the same file are still
    // processed.
    let commits = r#"[{"sha":"sha_err"},{"sha":"sha_ok"}]"#;
    let prs_ok = r#"[{"number":5,"title":"OK PR","state":"closed","html_url":"u5","body":"d"}]"#;

    let transport = FakeTransport::new()
        .with_commits("file.rs", Ok(commits))
        .with_prs("sha_err", Err(())) // PR lookup error
        .with_prs("sha_ok", Ok(prs_ok));

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let section = src
        .gather(&subject_with_files(&["file.rs"]))
        .await
        .expect("gather should succeed (fail-open)");

    assert_eq!(section.snippets.len(), 1);
    assert!(section.snippets[0].title.contains("5"));
}

#[tokio::test]
async fn gather_returns_empty_section_for_no_signal() {
    // Why: when no changed files are provided (local-diff with no diff), the
    // source must return an empty section, not an error.
    let transport = FakeTransport::new();

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let section = src
        .gather(&subject_with_files(&[]))
        .await
        .expect("empty subject should yield empty section, not error");

    assert!(section.snippets.is_empty());
    assert_eq!(section.heading, "Prior PRs touching changed files");
}

#[tokio::test]
async fn gather_returns_empty_section_for_local_diff_mode() {
    // Why: when owner/repo are empty (local-diff mode), gather must short-circuit
    // and return an empty section, not call the transport.
    let transport = FakeTransport::new();

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let local_diff_subject = ReviewSubject {
        owner: String::new(),
        repo: String::new(),
        changed_files: vec!["src/main.rs".to_string()],
        ..Default::default()
    };

    let section = src
        .gather(&local_diff_subject)
        .await
        .expect("local-diff mode must return empty section, not error");

    assert!(section.snippets.is_empty());
}

#[tokio::test]
async fn gather_skip_when_token_unavailable() {
    // Why: a token resolution failure must propagate as a NotConfigured error
    // so the orchestrator can log it and skip this source (fail-open at the
    // orchestrator level, not inside gather).
    let transport = FakeTransport::new();

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Live,
        Box::new(FakeToken(Err(()))), // no token
        Box::new(transport),
    );

    let r = src.gather(&subject_with_files(&["src/main.rs"])).await;
    assert!(
        matches!(r, Err(ContextSourceError::NotConfigured { .. })),
        "token failure must produce NotConfigured"
    );
}

#[tokio::test]
async fn gather_semantic_mode_errors() {
    // Why: semantic mode is not yet implemented; the source must return
    // SemanticNotImplemented so the orchestrator can log it.
    let transport = FakeTransport::new();

    let src = PrHistorySource::new(
        true,
        RetrievalMode::Semantic,
        Box::new(FakeToken(Ok("tok".to_string()))),
        Box::new(transport),
    );

    let r = src.gather(&subject_with_files(&["a.rs"])).await;
    assert!(
        matches!(
            r,
            Err(ContextSourceError::SemanticNotImplemented { src: "pr_history" })
        ),
        "semantic mode must produce SemanticNotImplemented"
    );
}

// ─── from_config tests ────────────────────────────────────────────────────────

#[test]
fn from_config_default_disabled_without_explicit_enable() {
    // Why: mirrors ConformanceSource — default DISABLED even when creds are
    // present; operator must explicitly set enabled = true.
    let cfg = super::super::SourceConfig {
        enabled: None, // no explicit intent
        mode: RetrievalMode::Live,
    };
    let src = PrHistorySource::from_config(
        &cfg,
        crate::integrations::github::RunMode::Cli,
        crate::config::ReviewConfig::load(None),
    );
    assert!(
        !src.is_enabled(),
        "pr_history must be default-disabled without explicit enabled = true"
    );
}

#[test]
fn from_config_respects_explicit_enable() {
    // Why: an operator who opts in (enabled = true) must get the source enabled.
    let cfg = super::super::SourceConfig {
        enabled: Some(true),
        mode: RetrievalMode::Live,
    };
    let src = PrHistorySource::from_config(
        &cfg,
        crate::integrations::github::RunMode::Cli,
        crate::config::ReviewConfig::load(None),
    );
    assert!(
        src.is_enabled(),
        "explicit enabled = true must enable the source"
    );
}

#[test]
fn from_config_respects_explicit_disable() {
    // Why: explicit enabled = false must always win.
    let cfg = super::super::SourceConfig {
        enabled: Some(false),
        mode: RetrievalMode::Live,
    };
    let src = PrHistorySource::from_config(
        &cfg,
        crate::integrations::github::RunMode::Cli,
        crate::config::ReviewConfig::load(None),
    );
    assert!(!src.is_enabled());
}

// ─── Heading constant test ────────────────────────────────────────────────────

#[test]
fn source_name_is_pr_history() {
    // Why: the name is used in env-var construction and logs; must be stable.
    let src = PrHistorySource::new(
        false,
        RetrievalMode::Live,
        Box::new(FakeToken(Err(()))),
        Box::new(FakeTransport::new()),
    );
    assert_eq!(src.name(), "pr_history");
}
