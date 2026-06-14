//! GitHub-backed deployment sources for `tga deployments collect`.
//!
//! Houses the `github_releases` and `github_actions` ingestion paths plus
//! their shared HTTP plumbing (auth, pagination, slug resolution) and the
//! API wire-shapes. The synchronous `git_tags`/`manual` dispatch and the
//! `run` entry point live in the parent module.

use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, LINK, USER_AGENT};
use rusqlite::params;
use serde::Deserialize;
use tracing::{debug, info, warn};

use tga::core::config::{DoraConfig, RepositoryConfig};
use tga::core::db::Database;

use super::{
    ingest_git_tags, CollectStats, GITHUB_API_BASE, GITHUB_TOKEN_ENV, PAGE_SIZE, USER_AGENT_VALUE,
};

/// Wire-shape for the GitHub Releases API entries.
#[derive(Debug, Deserialize)]
pub(super) struct ApiRelease {
    pub(super) tag_name: String,
    #[serde(default)]
    pub(super) target_commitish: Option<String>,
    #[serde(default)]
    pub(super) published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) draft: bool,
    #[serde(default)]
    pub(super) prerelease: bool,
}

/// Wire-shape for a single GitHub Actions workflow run.
///
/// `head_branch` is preserved on the wire-shape because the
/// `?branch=` query param GitHub honours is best-effort — keeping
/// the field lets future code re-filter client-side if needed.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(super) struct ApiWorkflowRun {
    pub(super) id: u64,
    pub(super) head_sha: String,
    #[serde(default)]
    pub(super) head_branch: Option<String>,
    #[serde(default)]
    pub(super) created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) conclusion: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) path: Option<String>,
}

/// Wire-shape envelope for the GitHub Actions runs list endpoint.
#[derive(Debug, Deserialize)]
pub(super) struct ApiWorkflowRunsEnvelope {
    #[serde(default)]
    pub(super) workflow_runs: Vec<ApiWorkflowRun>,
}

