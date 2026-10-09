//! LIVE GitHub Issues context source (Phase 6, #550).
//!
//! Why: the reviewed repo's own issues frequently capture the bug a PR fixes or
//! the feature it implements.  Surfacing related issues lets the reviewer judge
//! whether the diff actually closes them.  This is the fourth Stage-5 retrieval
//! source code-intelligence performs.
//!
//! What: `GithubIssuesSource` implements `ContextSource` in `Live` mode by
//! issuing a GitHub Search-API query (`GET /search/issues`) scoped to the repo
//! (`repo:{owner}/{repo} is:issue <keywords>`), then mapping each hit to a
//! `## Related GitHub issues` bullet (number, title, state, html link).
//!
//! ## Auth reuse (NO new auth)
//! GitHub auth is the Phase-1 dual-mode abstraction (#582): the token is
//! resolved by an injected `IssueTokenResolver` that, in production, delegates to
//! `AuthStrategy::select(run_mode).resolve_token(...)` — CLI → PAT/`gh`, serve →
//! App.  This source adds NO new credential mechanism; it threads the existing
//! one through a small resolver trait so the search query + parse logic stays
//! network-/auth-free for tests.
//!
//! Fail-open: token resolution failure → `NotConfigured` (skip, logged once);
//! transport / API / parse error → orchestrator logs and drops the section.
//! Never blocks the review.
//!
//! Test: `query_builds_search`, `query_capped_at_256_chars`,
//! `query_capped_at_word_boundary`, `query_short_unchanged`,
//! `parse_issues_to_section`, `disabled_without_token`, `semantic_mode_errors`,
//! `gather_with_fakes`.

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    ContextSection, ContextSnippet, ContextSource, ContextSourceError, RetrievalMode,
    ReviewSubject, SNIPPET_BODY_CHARS, TransportErr, truncate_on_char_boundary,
};
use crate::config::{ReviewConfig, constants::LOCAL_OWNER};
use crate::integrations::github::{AuthStrategy, GithubClient, RunMode};

/// Source identifier used in logs, config keys, and error messages.
pub(crate) const SOURCE_NAME: &str = "github_issues";

/// Max issues to embed in the section.
const MAX_RESULTS: u32 = 5;

/// Max diff identifiers folded into the keyword query.
const MAX_QUERY_IDENTIFIERS: usize = 4;

/// Budget for the free-text part of the GitHub issue-search query.
///
/// Why: GitHub answers 422 "The search is longer than 256 characters." based on
/// the FREE-TEXT part of `q` only: qualifiers (`repo:o/r`, `is:issue`) are
/// excluded, and each space-separated term costs about 2 extra on top of its
/// length.  It is not the raw length and not percent-encoding.  A raw 256 cap
/// let a real PR title + body through at 254 raw chars, which GitHub rejected,
/// so the whole GitHub Issues section was dropped (#9503).
/// What: the maximum free-text cost, `chars + 2 * terms`, with margin under
/// GitHub's 256.  The single place the limit lives; `cap_keywords` enforces it.
/// Test: `query_free_text_cost_within_budget_and_single_line_9503`,
/// `query_capped_at_256_chars`, `query_short_unchanged`.
const GITHUB_FREE_TEXT_BUDGET: usize = 200;

/// Extra cost GitHub charges per space-separated free-text term (empirical, #9503).
const GITHUB_TERM_COST: usize = 2;

