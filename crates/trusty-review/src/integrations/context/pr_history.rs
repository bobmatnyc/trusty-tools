//! LIVE prior-PR / file change-history context source (T10, #1423).
//!
//! Why: reviewers ground feedback in prior commits/PRs touching the same files
//! ("this looks like a bandaid fix — see prior streaming work in PR #10234").
//! Without historical PR context the LLM has no awareness of how the touched
//! files have been changed before or what technical debt patterns exist.  This
//! source surfaces recent closed PRs that touched the changed files so the
//! reviewer can detect repeated patterns, regressions, or related prior fixes.
//!
//! What: `PrHistorySource` implements `ContextSource` in `Live` mode.  For each
//! changed file (capped at `MAX_FILES_TO_QUERY`) it calls
//! `GET /repos/{owner}/{repo}/commits?path={file}&per_page=10` to get recent
//! commits touching that file, then for each commit SHA calls
//! `GET /repos/{owner}/{repo}/commits/{sha}/pulls` (with the groot-preview
//! header) to find PRs associated with that commit.  All discovered PRs are
//! deduped by number, sorted by descending PR number (most recent first), and
//! capped at `MAX_PRS` entries.  Each is rendered as a `ContextSnippet` under
//! `## Prior PRs touching changed files`.
//!
//! ## Auth reuse (NO new auth)
//! GitHub auth is the Phase-1 dual-mode abstraction (#582): the token is
//! resolved by an injected `PrHistoryTokenResolver` that, in production,
//! delegates to `AuthStrategy::select(run_mode).resolve_token(...)` — CLI →
//! PAT/`gh`, serve → App.  No new credential mechanism is added.
//!
//! ## Config / default-disabled
//! Default DISABLED (mirrors `ConformanceSource`): this source requires GitHub
//! API access and makes multiple requests per review.  Operators opt in by
//! setting `TRUSTY_REVIEW_CONTEXT_PR_HISTORY_ENABLED=true` or
//! `[context.sources.pr_history] enabled = true` in the TOML config.
//!
//! ## Fail-open
//! Any API error (per-file commit fetch, per-commit PR lookup) is logged and
//! contributes no snippets.  A partial result (some files succeed, some fail)
//! is better than no result, so per-file errors are not fatal.  The orchestrator
//! never sees these errors; they are caught within `gather`.
//!
//! Test: `pr_history_tests.rs` — snippet count, dedup, heading, default-disabled
//! skip, and fail-open on API error.

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::{
    ContextSection, ContextSnippet, ContextSource, ContextSourceError, RetrievalMode,
    ReviewSubject, TransportErr, truncate_on_char_boundary,
};
use crate::config::ReviewConfig;
use crate::integrations::github::{AuthStrategy, GithubClient, RunMode};

/// Source identifier used in logs, config keys, and error messages.
const SOURCE_NAME: &str = "pr_history";

/// Max changed files to query (limits API call count: `MAX_FILES * up-to-10
/// commits * 1 PR-lookup` per file = up to 50 HTTP requests).
const MAX_FILES_TO_QUERY: usize = 5;

/// Max recent commits fetched per file (GitHub API `per_page`).
const COMMITS_PER_FILE: u32 = 10;

/// Max distinct PRs to include in the section.
const MAX_PRS: usize = 5;

/// Max chars of a PR body excerpt embedded under a `## Prior PRs…` bullet.
///
/// Why: the spec calls for ~300 chars per PR body to bound prompt size while
/// still conveying intent; the other sources use `SNIPPET_BODY_CHARS` (500),
/// but historical PR context is secondary enrichment so we keep it shorter.
/// What: the truncation limit passed to `truncate_on_char_boundary`.
/// Test: `snippet_body_truncated`.
const PR_BODY_CHARS: usize = 300;

// ─── Auth seam (reuses #582 dual-mode auth) ─────────────────────────────────