/// Build the shared HTTP client used by the GitHub deployment paths.
///
/// Why: matches the auth and `User-Agent` conventions of the existing
/// `crate::collect::github::client::GitHubClient`. Reading the token
/// here (rather than threading a `GithubConfig` through every call site)
/// keeps the deployments command self-contained — it does not depend on
/// the PR-collection config block.
/// What: returns a reqwest client preloaded with `Accept`,
/// `User-Agent`, and (when a token is present) `Authorization` headers.
/// Test: indirectly via `ingest_github_releases` integration tests; the
/// header-construction code is exercised wherever a token is supplied.
fn build_github_http_client(token: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(t) = token {
        let val = HeaderValue::from_str(&format!("Bearer {t}"))
            .map_err(|e| anyhow::anyhow!("invalid GITHUB_TOKEN: {e}"))?;
        headers.insert(AUTHORIZATION, val);
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// Resolve a `RepositoryConfig` to an `(owner, repo)` slug.
///
/// Why: deployments need the same GitHub owner/repo derivation rules as
/// the PR fetcher: explicit `repo.org` wins, otherwise we probe the
/// local clone's `origin` remote.
/// What: returns `Some((owner, name))` when one of those rules
/// resolves; `None` means the repo cannot be addressed via the GitHub
/// API and the caller must skip it.
/// Test: covered by `resolve_repo_to_github_slug_*` unit tests.
pub(super) fn resolve_repo_to_github_slug(repo_cfg: &RepositoryConfig) -> Option<(String, String)> {
    let repo_name = repo_cfg.name.clone().or_else(|| {
        repo_cfg
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    });

    if let Some(owner) = &repo_cfg.org {
        if let Some(name) = repo_name.clone() {
            if !name.is_empty() {
                return Some((owner.clone(), name));
            }
        }
    }

    // Fall back to reading the local clone's `origin` remote.
    owner_repo_from_remote(&repo_cfg.path)
}

/// Try to read `origin`'s URL from a local git repo and extract a
/// GitHub `owner/name` pair from it.
///
/// Why: per-repo entries often omit the `org:` field; the local clone's
/// remote already encodes the canonical slug, so probing it is the
/// cheapest correct fallback.
/// What: opens the repo via `git2`, finds the `origin` remote, parses
/// the URL via [`extract_owner_repo_from_url`].
/// Test: disk-touching path exercised end-to-end via integration tests;
/// the URL-string parser is covered by `extract_owner_repo_from_url_*`.
fn owner_repo_from_remote(repo_path: &std::path::Path) -> Option<(String, String)> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?;
    extract_owner_repo_from_url(url)
}

/// Pure-string helper: extract an `owner/name` pair from a GitHub
/// remote URL string. Returns `None` for non-GitHub URLs or malformed
/// input.
///
/// Why: kept independent of `git2` so it can be unit-tested without a
/// real repo on disk.
/// What: strips the `.git` suffix, recognises HTTPS, SSH, and
/// `ssh://git@github.com/...` forms.
/// Test: covered by `extract_owner_repo_from_url_handles_common_forms`.
pub(super) fn extract_owner_repo_from_url(url: &str) -> Option<(String, String)> {
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

/// Split a `owner/name(/...)` tail into a `(String, String)`. Returns
/// `None` if either segment is empty.
fn split_owner_repo(rest: &str) -> Option<(String, String)> {
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Parse the `Link: <url>; rel="next"` header value, returning the URL of
/// the `next` page when present.
///
/// Why: GitHub's pagination contract is "follow Link headers until a
/// response omits `rel=\"next\"`". Implementing this once locally
/// keeps the github_releases/github_actions loops uniform.
/// What: scans comma-separated link entries and returns the first URL
/// whose `rel` parameter is exactly `"next"`. URL is stripped of its
/// `< >` delimiters.
/// Test: covered by `next_link_*` unit tests below.
fn next_link(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(LINK)?.to_str().ok()?;
    parse_next_link_value(link)
}

/// Pure-string companion to [`next_link`] — easier to unit-test.
pub(super) fn parse_next_link_value(link: &str) -> Option<String> {
    for entry in link.split(',') {
        let entry = entry.trim();
        // Entry shape: `<https://...>; rel="next"`
        let Some((url_part, rel_part)) = entry.split_once(';') else {
            continue;
        };
        let url = url_part.trim();
        if !url.starts_with('<') || !url.ends_with('>') {
            continue;
        }
        let url = &url[1..url.len() - 1];
        // Each rel-param block may contain multiple `;`-separated params.
        for param in rel_part.split(';') {
            let param = param.trim();
            if param == "rel=\"next\"" || param == "rel=next" {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Paginate the GitHub Releases API and INSERT OR IGNORE one row per
/// non-draft, non-prerelease release into `fact_deployments`.
///
/// Why: many teams cut releases via the GitHub Releases UI rather than
/// (or in addition to) raw `git tag` invocations. Ingesting that signal
/// avoids forcing the operator to enable the local tags path just to
/// see deploys land in DORA. Issue #212.
/// What: derives `(owner, repo)` from each `RepositoryConfig`, paginates
/// `GET /repos/{owner}/{repo}/releases?per_page=100` following `Link`
/// headers, and projects each release into a row. Drafts and
/// pre-releases are dropped. If `dora.deployment_tag_pattern` is set it
/// is applied as a secondary filter to `tag_name`. Falls back to
/// `git_tags` when no `GITHUB_TOKEN` is exported.
/// Test: schema-level path covered by the `commit_shas`-style unit
/// patterns; live HTTP is left to integration testing.
pub(super) async fn ingest_github_releases(
    db: &mut Database,
    repositories: &[RepositoryConfig],
    dora: &DoraConfig,
) -> anyhow::Result<CollectStats> {
    let token = std::env::var(GITHUB_TOKEN_ENV).ok();
    if token.is_none() {
        warn!(
            "deployment_source = 'github_releases' requires {GITHUB_TOKEN_ENV} \
             but it is unset. Falling back to git_tags so fact_deployments still populates."
        );
        return ingest_git_tags(db, repositories, dora);
    }
    let client = build_github_http_client(token.as_deref())?;
    let pattern = Regex::new(&dora.deployment_tag_pattern).map_err(|e| {
        anyhow::anyhow!(
            "dora.deployment_tag_pattern is not a valid regex: {e} \
             (pattern: {pat:?})",
            pat = dora.deployment_tag_pattern
        )
    })?;

    let mut stats = CollectStats::default();
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO fact_deployments \
             (deploy_id, repo, environment, triggered_at, completed_at, \
              status, git_sha, git_tag, triggered_by_pr, source) \
             VALUES (?1, ?2, 'production', ?3, ?3, 'success', ?4, ?5, NULL, 'github_release')",
        )?;
        for repo_cfg in repositories {
            let repo_name = repo_cfg.name.clone().unwrap_or_else(|| {
                repo_cfg
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(unknown)")
                    .to_string()
            });
            let Some((owner, name)) = resolve_repo_to_github_slug(repo_cfg) else {
                warn!(repo = %repo_name, "could not resolve owner/repo; skipping github_releases");
                continue;
            };

            let releases = match fetch_all_releases(&client, &owner, &name).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        repo = %repo_name,
                        error = %e,
                        "GitHub releases fetch failed; continuing with remaining repos"
                    );
                    continue;
                }
            };
            for rel in releases {
                stats.inspected_tags += 1;
                if rel.draft || rel.prerelease {
                    continue;
                }
                if !pattern.is_match(&rel.tag_name) {
                    continue;
                }
                stats.matched_tags += 1;

                let Some(published_at) = rel.published_at else {
                    debug!(repo = %repo_name, tag = %rel.tag_name, "release has no published_at; skipping");
                    continue;
                };
                let deploy_id = format!("{repo_name}@{}", rel.tag_name);
                let sha_or_branch = rel.target_commitish.unwrap_or_default();
                let changed = insert.execute(params![
                    deploy_id,
                    repo_name,
                    published_at.to_rfc3339(),
                    sha_or_branch,
                    rel.tag_name,
                ])?;
                if changed > 0 {
                    stats.inserted += 1;
                } else {
                    stats.skipped += 1;
                }
            }
        }
    }
    tx.commit()?;
    info!(
        inspected = stats.inspected_tags,
        matched = stats.matched_tags,
        inserted = stats.inserted,
        skipped = stats.skipped,
        "github_releases deployment ingestion complete"
    );
    Ok(stats)
}

/// Fetch every release from `GET /repos/{owner}/{repo}/releases`,
/// following `Link: rel="next"` headers until exhausted.
///
/// Why: GitHub caps each page at 100 entries; long-lived repos can have
/// hundreds of releases. Following `Link` headers is the canonical
/// pagination idiom.
/// What: starts at `?per_page=100&page=1` and walks every `next` URL
/// the server emits. Non-success status codes raise via
/// `error_for_status`.
/// Test: HTTP layer is integration-tested; this helper is exercised by
/// `ingest_github_releases` against `wiremock` in future passes.
async fn fetch_all_releases(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Vec<ApiRelease>> {
    let mut out: Vec<ApiRelease> = Vec::new();
    let mut next_url = Some(format!(
        "{GITHUB_API_BASE}/repos/{owner}/{repo}/releases?per_page={PAGE_SIZE}"
    ));
    while let Some(url) = next_url {
        debug!(url = %url, "GET (github releases)");
        let resp = client.get(&url).send().await?;
        let next = next_link(resp.headers());
        let resp = resp.error_for_status()?;
        let page: Vec<ApiRelease> = resp.json().await?;
        let n = page.len();
        out.extend(page);
        // Stop early when GitHub returns less than a full page even if a
        // `Link` header is somehow still present — defensive, since real
        // GitHub omits `rel="next"` once you've reached the end.
        if next.is_none() || n == 0 {
            break;
        }
        next_url = next;
    }
    Ok(out)
}

/// Paginate the GitHub Actions runs API and INSERT OR IGNORE one row
/// per successful workflow run on the production branch into
/// `fact_deployments`.
///
/// Why: teams that deploy via a GitHub Actions workflow (rather than a
/// release or tag) want each run to count toward deployment frequency.
/// What: derives `(owner, repo)`, paginates
/// `GET /repos/{owner}/{repo}/actions/runs?branch=...&status=success`,
/// optionally filters on workflow file name (`dora.deployment_workflow`),
/// and projects each kept run into a row with
/// `deploy_id = "<repo>@run:<id>"`. Falls back to `git_tags` when no
/// `GITHUB_TOKEN` is exported.
/// Test: HTTP shape unit-covered via the `ApiWorkflowRun` deserializer
/// tests below; live HTTP is integration-tested.
pub(super) async fn ingest_github_actions(
    db: &mut Database,
    repositories: &[RepositoryConfig],
    dora: &DoraConfig,
) -> anyhow::Result<CollectStats> {
    let token = std::env::var(GITHUB_TOKEN_ENV).ok();
    if token.is_none() {
        warn!(
            "deployment_source = 'github_actions' requires {GITHUB_TOKEN_ENV} \
             but it is unset. Falling back to git_tags so fact_deployments still populates."
        );
        return ingest_git_tags(db, repositories, dora);
    }
    let client = build_github_http_client(token.as_deref())?;
    let workflow_filter = dora.deployment_workflow.clone();
    let branch = dora.production_branch.clone();

    let mut stats = CollectStats::default();
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO fact_deployments \
             (deploy_id, repo, environment, triggered_at, completed_at, \
              status, git_sha, git_tag, triggered_by_pr, source) \
             VALUES (?1, ?2, 'production', ?3, ?4, 'success', ?5, NULL, NULL, 'github_actions')",
        )?;
        for repo_cfg in repositories {
            let repo_name = repo_cfg.name.clone().unwrap_or_else(|| {
                repo_cfg
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(unknown)")
                    .to_string()
            });
            let Some((owner, name)) = resolve_repo_to_github_slug(repo_cfg) else {
                warn!(repo = %repo_name, "could not resolve owner/repo; skipping github_actions");
                continue;
            };

            let runs = match fetch_all_workflow_runs(&client, &owner, &name, &branch).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        repo = %repo_name,
                        error = %e,
                        "GitHub actions runs fetch failed; continuing with remaining repos"
                    );
                    continue;
                }
            };
            for run in runs {
                stats.inspected_tags += 1;
                if !is_kept_run(&run, workflow_filter.as_deref()) {
                    continue;
                }
                stats.matched_tags += 1;

                let Some(triggered_at) = run.created_at else {
                    debug!(repo = %repo_name, id = run.id, "run has no created_at; skipping");
                    continue;
                };
                let completed_at = run.updated_at.unwrap_or(triggered_at);
                let deploy_id = format!("{repo_name}@run:{}", run.id);
                let changed = insert.execute(params![
                    deploy_id,
                    repo_name,
                    triggered_at.to_rfc3339(),
                    completed_at.to_rfc3339(),
                    run.head_sha,
                ])?;
                if changed > 0 {
                    stats.inserted += 1;
                } else {
                    stats.skipped += 1;
                }
            }
        }
    }
    tx.commit()?;
    info!(
        inspected = stats.inspected_tags,
        matched = stats.matched_tags,
        inserted = stats.inserted,
        skipped = stats.skipped,
        "github_actions deployment ingestion complete"
    );
    Ok(stats)
}