/// Collapse whitespace and bound the free-text keywords by GitHub's cost model.
///
/// Why: prevents HTTP 422 from the GitHub Search API (see
/// [`GITHUB_FREE_TEXT_BUDGET`]).  Newlines and runs of whitespace in a PR body
/// are not search terms and only add cost.
/// What: splits on whitespace, rejoins with single spaces, and stops before the
/// term that would push `chars + 2 * terms` over the budget (a term boundary,
/// never mid-word).  A first term longer than the budget is hard-cut on a char
/// boundary.  A short input comes back unchanged apart from the collapse.
/// Test: `query_free_text_cost_within_budget_and_single_line_9503`,
/// `query_capped_at_word_boundary`, `query_short_unchanged`.
fn cap_keywords(keywords: &str) -> String {
    let mut out = String::new();
    for (terms, term) in keywords.split_whitespace().enumerate() {
        let sep = usize::from(!out.is_empty());
        let cost =
            out.chars().count() + sep + term.chars().count() + GITHUB_TERM_COST * (terms + 1);
        if cost > GITHUB_FREE_TEXT_BUDGET {
            if out.is_empty() {
                // #9503: a single giant token still has to fit the budget.
                out = term
                    .chars()
                    .take(GITHUB_FREE_TEXT_BUDGET - GITHUB_TERM_COST)
                    .collect();
            }
            tracing::debug!(
                source = SOURCE_NAME,
                capped_chars = out.chars().count(),
                budget = GITHUB_FREE_TEXT_BUDGET,
                "truncated GitHub issue-search keywords to stay within the Search API limit"
            );
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(term);
    }
    out
}

// ─── Auth seam (reuses #582 dual-mode auth) ─────────────────────────────────

/// Resolves a GitHub bearer token for the issue-search call.
///
/// Why: this is the seam that REUSES the Phase-1 dual-mode auth (#582) without
/// the source knowing whether it is PAT- or App-backed, and lets tests inject a
/// fixed token (or a failure) so the query/parse logic is testable without
/// `gh`/network.
/// What: one async method returning a token for `owner`, or a `ContextSourceError`
/// (mapped to `NotConfigured` so the orchestrator skips fail-open).
/// Test: implemented by `DualModeTokenResolver` (prod) and a fake in tests.
#[async_trait]
pub trait IssueTokenResolver: Send + Sync {
    /// Resolve a GitHub token for `owner`, or an error (treated as skip).
    async fn resolve(&self, owner: &str) -> Result<String, ContextSourceError>;
}

/// Production resolver delegating to the #582 `AuthStrategy`.
///
/// Why: keeps the single dual-mode auth funnel — no second credential path.
/// What: holds the resolved run mode + a cloned `ReviewConfig`; on `resolve`,
/// selects the strategy (CLI→PAT/`gh`, Serve→App) and resolves a token, mapping
/// any `GithubError` to `NotConfigured` (skip).
/// Test: not unit-tested directly (requires real auth); the seam is exercised
/// via the fake in `gather_with_fakes`.
pub struct DualModeTokenResolver {
    run_mode: RunMode,
    config: ReviewConfig,
}

impl DualModeTokenResolver {
    /// Construct from the run mode and a config snapshot.
    ///
    /// Why: the resolver needs the same config the rest of the GitHub path uses
    /// (App id/key, PAT) to mint a token.
    /// What: stores `run_mode` and a clone of `config`.
    /// Test: covered transitively by `GithubIssuesSource::from_config`.
    pub fn new(run_mode: RunMode, config: ReviewConfig) -> Self {
        Self { run_mode, config }
    }
}

#[async_trait]
impl IssueTokenResolver for DualModeTokenResolver {
    async fn resolve(&self, owner: &str) -> Result<String, ContextSourceError> {
        let client = GithubClient::new().map_err(|e| ContextSourceError::NotConfigured {
            src: SOURCE_NAME,
            reason: format!("failed to build HTTP client: {e}"),
        })?;
        AuthStrategy::select(self.run_mode, None)
            .resolve_token(&client, &self.config, owner)
            .await
            .map_err(|e| ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: format!("GitHub token unavailable: {e}"),
            })
    }
}

// ─── Search transport seam ──────────────────────────────────────────────────

