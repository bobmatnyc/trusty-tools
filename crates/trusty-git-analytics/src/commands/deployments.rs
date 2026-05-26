//! `tga deployments collect` — ingest deployment events into the canonical
//! `fact_deployments` table (issues #207, #212).
//!
//! Supported sources (configured via `dora.deployment_source`):
//!
//! * `git_tags` — walk every tag in every configured repository, match
//!   against `dora.deployment_tag_pattern`, and emit one row per match.
//!   This is the default because it works without external credentials.
//! * `github_releases` — paginate the GitHub Releases API
//!   (`GET /repos/{owner}/{repo}/releases`), filter out drafts and
//!   pre-releases, and project each release into a `fact_deployments`
//!   row. Requires `GITHUB_TOKEN`; falls back to `git_tags` when absent.
//! * `github_actions` — paginate the GitHub Actions runs API
//!   (`GET /repos/{owner}/{repo}/actions/runs`) restricted to successful
//!   runs on the configured production branch, optionally filtered to a
//!   single workflow name via `dora.deployment_workflow`. Requires
//!   `GITHUB_TOKEN`; falls back to `git_tags` when absent.
//! * `manual` — no-op (operator is expected to INSERT directly).

use chrono::{DateTime, TimeZone, Utc};
use clap::Args;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, LINK, USER_AGENT};
use rusqlite::params;
use serde::Deserialize;
use tracing::{debug, info, warn};

use tga::core::config::{Config, DoraConfig, RepositoryConfig};
use tga::core::db::Database;

/// HTTP `User-Agent` sent on every GitHub API request. Mirrors the value
/// used by `crate::collect::github::client`.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// GitHub REST API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
const PAGE_SIZE: u32 = 100;
/// Environment variable consulted for GitHub bearer auth.
const GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";

/// Arguments for `tga deployments collect`.
#[derive(Args, Debug)]
pub struct DeploymentsCollectArgs {
    /// Override the deployment source from the CLI (defaults to
    /// `dora.deployment_source` or `git_tags` if no DORA config is
    /// present).
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
}

/// Per-run counters surfaced on the CLI output.
#[derive(Debug, Default, Clone)]
struct CollectStats {
    inspected_tags: usize,
    matched_tags: usize,
    inserted: usize,
    skipped: usize,
}

/// Dispatch entry point for `tga deployments collect`.
///
/// Why: a single async entry point lets the github_releases and
/// github_actions paths share the tokio runtime spun up by `#[tokio::main]`
/// in the binary; the synchronous git_tags + manual paths cost nothing
/// extra because they never `.await`.
/// What: dispatches on the resolved source name and prints a summary
/// line so operators can sanity-check ingestion volume.
/// Test: smoke-covered by `ingest_jira_sre_*` and unit tests below;
/// integration coverage lives in repo-level QA passes.
///
/// # Errors
///
/// Propagates git2 / SQL / HTTP errors from the underlying ingestor.
pub async fn run(
    config: Config,
    db: &mut Database,
    args: DeploymentsCollectArgs,
) -> anyhow::Result<()> {
    let dora = config.dora.clone().unwrap_or_default();
    let source = args
        .source
        .clone()
        .unwrap_or_else(|| dora.deployment_source.clone());

    let stats = match source.as_str() {
        "git_tags" => ingest_git_tags(db, &config.repositories, &dora)?,
        "github_releases" => ingest_github_releases(db, &config.repositories, &dora).await?,
        "github_actions" => ingest_github_actions(db, &config.repositories, &dora).await?,
        "manual" => {
            println!(
                "deployment_source = 'manual' — no-op. INSERT into \
                 fact_deployments directly."
            );
            CollectStats::default()
        }
        other => {
            anyhow::bail!(
                "unknown deployment_source '{other}'. Expected one of: \
                 git_tags, github_releases, github_actions, manual."
            );
        }
    };

    println!(
        "Inspected {} tag(s) across {} repo(s); {} matched the deployment pattern; \
         {} inserted into fact_deployments, {} skipped (already present).",
        stats.inspected_tags,
        config.repositories.len(),
        stats.matched_tags,
        stats.inserted,
        stats.skipped,
    );
    Ok(())
}

