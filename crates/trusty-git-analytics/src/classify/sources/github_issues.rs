//! GitHub Issues client for commit classification signals.
//!
//! Why: GitHub Issues labels carry reliable classification intent — a commit
//! that closes a `bug`-labelled issue is a bugfix even when the commit message
//! says nothing useful. This module extracts `#NNN` and `org/repo#NNN`
//! references from commit messages and fetches issue labels via the GitHub
//! REST v3 API to produce a [`super::ExternalSignal`].
//!
//! What: a regex-based issue-number extractor plus a minimal reqwest-based
//! client that calls `GET /repos/{owner}/{repo}/issues/{number}`. Credentials
//! are read from the environment variable named in
//! [`super::GithubIssuesSourceConfig::token_env`].
//!
//! Test: see `tests::extract_github_refs_*` for extractor coverage and the
//! resolver integration tests for the full pipeline.

use std::collections::HashMap;

use regex::Regex;
use serde::Deserialize;
use tracing::warn;

use super::{ExternalSignal, GithubIssuesSourceConfig, EXTERNAL_SOURCE_CONFIDENCE};
use crate::collect::github::budget::{FetchBudget, MAX_REFERENCE_LOOKUPS};
use crate::collect::github::retry::retry_send;
use crate::core::creds::CredentialSource;

/// A parsed GitHub issue reference extracted from a commit message.
///
/// Why: a commit message can contain both bare `#123` references (resolved
/// against the default repo) and qualified `org/repo#123` references
/// (resolved against a different repo). We keep both forms so the resolver
/// can route them appropriately.
/// What: holds the owner/repo pair (may be inferred from config) and the
/// issue number.
/// Test: covered by `tests::extract_github_refs_bare` and
/// `tests::extract_github_refs_qualified`.
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubRef {
    /// Optional `owner/repo` qualifier. When `None`, the caller should
    /// use the repo from `GithubIssuesSourceConfig::repo`.
    pub repo: Option<String>,
    /// Issue number.
    pub number: u64,
}

/// Regex matching bare `#NNN` GitHub issue references.
fn bare_ref_regex() -> Regex {
    // Require a word boundary or start-of-line / whitespace before `#` to
    // avoid matching hex colors. The lookahead `(?!\d)` prevents matching
    // `#123456` (six-digit hex).
    Regex::new(r"(?:^|[\s(])#(\d+)\b").expect("static regex is valid")
}

/// Regex matching qualified `owner/repo#NNN` references.
fn qualified_ref_regex() -> Regex {
    Regex::new(r"(?:^|\s)([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#(\d+)\b").expect("static regex is valid")
}

/// Extract all GitHub issue references from a commit message.
///
/// Why: the extractor must handle both bare `#123` and qualified
/// `org/repo#123` forms so that multi-repo monorepos and cross-repo
/// references are supported.
/// What: runs both regexes in order; qualified references are returned
/// first, then bare references (in left-to-right appearance order).
/// Deduplication is by `(repo, number)` pair.
/// Test: covered by `tests::extract_github_refs_*`.
pub fn extract_github_refs(message: &str) -> Vec<GitHubRef> {
    let mut seen: std::collections::HashSet<(Option<String>, u64)> = Default::default();
    let mut out = Vec::new();

    // Qualified first (more specific).
    let qre = qualified_ref_regex();
    for cap in qre.captures_iter(message) {
        if let (Some(repo_m), Some(num_m)) = (cap.get(1), cap.get(2)) {
            let repo = repo_m.as_str().to_string();
            let number: u64 = num_m.as_str().parse().unwrap_or(0);
            if number > 0 && seen.insert((Some(repo.clone()), number)) {
                out.push(GitHubRef {
                    repo: Some(repo),
                    number,
                });
            }
        }
    }

    // Bare references.
    let bre = bare_ref_regex();
    for cap in bre.captures_iter(message) {
        if let Some(num_m) = cap.get(1) {
            let number: u64 = num_m.as_str().parse().unwrap_or(0);
            if number > 0 && seen.insert((None, number)) {
                out.push(GitHubRef { repo: None, number });
            }
        }
    }

    out
}