/// Injectable transport for the GitHub issue-search call.
///
/// Why: isolate the only network call so query construction + parsing are
/// tested against canned JSON.
/// What: one async method performing `GET /search/issues?q=...` with the bearer
/// token, returning the raw JSON body (or a typed failure).
/// Test: implemented by `ReqwestIssueSearch` (prod) and a fake in tests.
#[async_trait]
pub trait IssueSearchTransport: Send + Sync {
    /// Run the issue search and return the raw response body on 2xx.
    async fn search(
        &self,
        token: &str,
        query: &str,
        per_page: u32,
    ) -> Result<String, ContextSourceError>;
}

/// Production `IssueSearchTransport` over reqwest + the GitHub Search API.
///
/// Why: the default transport for real reviews.
/// What: GETs `https://api.github.com/search/issues` with the bearer token and
/// the GitHub `User-Agent`, mapping non-2xx to `Api` and transport failures to
/// `Transport`.
/// Test: exercised via the fake in `gather_with_fakes`.
pub struct ReqwestIssueSearch {
    http: reqwest::Client,
}

impl ReqwestIssueSearch {
    /// Construct with a default 15s-timeout client.
    ///
    /// Why: bound the worst-case latency of an enrichment call.
    /// What: builds a reqwest client.  Returns `Err(ContextSourceError::Transport)`
    /// if the TLS backend cannot be initialised — surfaces the failure to
    /// `GithubIssuesSource::from_config` instead of panicking at startup (closes
    /// #953).
    /// Test: covered transitively by `GithubIssuesSource::from_config`.
    pub fn new() -> Result<Self, ContextSourceError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("failed to build HTTP client: {e}")),
            })?;
        Ok(Self { http })
    }
}

impl Default for ReqwestIssueSearch {
    /// Why: `Default` cannot return `Result`; production callers use `new()`.
    /// What: delegates to `Self::new().expect(…)`.
    /// Test: covered by `GithubIssuesSource::from_config`.
    fn default() -> Self {
        Self::new().expect("reqwest::Client::build failed — TLS backend unavailable")
    }
}

#[async_trait]
impl IssueSearchTransport for ReqwestIssueSearch {
    async fn search(
        &self,
        token: &str,
        query: &str,
        per_page: u32,
    ) -> Result<String, ContextSourceError> {
        let url = "https://api.github.com/search/issues";
        let resp = self
            .http
            .get(url)
            // Fix 4 (#599): omit `sort` so the Search API ranks by its default
            // best-match relevance (the incumbent ranks by semantic similarity;
            // best-match is the closest live-API equivalent and beats pure
            // recency for surfacing the issue a PR actually addresses).
            .query(&[("q", query), ("per_page", &per_page.to_string())])
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "trusty-review")
            .send()
            .await
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("GET {url}: {e}")),
            })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("read body of {url}: {e}")),
            })?;
        if !status.is_success() {
            return Err(ContextSourceError::Api {
                src: SOURCE_NAME,
                status: status.as_u16(),
                body: text,
            });
        }
        Ok(text)
    }
}

// Disabled sentinels: used by `from_config` when TLS init fails to build a
// permanently-disabled source.  `is_enabled()` is false so `gather` is never
// called; these impls satisfy the trait bounds but are unreachable dead-ends.

struct DisabledTokenResolver;

#[async_trait]
impl IssueTokenResolver for DisabledTokenResolver {
    async fn resolve(&self, _owner: &str) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::NotConfigured {
            src: SOURCE_NAME,
            reason: "HTTP transport unavailable (TLS init failed)".to_string(),
        })
    }
}

struct DisabledIssueSearch;

#[async_trait]
impl IssueSearchTransport for DisabledIssueSearch {
    async fn search(
        &self,
        _token: &str,
        _query: &str,
        _per_page: u32,
    ) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::Transport {
            src: SOURCE_NAME,
            err: TransportErr("HTTP transport unavailable (TLS init failed)".to_string()),
        })
    }
}

// ─── JSON shapes ────────────────────────────────────────────────────────────

/// GitHub `search/issues` response (only the fields we render).
#[derive(Debug, Deserialize)]
struct IssueSearchResponse {
    #[serde(default)]
    items: Vec<IssueItem>,
}