/// Walk every tag in every configured repository, match against the
/// configured deployment-tag pattern, and INSERT OR IGNORE one row per
/// match into `fact_deployments`.
///
/// Why: git tags are the lowest-common-denominator deployment signal
/// — any project that releases via `git tag vX.Y.Z` already has the
/// data on disk; no external API or token is required.
/// What: opens each repo via git2, iterates `repo.tag_names()`, peels
/// each tag to its commit, and emits a `fact_deployments` row with
/// `source = 'git_tag'`, `git_tag`, `git_sha`, and `triggered_at` set
/// to the tagger's commit time.
/// Test: covered by `ingest_git_tags_*` integration tests.
fn ingest_git_tags(
    db: &mut Database,
    repositories: &[RepositoryConfig],
    dora: &DoraConfig,
) -> anyhow::Result<CollectStats> {
    let mut stats = CollectStats::default();
    let pattern = Regex::new(&dora.deployment_tag_pattern).map_err(|e| {
        anyhow::anyhow!(
            "dora.deployment_tag_pattern is not a valid regex: {e} \
             (pattern: {pat:?})",
            pat = dora.deployment_tag_pattern
        )
    })?;

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO fact_deployments \
             (deploy_id, repo, environment, triggered_at, completed_at, \
              status, git_sha, git_tag, triggered_by_pr, source) \
             VALUES (?1, ?2, 'production', ?3, ?3, 'success', ?4, ?5, NULL, 'git_tag')",
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
            let repo = match git2::Repository::open(&repo_cfg.path) {
                Ok(r) => r,
                Err(e) => {
                    warn!(repo = %repo_name, error = %e, "git open failed; skipping tags");
                    continue;
                }
            };
            let tags = match repo.tag_names(None) {
                Ok(t) => t,
                Err(e) => {
                    warn!(repo = %repo_name, error = %e, "tag_names failed; skipping");
                    continue;
                }
            };
            for tag in tags.iter().flatten() {
                stats.inspected_tags += 1;
                if !pattern.is_match(tag) {
                    continue;
                }
                stats.matched_tags += 1;
                // Peel tag -> commit. Some repos use annotated tags
                // (which wrap a tag object) and some use lightweight
                // tags (which are just a ref to a commit). `peel` resolves
                // both to the final commit.
                let refname = format!("refs/tags/{tag}");
                let obj = match repo.revparse_single(&refname) {
                    Ok(o) => o,
                    Err(e) => {
                        warn!(repo = %repo_name, tag = %tag, error = %e, "revparse failed");
                        continue;
                    }
                };
                let commit = match obj.peel_to_commit() {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(repo = %repo_name, tag = %tag, error = %e, "peel failed");
                        continue;
                    }
                };
                let sha = commit.id().to_string();
                let time = commit.time();
                let triggered_at: DateTime<Utc> = Utc
                    .timestamp_opt(time.seconds(), 0)
                    .single()
                    .unwrap_or_else(Utc::now);

                // deploy_id is "<repo>@<tag>" — stable across re-ingests
                // so INSERT OR IGNORE is idempotent.
                let deploy_id = format!("{repo_name}@{tag}");
                let changed = insert.execute(params![
                    deploy_id,
                    repo_name,
                    triggered_at.to_rfc3339(),
                    sha,
                    tag,
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
        "git-tag deployment ingestion complete"
    );
    Ok(stats)
}

/// Wire-shape for the GitHub Releases API entries.
#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    target_commitish: Option<String>,
    #[serde(default)]
    published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Wire-shape for a single GitHub Actions workflow run.
///
/// `head_branch` is preserved on the wire-shape because the
/// `?branch=` query param GitHub honours is best-effort — keeping
/// the field lets future code re-filter client-side if needed.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiWorkflowRun {
    id: u64,
    head_sha: String,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

/// Wire-shape envelope for the GitHub Actions runs list endpoint.
#[derive(Debug, Deserialize)]
struct ApiWorkflowRunsEnvelope {
    #[serde(default)]
    workflow_runs: Vec<ApiWorkflowRun>,
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
fn resolve_repo_to_github_slug(repo_cfg: &RepositoryConfig) -> Option<(String, String)> {
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
fn extract_owner_repo_from_url(url: &str) -> Option<(String, String)> {
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
fn parse_next_link_value(link: &str) -> Option<String> {
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
async fn ingest_github_releases(
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
async fn ingest_github_actions(
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
fn is_kept_run(run: &ApiWorkflowRun, workflow_filter: Option<&str>) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: smoke check that a malformed `deployment_tag_pattern` is
    /// rejected with a clear error rather than panicking.
    /// What: pass a clearly-invalid regex through and assert the error
    /// names the field.
    /// Test: pure constructor exercise.
    #[test]
    fn bad_deployment_tag_pattern_returns_clear_error() {
        let mut db = Database::open_in_memory().expect("db");
        let dora = DoraConfig {
            deployment_tag_pattern: "[unclosed".into(),
            ..DoraConfig::default()
        };
        let err = ingest_git_tags(&mut db, &[], &dora).expect_err("bad regex");
        let msg = format!("{err}");
        assert!(
            msg.contains("dora.deployment_tag_pattern"),
            "error should name the field: {msg}"
        );
    }

    /// Why: idempotency is the contract for `fact_deployments.deploy_id`
    /// (issue #212) — re-running `tga deployments collect` must not
    /// duplicate rows.
    /// What: directly INSERT OR IGNORE two rows with the same
    /// `deploy_id` and assert the second is a no-op.
    /// Test: pure SQL exercise; the migration runner builds the table.
    #[test]
    fn deploy_id_primary_key_makes_reingest_idempotent() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        for _ in 0..2 {
            conn.execute(
                "INSERT OR IGNORE INTO fact_deployments \
                 (deploy_id, repo, environment, triggered_at, status, git_sha, git_tag, source) \
                 VALUES ('repo@v1.0.0', 'repo', 'production', \
                         '2025-01-01T00:00:00Z', 'success', 'sha', 'v1.0.0', 'git_tag')",
                [],
            )
            .expect("insert");
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_deployments", [], |r| r.get(0))
            .expect("count");
        assert_eq!(n, 1, "INSERT OR IGNORE must dedupe on deploy_id PK");
    }

    /// Why: the URL parser is small but critical for the github_releases
    /// path — wrong owner/repo means we hit the wrong API endpoint.
    /// What: probe each supported URL form and a couple of negatives.
    /// Test: each call returns the expected `(owner, repo)` or `None`.
    #[test]
    fn extract_owner_repo_from_url_handles_common_forms() {
        assert_eq!(
            extract_owner_repo_from_url("https://github.com/acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("https://github.com/acme/widget"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("git@github.com:acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("ssh://git@github.com/acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("https://user@github.com/acme/widget"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert!(extract_owner_repo_from_url("https://gitlab.com/acme/widget").is_none());
        assert!(extract_owner_repo_from_url("nonsense").is_none());
    }

    /// Why: explicit `org:` on a repo config must short-circuit ahead of
    /// the on-disk remote probe — operators set it precisely to override
    /// what's on the clone.
    /// What: build a `RepositoryConfig` with `org=acme` and `name=widget`.
    /// Test: returns `Some(("acme", "widget"))` regardless of path.
    #[test]
    fn resolve_repo_to_github_slug_prefers_explicit_org() {
        let cfg = RepositoryConfig {
            path: std::path::PathBuf::from("/tmp/some-dir"),
            name: Some("widget".into()),
            org: Some("acme".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_repo_to_github_slug(&cfg),
            Some(("acme".to_string(), "widget".to_string()))
        );
    }

    /// Why: when `org:` is unset and the path doesn't point at a real
    /// repo, slug resolution must yield `None` so the caller skips the
    /// repo cleanly.
    /// What: synthetic path with no `org`.
    /// Test: returns `None`.
    #[test]
    fn resolve_repo_to_github_slug_returns_none_when_unresolvable() {
        let cfg = RepositoryConfig {
            path: std::path::PathBuf::from("/nonexistent/path-xyz-987"),
            name: None,
            org: None,
            ..Default::default()
        };
        assert_eq!(resolve_repo_to_github_slug(&cfg), None);
    }

    /// Why: pagination correctness hinges on `Link: rel="next"` parsing;
    /// a bug here either drops pages or loops forever.
    /// What: feed the canonical GitHub `Link` header value and a value
    /// without `rel="next"`.
    /// Test: positive case returns the URL; negative returns `None`.
    #[test]
    fn next_link_parses_canonical_github_header() {
        let h = "<https://api.github.com/repositories/1/releases?page=2>; rel=\"next\", \
                 <https://api.github.com/repositories/1/releases?page=5>; rel=\"last\"";
        assert_eq!(
            parse_next_link_value(h).as_deref(),
            Some("https://api.github.com/repositories/1/releases?page=2"),
        );

        let last_only = "<https://api.github.com/repositories/1/releases?page=5>; rel=\"last\"";
        assert!(parse_next_link_value(last_only).is_none());
    }

    /// Why: the github_releases JSON shape is small but easy to break if
    /// serde drops a `#[serde(default)]`. Lock the deserializer behavior.
    /// What: parse the canonical Releases API payload.
    /// Test: all fields extract; missing `target_commitish` tolerated.
    #[test]
    fn api_release_deserializes_full_and_minimal() {
        let full = r#"{
            "id": 1,
            "tag_name": "v1.2.3",
            "target_commitish": "main",
            "published_at": "2025-01-01T00:00:00Z",
            "draft": false,
            "prerelease": false
        }"#;
        let r: ApiRelease = serde_json::from_str(full).expect("parses");
        assert_eq!(r.tag_name, "v1.2.3");
        assert_eq!(r.target_commitish.as_deref(), Some("main"));
        assert!(!r.draft && !r.prerelease);
        assert!(r.published_at.is_some());

        let minimal = r#"{"tag_name": "v0.1.0"}"#;
        let r: ApiRelease = serde_json::from_str(minimal).expect("parses");
        assert_eq!(r.tag_name, "v0.1.0");
        assert!(r.target_commitish.is_none());
        assert!(r.published_at.is_none());
        assert!(!r.draft && !r.prerelease);
    }

    /// Why: the github_actions JSON envelope nests runs under
    /// `workflow_runs`. A schema drift here drops every run silently.
    /// What: parse a minimal envelope.
    /// Test: run id, head_sha, conclusion all extract.
    #[test]
    fn api_workflow_run_deserializes() {
        let json = r#"{
            "workflow_runs": [
                {
                    "id": 999,
                    "head_sha": "deadbeefcafebabe",
                    "head_branch": "main",
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:05:00Z",
                    "conclusion": "success",
                    "name": "deploy-production",
                    "path": ".github/workflows/deploy-production.yml"
                }
            ]
        }"#;
        let env: ApiWorkflowRunsEnvelope = serde_json::from_str(json).expect("parses");
        assert_eq!(env.workflow_runs.len(), 1);
        let r = &env.workflow_runs[0];
        assert_eq!(r.id, 999);
        assert_eq!(r.head_sha, "deadbeefcafebabe");
        assert_eq!(r.conclusion.as_deref(), Some("success"));
        assert_eq!(r.name.as_deref(), Some("deploy-production"));
    }

    /// Why: `is_kept_run` is the bouncer for `fact_deployments` rows —
    /// wrong predicate means we either inflate deployment counts with
    /// failed runs or silently drop legitimate deploys.
    /// What: probe every combination of conclusion + workflow filter.
    /// Test: success-no-filter keeps; success-matching-name keeps;
    /// success-mismatching-name drops; non-success drops.
    #[test]
    fn is_kept_run_enforces_conclusion_and_workflow_filter() {
        let mut run = ApiWorkflowRun {
            id: 1,
            head_sha: "sha".into(),
            head_branch: Some("main".into()),
            created_at: None,
            updated_at: None,
            conclusion: Some("success".into()),
            name: Some("deploy-production".into()),
            path: Some(".github/workflows/deploy-production.yml".into()),
        };
        assert!(is_kept_run(&run, None));
        assert!(is_kept_run(&run, Some("deploy-production")));
        assert!(is_kept_run(&run, Some("deploy-production.yml")));
        assert!(!is_kept_run(&run, Some("ci.yml")));

        run.conclusion = Some("failure".into());
        assert!(!is_kept_run(&run, None));

        run.conclusion = None;
        assert!(!is_kept_run(&run, None));
    }
}