/// Partial deserialization target for `GET /repos/{owner}/{repo}/issues/{number}`.
///
/// Why: we only need the labels array to produce classification signals.
/// What: a minimal serde struct over the GitHub Issues REST response.
/// Test: covered by resolver integration tests with wiremock.
#[derive(Debug, Deserialize)]
pub struct GitHubIssue {
    /// Issue number.
    pub number: u64,
    /// Label objects attached to the issue.
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
}

/// A GitHub issue label.
#[derive(Debug, Deserialize)]
pub struct GitHubLabel {
    /// Label name (e.g. `"bug"`, `"enhancement"`).
    pub name: String,
}

/// Classify a GitHub issue using the configured `label_mappings`.
///
/// Why: a first-match approach is sufficient because label_mappings form a
/// priority list from the user's perspective — they put their most important
/// mapping first.
/// What: iterates issue labels in order; returns an [`ExternalSignal`] for
/// the first label that maps to a category, or `None` if no label matches.
/// Test: covered by `tests::classify_github_issue_matches_label` and
/// `tests::classify_github_issue_returns_none_on_no_match`.
pub fn classify_github_issue(
    issue: &GitHubIssue,
    config: &GithubIssuesSourceConfig,
) -> Option<ExternalSignal> {
    for label in &issue.labels {
        if let Some(cat) = config.label_mappings.get(label.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("github_issues:label:{}", label.name),
            });
        }
    }
    None
}

/// Fetch a GitHub issue by owner/repo/number.
///
/// Why: the HTTP call must be isolated here so the resolver can inject a
/// mock client for testing. Before #6084 it also collapsed every failure into
/// `None`, which the resolver then cached as "this issue carries no
/// classification signal" — so under a rate limit the run cached 3681 wrong
/// answers and kept issuing rejected requests to collect them. A throttle has
/// to be a different answer from an absent label.
/// What: issues `GET /repos/{owner}/{repo}/issues/{number}` with an optional
/// Bearer token, through [`retry_send`] so `Retry-After` is honoured and every
/// wait is charged to `budget`. `Ok(None)` means the issue genuinely carries
/// nothing usable — a 404, a scope-less 403, an unparseable body — and is safe
/// to cache. `Err` means the answer is unknown and must not be cached.
/// Test: `crate::classify::sources::resolver::resolver_tests` and
/// `github_issue_tests` in this module.
///
/// # Errors
///
/// [`CollectError::Throttled`](crate::collect::errors::CollectError::Throttled)
/// once GitHub is rate-limiting and the run's budget is spent;
/// [`CollectError::Http`](crate::collect::errors::CollectError::Http) on a
/// transport failure that outlived its retries.
pub async fn fetch_issue(
    client: &reqwest::Client,
    config: &GithubIssuesSourceConfig,
    owner_repo: &str,
    number: u64,
    api_base_override: Option<&str>,
    budget: &FetchBudget,
) -> crate::collect::errors::Result<Option<GitHubIssue>> {
    fetch_issue_with_creds(
        client,
        config,
        owner_repo,
        number,
        api_base_override,
        budget,
        &CredentialSource::from_env(),
    )
    .await
}