/// Resolves a GitHub bearer token for the commit / PR-lookup calls.
///
/// Why: mirrors `github_issues::IssueTokenResolver` — reuses the Phase-1
/// dual-mode auth (#582) without this source knowing whether it is PAT- or
/// App-backed, and lets tests inject a fixed token so the fetch logic is
/// testable without `gh`/network.
/// What: one async method returning a token for `owner`, or a
/// `ContextSourceError` (mapped to `NotConfigured` so the orchestrator skips
/// fail-open).
/// Test: implemented by `DualModePrHistoryToken` (prod) and a fake in tests.
#[async_trait]
pub trait PrHistoryTokenResolver: Send + Sync {
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
/// via the fake in `pr_history_tests`.
pub struct DualModePrHistoryToken {
    run_mode: RunMode,
    config: ReviewConfig,
}

impl DualModePrHistoryToken {
    /// Construct from the run mode and a config snapshot.
    ///
    /// Why: the resolver needs the same config the rest of the GitHub path uses
    /// (App id/key, PAT) to mint a token.
    /// What: stores `run_mode` and a clone of `config`.
    /// Test: covered transitively by `PrHistorySource::from_config`.
    pub fn new(run_mode: RunMode, config: ReviewConfig) -> Self {
        Self { run_mode, config }
    }
}

#[async_trait]
impl PrHistoryTokenResolver for DualModePrHistoryToken {
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

// ─── Transport seam ──────────────────────────────────────────────────────────

/// Injectable transport for the GitHub commits-per-path and PR-lookup calls.
///
/// Why: isolate all network calls so fetch logic + dedup + rendering are tested
/// against canned JSON without hitting the real GitHub API.
/// What: two async methods — one for `GET /repos/{owner}/{repo}/commits?path=`
/// and one for `GET /repos/{owner}/{repo}/commits/{sha}/pulls`.  Both return
/// the raw JSON body on 2xx, or a `ContextSourceError` on failure.
/// Test: implemented by `ReqwestPrHistoryTransport` (prod) and a fake in tests.
#[async_trait]
pub trait PrHistoryTransport: Send + Sync {
    /// Fetch recent commits touching `path` in `{owner}/{repo}`.
    async fn fetch_commits(
        &self,
        token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        per_page: u32,
    ) -> Result<String, ContextSourceError>;

    /// Fetch PRs associated with a commit `sha` in `{owner}/{repo}`.
    ///
    /// Uses the `groot-preview` media type required by the GitHub Commits API
    /// "List pull requests associated with a commit" endpoint.
    async fn fetch_prs_for_commit(
        &self,
        token: &str,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<String, ContextSourceError>;
}

/// Production `PrHistoryTransport` over reqwest + GitHub REST API.
///
/// Why: the default transport for real reviews.
/// What: GETs `https://api.github.com/repos/{owner}/{repo}/commits?path=…` and
/// the per-commit `/pulls` endpoint, mapping non-2xx → `Api` and transport
/// failures → `Transport`.
/// Test: exercised via the fake in `pr_history_tests`.
pub struct ReqwestPrHistoryTransport {
    http: reqwest::Client,
}

impl ReqwestPrHistoryTransport {
    /// Construct with a default 15s-timeout client.
    ///
    /// Why: bound the worst-case latency of an enrichment call.
    /// What: builds a reqwest client.  Returns `Err(ContextSourceError::Transport)`
    /// if the TLS backend cannot be initialised.
    /// Test: covered transitively by `PrHistorySource::from_config`.
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

#[async_trait]
impl PrHistoryTransport for ReqwestPrHistoryTransport {
    async fn fetch_commits(
        &self,
        token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        per_page: u32,
    ) -> Result<String, ContextSourceError> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/commits");
        let resp = self
            .http
            .get(&url)
            .query(&[("path", path), ("per_page", &per_page.to_string())])
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

    async fn fetch_prs_for_commit(
        &self,
        token: &str,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<String, ContextSourceError> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/commits/{sha}/pulls");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            // groot-preview header required to list PRs for a commit.
            .header("Accept", "application/vnd.github.groot-preview+json")
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

// Disabled sentinels: returned by `from_config` when TLS init fails.
struct DisabledPrHistoryToken;

#[async_trait]
impl PrHistoryTokenResolver for DisabledPrHistoryToken {
    async fn resolve(&self, _owner: &str) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::NotConfigured {
            src: SOURCE_NAME,
            reason: "HTTP transport unavailable (TLS init failed)".to_string(),
        })
    }
}

struct DisabledPrHistoryTransport;

#[async_trait]
impl PrHistoryTransport for DisabledPrHistoryTransport {
    async fn fetch_commits(
        &self,
        _token: &str,
        _owner: &str,
        _repo: &str,
        _path: &str,
        _per_page: u32,
    ) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::Transport {
            src: SOURCE_NAME,
            err: TransportErr("HTTP transport unavailable (TLS init failed)".to_string()),
        })
    }

    async fn fetch_prs_for_commit(
        &self,
        _token: &str,
        _owner: &str,
        _repo: &str,
        _sha: &str,
    ) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::Transport {
            src: SOURCE_NAME,
            err: TransportErr("HTTP transport unavailable (TLS init failed)".to_string()),
        })
    }
}

// ─── JSON shapes ────────────────────────────────────────────────────────────

/// One entry in the commits-per-path response array.
#[derive(Debug, Deserialize)]
struct CommitEntry {
    sha: String,
}

/// One entry in the PRs-for-commit response array.
#[derive(Debug, Deserialize)]
struct PrEntry {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: Option<String>,
}