/// One issue search hit.
#[derive(Debug, Deserialize)]
struct IssueItem {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    html_url: String,
    /// Issue body / description (Fix 2: embedded as the snippet body excerpt).
    #[serde(default)]
    body: Option<String>,
    /// Present on PRs (not plain issues); used to filter PRs out.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

// ─── The source ─────────────────────────────────────────────────────────────

/// LIVE GitHub Issues context source.
///
/// Why: implements the `ContextSource` seam for GitHub Issues, reusing the #582
/// dual-mode auth via the injected `IssueTokenResolver`.
/// What: holds `enabled`, `mode`, the token resolver, and the search transport.
/// Unlike the Atlassian sources, "creds present" is decided at gather time (the
/// token resolver may fail), so `enabled` here reflects only config intent
/// (default disabled unless explicitly enabled — see `from_config`).
/// Test: `disabled_without_token`, `gather_with_fakes`.
pub struct GithubIssuesSource {
    enabled: bool,
    mode: RetrievalMode,
    token: Box<dyn IssueTokenResolver>,
    transport: Box<dyn IssueSearchTransport>,
}

impl GithubIssuesSource {
    /// Build from resolved config + run mode, wiring the dual-mode resolver.
    ///
    /// Why: the runner enables GitHub Issues when configured; because the token
    /// is resolved lazily at gather time (and may legitimately be present via
    /// `gh`), we treat "creds present" as true for the auto-enable decision and
    /// let the resolver fail-open at gather time if no token is actually
    /// available.  An explicit `enabled = false` still wins.
    /// What: computes `effective_enabled(true)` (auto-enable when not explicitly
    /// disabled), and attaches the `DualModeTokenResolver` + prod transport.
    /// If the reqwest TLS backend cannot be initialised the source is constructed
    /// in a permanently-disabled state so it degrades gracefully instead of
    /// panicking at startup (closes #953).
    /// Test: `from_config_respects_explicit_disable`.
    pub fn from_config(cfg: &super::SourceConfig, run_mode: RunMode, config: ReviewConfig) -> Self {
        // GitHub credentials may come from `gh` even with no env token, so we
        // cannot cheaply pre-detect them; treat as available and fail-open later.
        let transport = match ReqwestIssueSearch::new() {
            Ok(t) => Box::new(t) as Box<dyn IssueSearchTransport>,
            Err(e) => {
                tracing::error!(
                    "github_issues: failed to build HTTP transport (source disabled): {e}"
                );
                return Self {
                    enabled: false,
                    mode: cfg.mode,
                    token: Box::new(DisabledTokenResolver),
                    transport: Box::new(DisabledIssueSearch),
                };
            }
        };
        let enabled = cfg.effective_enabled(true);
        Self {
            enabled,
            mode: cfg.mode,
            token: Box::new(DualModeTokenResolver::new(run_mode, config)),
            transport,
        }
    }

    /// Construct directly (tests inject fakes).
    ///
    /// Why: drive `gather` without auth or network.
    /// What: stores the provided fields verbatim.
    /// Test: `gather_with_fakes`, `semantic_mode_errors`.
    pub fn new(
        enabled: bool,
        mode: RetrievalMode,
        token: Box<dyn IssueTokenResolver>,
        transport: Box<dyn IssueSearchTransport>,
    ) -> Self {
        Self {
            enabled,
            mode,
            token,
            transport,
        }
    }