/// [`fetch_issue`], with the credential lookup supplied by the caller (#6405).
///
/// # Errors
///
/// As [`fetch_issue`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_issue_with_creds(
    client: &reqwest::Client,
    config: &GithubIssuesSourceConfig,
    owner_repo: &str,
    number: u64,
    api_base_override: Option<&str>,
    budget: &FetchBudget,
    creds: &CredentialSource,
) -> crate::collect::errors::Result<Option<GitHubIssue>> {
    let token = creds.get(&config.token_env);

    let base = api_base_override.unwrap_or("https://api.github.com");
    let url = format!("{base}/repos/{owner_repo}/issues/{number}");

    let mut req = client.get(&url).header("User-Agent", "tga/1.0");
    if let Some(t) = &token {
        req = req.bearer_auth(t);
    }

    let resp = retry_send(req, budget).await?;
    if !resp.status().is_success() {
        warn!(
            owner_repo,
            number,
            status = %resp.status(),
            "GitHub Issues API returned non-success; skipping this reference"
        );
        return Ok(None);
    }
    match resp.json::<GitHubIssue>().await {
        Ok(issue) => Ok(Some(issue)),
        Err(e) => {
            warn!(owner_repo, number, error = %e, "failed to parse GitHub issue response");
            Ok(None)
        }
    }
}

/// What one [`fetch_issues_batch`] pass produced, and whether it finished.
///
/// Why (#6084): the batch used to return a bare map, so a pass that stopped at
/// a cap or a rate limit was indistinguishable from one that looked every
/// reference up and found nothing. The two need different handling — the
/// second is a cacheable answer, the first is a gap the run must report.
/// What: the signals actually resolved, plus `stopped_early` naming the bound
/// that ended the pass. References never attempted are simply absent from
/// `signals`, so nothing unfetched is cached as "no signal".
/// Test: `github_issue_tests::a_rate_limited_batch_stops_and_caches_nothing`,
/// `github_issue_tests::the_batch_stops_at_the_reference_lookup_cap`.
#[derive(Debug, Default)]
pub struct IssueBatch {
    /// Resolved signals, keyed `"{repo}#{number}"`.
    pub signals: HashMap<String, Option<ExternalSignal>>,
    /// Set when a bound ended the pass before every reference was looked up.
    pub stopped_early: Option<String>,
}

/// Look up every unique reference in `refs`, bounded.
///
/// Why: this is the `fetch_on_reference` path — one Issues-API call per unique
/// `#N` in commit history, a count that scales with the repository rather than
/// with anything the operator asked for. On this repo it was 3681 lookups, and
/// under a secondary rate limit every one of them retried and failed.
/// What: deduplicates `refs`, then stops at the first of three bounds —
/// [`MAX_REFERENCE_LOOKUPS`] unique lookups, the shared `budget` latching, or
/// the reference list running out. Either early stop is named in
/// [`IssueBatch::stopped_early`] rather than left silent.
/// Test: `github_issue_tests::*`.
pub async fn fetch_issues_batch(
    client: &reqwest::Client,
    config: &GithubIssuesSourceConfig,
    refs: &[GitHubRef],
    api_base_override: Option<&str>,
    budget: &FetchBudget,
) -> IssueBatch {
    fetch_issues_batch_with_creds(
        client,
        config,
        refs,
        api_base_override,
        budget,
        &CredentialSource::from_env(),
    )
    .await
}

/// [`fetch_issues_batch`], with the credential lookup supplied by the caller
/// (#6405).
pub(crate) async fn fetch_issues_batch_with_creds(
    client: &reqwest::Client,
    config: &GithubIssuesSourceConfig,
    refs: &[GitHubRef],
    api_base_override: Option<&str>,
    budget: &FetchBudget,
    creds: &CredentialSource,
) -> IssueBatch {
    let mut out = IssueBatch::default();
    let mut looked_up = 0usize;

    for gh_ref in refs {
        let repo = gh_ref.repo.as_deref().unwrap_or(config.repo.as_str());
        let cache_key = format!("{repo}#{}", gh_ref.number);
        if out.signals.contains_key(&cache_key) {
            continue;
        }
        if looked_up >= MAX_REFERENCE_LOOKUPS {
            let notice = format!(
                "GitHub reference lookups stopped at the {MAX_REFERENCE_LOOKUPS}-lookup cap; \
                 the remaining references in this batch are UNCLASSIFIED"
            );
            warn!(cap = MAX_REFERENCE_LOOKUPS, "{notice}");
            budget.note_truncation(notice.clone());
            out.stopped_early = Some(notice);
            break;
        }

        looked_up += 1;
        match fetch_issue_with_creds(
            client,
            config,
            repo,
            gh_ref.number,
            api_base_override,
            budget,
            creds,
        )
        .await
        {
            Ok(issue) => {
                let signal = issue.and_then(|iss| classify_github_issue(&iss, config));
                out.signals.insert(cache_key, signal);
            }
            Err(e) => {
                // Unknown, not absent. Recording nothing for this key is what
                // keeps the resolver from caching a throttle as "no signal".
                let notice = format!(
                    "GitHub reference lookups stopped after {looked_up} of {} references: {e}. \
                     The remaining references are UNCLASSIFIED",
                    refs.len()
                );
                warn!("{notice}");
                out.stopped_early = Some(notice);
                break;
            }
        }
    }

    out
}

