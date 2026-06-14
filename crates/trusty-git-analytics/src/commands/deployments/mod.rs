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
use rusqlite::params;
use tracing::{info, warn};

use tga::core::config::{Config, DoraConfig, RepositoryConfig};
use tga::core::db::Database;

mod github;
use github::{ingest_github_actions, ingest_github_releases};
// The inline test module (path = "deployments_tests.rs") exercises several
// GitHub-path helpers via `use super::*`; re-export them under cfg(test).
#[cfg(test)]
use github::{
    extract_owner_repo_from_url, is_kept_run, parse_next_link_value, resolve_repo_to_github_slug,
    ApiRelease, ApiWorkflowRun, ApiWorkflowRunsEnvelope,
};

/// HTTP `User-Agent` sent on every GitHub API request. Mirrors the value
/// used by `crate::collect::github::client`.
pub(super) const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// GitHub REST API base URL.
pub(super) const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
pub(super) const PAGE_SIZE: u32 = 100;
/// Environment variable consulted for GitHub bearer auth.
pub(super) const GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";

/// Arguments for `tga deployments collect`.
#[derive(Args, Debug)]
#[command(
    about = "Ingest deployment events into fact_deployments.",
    long_about = "Walk the configured deployment source and persist deployment events into\n\
`fact_deployments`. Supported sources:\n\n\
  git_tags         -- match tags against dora.deployment_tag_pattern (default)\n\
  github_releases  -- paginate GitHub Releases API (requires GITHUB_TOKEN)\n\
  github_actions   -- paginate GitHub Actions runs (requires GITHUB_TOKEN)\n\
  manual           -- no-op (operator INSERTs directly into fact_deployments)\n\n\
The source is configured via `dora.deployment_source` in config.yaml.\n\
Use --source to override at runtime without editing the config file.",
    after_help = "EXAMPLES:\n\
  # Ingest from the configured source (usually git_tags)\n\
  tga deployments collect\n\n\
  # Force GitHub Releases source regardless of config\n\
  tga deployments collect --source github_releases\n\n\
TIPS:\n\
  - Set GITHUB_TOKEN before using github_releases or github_actions sources.\n\
  - After ingestion, run `tga dora` to compute deployment frequency and lead time."
)]
pub struct DeploymentsCollectArgs {
    /// Override the deployment source from the CLI (defaults to
    /// `dora.deployment_source` or `git_tags` if no DORA config is
    /// present).
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
}

/// Per-run counters surfaced on the CLI output.
#[derive(Debug, Default, Clone)]
pub(super) struct CollectStats {
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
pub(super) fn ingest_git_tags(
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

#[cfg(test)]
#[path = "deployments_tests.rs"]
mod tests;