// ─── The source ──────────────────────────────────────────────────────────────

/// LIVE prior-PR / file change-history context source (T10, #1423).
///
/// Why: surfaces recent PRs that touched the same files so the LLM reviewer
/// can detect repeated patterns, regressions, and related prior work.
/// Default DISABLED: multiple API calls per review; opt in via config or env.
/// What: holds `enabled`, `mode`, the token resolver, and the transport.
/// Test: `pr_history_tests.rs` — dedup, heading, fail-open, disabled skip.
pub struct PrHistorySource {
    enabled: bool,
    mode: RetrievalMode,
    token: Box<dyn PrHistoryTokenResolver>,
    transport: Box<dyn PrHistoryTransport>,
}

impl PrHistorySource {
    /// Build from resolved config + run mode (production constructor).
    ///
    /// Why: default DISABLED (mirrors `ConformanceSource`) — the source makes
    /// multiple GitHub API calls per review and should only run when the
    /// operator explicitly opts in via env or TOML.
    /// What: computes `effective_enabled(false)` so the source is disabled by
    /// default even when GitHub credentials are present.  If the reqwest TLS
    /// backend cannot be initialised the source is constructed in a
    /// permanently-disabled state (graceful degradation, closes #953).
    /// Test: `from_config_default_disabled`, `from_config_respects_explicit_enable`.
    pub fn from_config(cfg: &super::SourceConfig, run_mode: RunMode, config: ReviewConfig) -> Self {
        let transport = match ReqwestPrHistoryTransport::new() {
            Ok(t) => Box::new(t) as Box<dyn PrHistoryTransport>,
            Err(e) => {
                tracing::error!(
                    "pr_history: failed to build HTTP transport (source disabled): {e}"
                );
                return Self {
                    enabled: false,
                    mode: cfg.mode,
                    token: Box::new(DisabledPrHistoryToken),
                    transport: Box::new(DisabledPrHistoryTransport),
                };
            }
        };
        // Default DISABLED: creds_present = false so `effective_enabled(false)`
        // returns false unless the operator has explicitly set enabled = true.
        let enabled = cfg.effective_enabled(false);
        Self {
            enabled,
            mode: cfg.mode,
            token: Box::new(DualModePrHistoryToken::new(run_mode, config)),
            transport,
        }
    }

    /// Construct directly (tests inject fakes).
    ///
    /// Why: drive `gather` without auth or network.
    /// What: stores the provided fields verbatim.
    /// Test: all tests in `pr_history_tests.rs`.
    pub fn new(
        enabled: bool,
        mode: RetrievalMode,
        token: Box<dyn PrHistoryTokenResolver>,
        transport: Box<dyn PrHistoryTransport>,
    ) -> Self {
        Self {
            enabled,
            mode,
            token,
            transport,
        }
    }

    /// Parse a commits-per-path JSON body into a list of SHAs.
    ///
    /// Why: separate parsing from the network call for testability.
    /// What: deserialises the `[{"sha": "…"}, …]` array and returns the SHA
    /// strings.  An empty array or a parse error both yield an empty vec (the
    /// caller should log parse errors before calling this).
    /// Test: `parse_commit_shas`.
    fn parse_commit_shas(body: &str) -> Result<Vec<String>, ContextSourceError> {
        let entries: Vec<CommitEntry> =
            serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
                src: SOURCE_NAME,
                detail: format!("commits response: {e}"),
            })?;
        Ok(entries.into_iter().map(|c| c.sha).collect())
    }

    /// Parse a PRs-for-commit JSON body into `PrEntry` items.
    ///
    /// Why: separate parsing from the network call for testability.
    /// What: deserialises the `[{"number": N, …}, …]` array.
    /// Test: `parse_pr_entries`.
    fn parse_pr_entries(body: &str) -> Result<Vec<PrEntry>, ContextSourceError> {
        serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
            src: SOURCE_NAME,
            detail: format!("PRs-for-commit response: {e}"),
        })
    }

    /// Convert a map of deduped PRs (keyed by number) into a sorted
    /// `ContextSection`.
    ///
    /// Why: after gathering PRs across all files, we need to deduplicate by PR
    /// number, take the top `MAX_PRS` by descending number (most recent first),
    /// and map them to `ContextSnippet`s — keeping this logic a pure function
    /// makes it testable without fakes.
    /// What: sorts the iterator by `number` descending, takes `MAX_PRS`, maps
    /// each entry to a snippet with title, subtitle, body excerpt, and link.
    /// Test: `render_deduped_prs_section`.
    fn build_section(prs: HashMap<u64, PrEntry>) -> ContextSection {
        let mut entries: Vec<PrEntry> = prs.into_values().collect();
        // Most-recent-first (highest PR number first).
        entries.sort_by_key(|e| std::cmp::Reverse(e.number));
        entries.truncate(MAX_PRS);

        let snippets = entries
            .into_iter()
            .map(|pr| {
                let title = if pr.title.is_empty() {
                    format!("PR #{}", pr.number)
                } else {
                    format!("PR #{}: {}", pr.number, pr.title)
                };
                let body_excerpt = pr.body.as_deref().map(str::trim).and_then(|b| {
                    (!b.is_empty()).then(|| truncate_on_char_boundary(b, PR_BODY_CHARS).to_string())
                });
                ContextSnippet {
                    title,
                    subtitle: (!pr.state.is_empty()).then(|| pr.state.clone()),
                    body: body_excerpt,
                    link: (!pr.html_url.is_empty()).then(|| pr.html_url.clone()),
                }
            })
            .collect();

        ContextSection {
            heading: "Prior PRs touching changed files".to_string(),
            snippets,
        }
    }
}