    /// Build the GitHub search query string for the subject.
    ///
    /// Why: GitHub's issue search scopes by `repo:` and `is:issue`; centralising
    /// the construction keeps the qualifier set consistent and testable.  The
    /// free text is whitespace-collapsed and bounded by GitHub's cost model
    /// ([`GITHUB_FREE_TEXT_BUDGET`]) to prevent HTTP 422 responses when the PR
    /// title + body + identifiers are long (#9503).
    /// What: returns `repo:{owner}/{repo} is:issue <keywords>` with the keywords
    /// capped at a term boundary by `cap_keywords`.  `None` when there is no
    /// keyword signal or no owner/repo, or the owner is [`LOCAL_OWNER`]: a local diff names
    /// no repository, and the search GitHub answered with a 422 was a wasted
    /// call (#9194, owner comment 2026-10-07; Architect ruling Q7).
    /// Test: `query_builds_search`, `query_capped_at_256_chars`,
    /// `query_capped_at_word_boundary`, `query_short_unchanged`,
    /// `query_free_text_cost_within_budget_and_single_line_9503`,
    /// `github_issues_makes_no_search_call_for_the_local_owner`.
    fn build_query(subject: &ReviewSubject) -> Option<String> {
        if subject.owner.is_empty() || subject.repo.is_empty() || subject.owner == LOCAL_OWNER {
            return None;
        }
        let keywords = subject.keyword_query(MAX_QUERY_IDENTIFIERS);
        // #9503: bound the free text by GitHub's cost, not the raw query length.
        let keywords = cap_keywords(&keywords);
        if keywords.is_empty() {
            return None;
        }
        Some(format!(
            "repo:{}/{} is:issue {keywords}",
            subject.owner, subject.repo
        ))
    }

    /// Parse a GitHub issue-search body into a `ContextSection`.
    ///
    /// Why: separate parsing from the network call for unit-testability, and
    /// filter out PR hits (the search API returns PRs as issues).
    /// What: drops any item with a `pull_request` field, then maps each issue to
    /// a `ContextSnippet` (`#N — title`, subtitle = state, body = truncated issue
    /// body, link = html_url), wrapped in a `Related GitHub issues` section.
    /// The body excerpt (Fix 2, #599) gives the model the issue's description, not
    /// just its title (incumbent `pr_review_service.py:4848`).
    /// Test: `parse_issues_to_section`, `parse_embeds_body`,
    /// `parse_filters_pull_requests`.
    fn parse_section(body: &str) -> Result<ContextSection, ContextSourceError> {
        let resp: IssueSearchResponse =
            serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
                src: SOURCE_NAME,
                detail: e.to_string(),
            })?;
        let snippets = resp
            .items
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .map(|i| {
                let title = if i.title.is_empty() {
                    format!("#{}", i.number)
                } else {
                    format!("#{} — {}", i.number, i.title)
                };
                let body_excerpt = i.body.as_deref().map(str::trim).and_then(|b| {
                    (!b.is_empty())
                        .then(|| truncate_on_char_boundary(b, SNIPPET_BODY_CHARS).to_string())
                });
                ContextSnippet {
                    title,
                    subtitle: (!i.state.is_empty()).then(|| i.state.clone()),
                    body: body_excerpt,
                    link: (!i.html_url.is_empty()).then(|| i.html_url.clone()),
                }
            })
            .collect();
        Ok(ContextSection {
            heading: "Related GitHub issues".to_string(),
            snippets,
        })
    }
}

#[async_trait]
impl ContextSource for GithubIssuesSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn mode(&self) -> RetrievalMode {
        self.mode
    }

    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        if self.mode == RetrievalMode::Semantic {
            return Err(ContextSourceError::SemanticNotImplemented { src: SOURCE_NAME });
        }
        let Some(query) = Self::build_query(subject) else {
            // No repo/keyword signal (e.g. local-diff) — empty section, not error.
            return Ok(ContextSection {
                heading: "Related GitHub issues".to_string(),
                snippets: Vec::new(),
            });
        };
        // Resolve a token via the reused dual-mode auth; failure → skip fail-open.
        let token = self.token.resolve(&subject.owner).await?;
        let body = self.transport.search(&token, &query, MAX_RESULTS).await?;
        Self::parse_section(&body)
    }
}

#[cfg(test)]
#[path = "github_issues_tests.rs"]
mod tests;
