//! The PR-metadata and diff seam behind `run_review_with` (#9192).
//!
//! Why: the GitHub path fetched PR metadata and the diff through clients that
//! dial `api.github.com`, so no test could run the real pipeline down that path
//! and prove its prompts byte-identical with every optional input off.
//! What: [`PrSource`] answers both fetches; [`pr_meta_via`] and
//! [`load_diff_via`] call it when one is injected and the production fetch
//! otherwise, so production wiring is unchanged.
//! Test: `off_is_byte_identical_unified`, `off_makes_no_extra_calls`.

use async_trait::async_trait;

use crate::{
    config::ReviewConfig,
    integrations::github::{GithubError, RunMode},
    pipeline::{
        diff::{DiffSource, load_diff},
        prompt::ReviewPrMeta,
        runner_helpers::fetch_github_pr_meta,
    },
};

/// Where the runner reads a GitHub PR's metadata and diff (#9192).
///
/// Why: a test double for the two GitHub reads; production never sets one.
/// What: `meta` returns the PR metadata and head SHA; `diff` returns the
/// unified diff text.
/// Test: `off_is_byte_identical_unified`.
#[async_trait]
pub(crate) trait PrSource: Send + Sync {
    /// PR metadata and the head SHA.
    async fn meta(
        &self,
        config: &ReviewConfig,
        owner: &str,
        repo: &str,
        pr: u64,
        run_mode: RunMode,
    ) -> Result<(ReviewPrMeta, String), GithubError>;

    /// The PR's unified diff.
    async fn diff(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        token: &str,
    ) -> Result<String, GithubError>;
}

/// PR metadata through `source`, or the production fetch when it is `None`.
pub(crate) async fn pr_meta_via(
    source: Option<&dyn PrSource>,
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    pr: u64,
    run_mode: RunMode,
) -> Result<(ReviewPrMeta, String), GithubError> {
    match source {
        Some(src) => src.meta(config, owner, repo, pr, run_mode).await,
        None => fetch_github_pr_meta(config, owner, repo, pr, run_mode).await,
    }
}

/// The diff through `source` for a GitHub PR, or [`load_diff`] otherwise.
pub(crate) async fn load_diff_via(
    source: Option<&dyn PrSource>,
    diff_source: &DiffSource,
) -> Result<String, GithubError> {
    match (source, diff_source) {
        (
            Some(src),
            DiffSource::Github {
                owner,
                repo,
                pr,
                token,
            },
        ) => src.diff(owner, repo, *pr, token).await,
        _ => load_diff(diff_source).await,
    }
}