#[async_trait]
impl ContextSource for PrHistorySource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn mode(&self) -> RetrievalMode {
        self.mode
    }

    /// Gather prior PRs touching the changed files.
    ///
    /// Why: surfaces historical context so the LLM reviewer can detect
    /// recurring patterns or prior fixes to the same code paths.
    /// What: for each changed file (capped at `MAX_FILES_TO_QUERY`), fetches
    /// recent commits touching that file, then for each commit fetches its
    /// associated PRs (groot-preview endpoint).  All PRs are deduped by number;
    /// the top `MAX_PRS` most-recent are rendered into a section.  Per-file API
    /// errors are logged and skipped (fail-open within the gather).
    /// Test: `gather_deduplicates_prs`, `gather_fail_open_on_api_error`,
    /// `gather_returns_empty_section_for_no_signal`.
    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        if self.mode == RetrievalMode::Semantic {
            return Err(ContextSourceError::SemanticNotImplemented { src: SOURCE_NAME });
        }
        // No owner/repo → local-diff mode; return empty section.
        if subject.owner.is_empty() || subject.repo.is_empty() {
            return Ok(ContextSection {
                heading: "Prior PRs touching changed files".to_string(),
                snippets: Vec::new(),
            });
        }
        // Resolve the token once; fail-open if unavailable.
        let token = self.token.resolve(&subject.owner).await?;

        let files_to_query: Vec<&String> = subject
            .changed_files
            .iter()
            .take(MAX_FILES_TO_QUERY)
            .collect();

        if files_to_query.is_empty() {
            return Ok(ContextSection {
                heading: "Prior PRs touching changed files".to_string(),
                snippets: Vec::new(),
            });
        }

        // Accumulate deduped PRs: keyed by PR number.
        let mut seen: HashMap<u64, PrEntry> = HashMap::new();

        for file in files_to_query {
            // Fetch commits touching this file.
            let commits_body = match self
                .transport
                .fetch_commits(
                    &token,
                    &subject.owner,
                    &subject.repo,
                    file,
                    COMMITS_PER_FILE,
                )
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        source = SOURCE_NAME,
                        file = file.as_str(),
                        "failed to fetch commits for file (skipping): {e}"
                    );
                    continue;
                }
            };

            let shas = match Self::parse_commit_shas(&commits_body) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        source = SOURCE_NAME,
                        file = file.as_str(),
                        "failed to parse commits response (skipping): {e}"
                    );
                    continue;
                }
            };

            debug!(
                source = SOURCE_NAME,
                file = file.as_str(),
                sha_count = shas.len(),
                "fetched commits for file"
            );

            // For each commit, look up associated PRs.
            for sha in &shas {
                // Stop early if we already have enough PRs.
                if seen.len() >= MAX_PRS * 2 {
                    break;
                }

                let prs_body = match self
                    .transport
                    .fetch_prs_for_commit(&token, &subject.owner, &subject.repo, sha)
                    .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(
                            source = SOURCE_NAME,
                            sha = sha.as_str(),
                            "failed to fetch PRs for commit (skipping): {e}"
                        );
                        continue;
                    }
                };

                let prs = match Self::parse_pr_entries(&prs_body) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(
                            source = SOURCE_NAME,
                            sha = sha.as_str(),
                            "failed to parse PRs-for-commit response (skipping): {e}"
                        );
                        continue;
                    }
                };

                for pr in prs {
                    // Dedup by PR number.
                    seen.entry(pr.number).or_insert(pr);
                }
            }
        }

        Ok(Self::build_section(seen))
    }
}

#[cfg(test)]
#[path = "pr_history_tests.rs"]
mod tests;