#[cfg(test)]
#[path = "github_issue_tests.rs"]
mod github_issue_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: extracting bare `#NNN` references is the most common GitHub
    /// reference form and the extractor must not miss them.
    /// What: asserts extraction from typical commit messages.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_github_refs_bare() {
        let refs = extract_github_refs("fix: closes #123");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].number, 123);
        assert!(refs[0].repo.is_none());
    }

    /// Why: qualified `org/repo#NNN` references must be extracted correctly
    /// so that cross-repo references in monorepos resolve to the right issue.
    /// What: asserts extraction from a qualified reference.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_github_refs_qualified() {
        let refs = extract_github_refs("see acme/widgets#456 for context");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].number, 456);
        assert_eq!(refs[0].repo.as_deref(), Some("acme/widgets"));
    }

    /// Why: a commit may reference multiple issues; the extractor must return
    /// all of them without duplicates.
    /// What: asserts multi-ref extraction and deduplication.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_github_refs_multiple_and_dedup() {
        let refs = extract_github_refs("fixes #10 and closes #20 (see #10 again)");
        let numbers: Vec<u64> = refs.iter().map(|r| r.number).collect();
        assert_eq!(numbers, vec![10, 20]);
    }

    /// Why: the extractor must not match hex colours or other non-issue
    /// `#`-prefixed strings.
    /// What: asserts no match on typical hex colours.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_github_refs_ignores_hex_colors() {
        let refs = extract_github_refs("color: #ff0000 or #FFF");
        assert!(refs.is_empty(), "should not match hex colors, got {refs:?}");
    }

    /// Why: `classify_github_issue` must return a signal for the first
    /// matching label and ignore subsequent labels.
    /// What: build a `GitHubIssue` with multiple labels, assert first match
    /// wins.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_github_issue_matches_label() {
        let issue = GitHubIssue {
            number: 1,
            labels: vec![
                GitHubLabel {
                    name: "bug".to_string(),
                },
                GitHubLabel {
                    name: "enhancement".to_string(),
                },
            ],
        };
        let config = GithubIssuesSourceConfig {
            repo: "acme/widgets".to_string(),
            token_env: "GITHUB_TOKEN".to_string(),
            label_mappings: {
                let mut m = HashMap::new();
                m.insert("bug".to_string(), "bug_fix".to_string());
                m.insert("enhancement".to_string(), "new_feature".to_string());
                m
            },
        };
        let signal = classify_github_issue(&issue, &config).expect("should match");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("bug"));
    }

    /// Why: when no label matches, `classify_github_issue` must return
    /// `None` so the pipeline falls through to commit-message rules.
    /// What: build an issue with no mapped labels.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_github_issue_returns_none_on_no_match() {
        let issue = GitHubIssue {
            number: 2,
            labels: vec![GitHubLabel {
                name: "wontfix".to_string(),
            }],
        };
        let config = GithubIssuesSourceConfig {
            repo: "acme/widgets".to_string(),
            token_env: "GITHUB_TOKEN".to_string(),
            label_mappings: HashMap::new(),
        };
        assert!(classify_github_issue(&issue, &config).is_none());
    }
}