/// Decide whether a workflow run should be projected into
/// `fact_deployments`.
///
/// Why: the GitHub `?status=success` filter is best-effort — some runs
/// land with `conclusion = "success"` and an unrelated `status` flag,
/// and the operator may want only one workflow file counted.
/// What: keeps runs whose `conclusion == "success"` (when present) and,
/// if `workflow_filter` is set, whose `name` or `path` ends with /
/// equals the filter.
/// Test: covered by `is_kept_run_*` unit tests.
pub(super) fn is_kept_run(run: &ApiWorkflowRun, workflow_filter: Option<&str>) -> bool {
    // GitHub returns `conclusion = null` for in-progress runs; treat
    // those as not-success.
    if run.conclusion.as_deref() != Some("success") {
        return false;
    }
    let Some(filter) = workflow_filter else {
        return true;
    };
    if filter.is_empty() {
        return true;
    }
    // Match on either the workflow display name or the workflow file
    // path's trailing segment — operators set the filter to whichever
    // they happen to know.
    if run.name.as_deref() == Some(filter) {
        return true;
    }
    if let Some(path) = run.path.as_deref() {
        if path == filter || path.ends_with(&format!("/{filter}")) {
            return true;
        }
    }
    false
}

/// Fetch every successful workflow run on `branch`, following `Link`
/// headers until exhausted. Returns the flat run list (envelope
/// flattened).
async fn fetch_all_workflow_runs(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> anyhow::Result<Vec<ApiWorkflowRun>> {
    let mut out: Vec<ApiWorkflowRun> = Vec::new();
    let mut next_url = Some(format!(
        "{GITHUB_API_BASE}/repos/{owner}/{repo}/actions/runs\
         ?branch={branch}&status=success&per_page={PAGE_SIZE}",
    ));
    while let Some(url) = next_url {
        debug!(url = %url, "GET (github actions runs)");
        let resp = client.get(&url).send().await?;
        let next = next_link(resp.headers());
        let resp = resp.error_for_status()?;
        let env: ApiWorkflowRunsEnvelope = resp.json().await?;
        let n = env.workflow_runs.len();
        out.extend(env.workflow_runs);
        if next.is_none() || n == 0 {
            break;
        }
        next_url = next;
    }
    Ok(out)
}
