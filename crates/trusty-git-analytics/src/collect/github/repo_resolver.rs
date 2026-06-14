//! GitHub repository resolution helpers.
//!
//! Why: decouples the URL-parsing / repo-resolution logic from the HTTP client
//! so each can be tested in isolation without touching the network.
//! What: provides `resolve_github_repos`, `parse_slug`, `build_http_client`,
//! and the URL-extraction helpers used by collection and org-discovery.
//! Test: `resolve_github_repos_*` and `extract_owner_repo_from_url_*` unit
//! tests live in `github/client.rs` (in the `tests` module) because they
//! exercise the full resolution pipeline end-to-end.

use tracing::debug;

use crate::collect::errors::{CollectError, Result};
use crate::collect::github::client::USER_AGENT_VALUE;
use crate::core::config::{GithubConfig, RepositoryConfig};

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};

use crate::collect::env_expand::expand_env_var;

/// Parse an `owner/name` slug, returning a [`CollectError::Config`] on
/// malformed input. Extracted so both [`super::client::GitHubClient::new`] and
/// [`resolve_github_repos`] share one error message format.
///
/// Why: centralises slug parsing so every consumer gets the same error message
/// and validation rules.
/// What: splits on `/`, validates both parts are non-empty.
/// Test: `parse_slug_validates_input` in `client.rs` tests.
pub fn parse_slug(slug: &str) -> Result<(String, String)> {
    let (owner, repo) = slug.split_once('/').ok_or_else(|| {
        CollectError::Config(format!("github repo must be 'owner/name', got '{slug}'"))
    })?;
    if owner.is_empty() || repo.is_empty() {
        return Err(CollectError::Config(format!(
            "github repo must be 'owner/name', got '{slug}'"
        )));
    }
    Ok((owner.to_string(), repo.to_string()))
}

/// Build the shared authenticated `reqwest::Client` for all GitHub HTTP traffic.
///
/// Why: org-discovery, reviewer-ingestion, and the PR client all need the same
/// authed client; `pub(crate)` visibility avoids duplicating header-build logic
/// without widening the public API surface.
/// What: builds a `reqwest::Client` with `Authorization: Bearer <token>` (when
/// a token is configured), the GitHub `Accept` header, and a 30-second timeout.
/// Test: used by all GitHub call sites — covered indirectly by their tests.
pub(crate) fn build_http_client(config: &GithubConfig) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(raw) = &config.token {
        let val = HeaderValue::from_str(&format!("Bearer {}", expand_env_var(raw)))
            .map_err(|e| CollectError::Config(format!("invalid token header: {e}")))?;
        headers.insert(AUTHORIZATION, val);
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// Try to read `origin`'s URL from a local git repository and extract an
/// `owner/name` pair if it looks like a GitHub URL.
///
/// Accepts both `https://github.com/owner/name(.git)?` and
/// `git@github.com:owner/name(.git)?` forms. Returns `None` for non-GitHub
/// remotes, missing `origin`, or anything that fails to parse.
///
/// Why: per-repo entries in `repositories[]` often don't declare an `org:`
/// field; the local clone's remote already encodes the canonical
/// `owner/name`, so probing it is the cheapest correct fallback.
/// What: opens the repo via `git2`, finds the `origin` remote, parses the URL.
/// Test: `extract_owner_repo_from_url` below covers the URL-parse path.
pub fn owner_repo_from_remote(repo_path: &std::path::Path) -> Option<(String, String)> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?;
    extract_owner_repo_from_url(url)
}

/// Pure-string helper: extracts `owner/name` from a GitHub remote URL string.
/// Returns `None` for non-GitHub URLs or malformed input.
///
/// Why: separating the pure URL-parse logic from the disk-touching
/// `owner_repo_from_remote` makes the URL-parse path independently testable.
/// What: handles HTTPS, SSH, and `https://user@` URL forms.
/// Test: `extract_owner_repo_from_url_handles_common_forms` in `client.rs` tests.
pub fn extract_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let cleaned = url.strip_suffix(".git").unwrap_or(url);
    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        return split_owner_repo(rest);
    }
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    ] {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            return split_owner_repo(rest);
        }
    }
    if let Some(after_scheme) = cleaned.strip_prefix("https://") {
        if let Some(at_idx) = after_scheme.find('@') {
            let after_at = &after_scheme[at_idx + 1..];
            if let Some(rest) = after_at.strip_prefix("github.com/") {
                return split_owner_repo(rest);
            }
        }
    }
    None
}

/// Split a `owner/name(/...)` tail into a `(String, String)` pair.
/// Returns `None` if either segment is empty.
fn split_owner_repo(rest: &str) -> Option<(String, String)> {
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Resolve the set of `(owner, repo)` pairs the GitHub PR fetcher should
/// scan, given the GitHub config and the project's repository list.
///
/// Resolution rules, tried in order for each repository:
/// 1. `github.repo` (single-repo mode) — when set, returns a single-entry
///    list and ignores `repositories[]` / `github.org`.
/// 2. For each `RepositoryConfig`:
///    - if `repo.org` is set, derive `owner/name` from `org` + repo name;
///    - else, try `git remote get-url origin` on `repo.path`;
///    - else, fall back to `github.org` as the owner with the repo's name.
/// 3. Deduplicate; preserve first-seen order.
///
/// Why: org-wide deployments (issue #87) need to drive PR collection from
/// `repositories[]` rather than a single `github.repo`. Mirrors the ADO PR
/// fetcher's per-repo expansion strategy.
/// What: walks the three fallback paths above and returns a deduped vec.
/// Test: `resolve_github_repos_*` cases in `client.rs` tests.
pub fn resolve_github_repos(
    github: &GithubConfig,
    repositories: &[RepositoryConfig],
) -> Vec<(String, String)> {
    if let Some(slug) = &github.repo {
        if let Ok(pair) = parse_slug(slug) {
            return vec![pair];
        } else {
            tracing::warn!(slug = %slug, "github.repo is malformed; falling back to repositories[]");
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for repo_cfg in repositories {
        let repo_name = repo_cfg
            .name
            .clone()
            .or_else(|| {
                repo_cfg
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();

        let owner_from_cfg = repo_cfg.org.clone().or_else(|| github.org.clone());

        let pair = if let Some(owner) = &owner_from_cfg {
            if repo_name.is_empty() {
                owner_repo_from_remote(&repo_cfg.path)
            } else {
                Some((owner.clone(), repo_name.clone()))
            }
        } else {
            owner_repo_from_remote(&repo_cfg.path)
        };

        if let Some(p) = pair {
            if seen.insert(p.clone()) {
                out.push(p);
            }
        } else {
            debug!(
                path = %repo_cfg.path.display(),
                "could not resolve owner/repo for repository; skipping for GitHub PR fetch"
            );
        }
    }

    out
}
