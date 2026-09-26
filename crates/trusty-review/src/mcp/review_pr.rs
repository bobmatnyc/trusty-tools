//! The `review_pr` MCP tool and its per-call index resolution (#8649).
//!
//! Why: `review_pr` names its own repo, so it must review against that repo's
//! trusty-search index — not the index the MCP server resolved from its CWD
//! at startup — while keeping the interactive surface's promise that a search
//! outage degrades rather than skips. Split from `tools.rs` for the SLOC cap.
//! What: [`call_review_pr`] parses the arguments and hands
//! [`review_pr_with`] the production runner; `review_pr_with` resolves the
//! [`PrIndex`], authenticates, builds the input, and runs the review under
//! the per-call config.
//! Test: `tools_pr_index_tests.rs`.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tracing::{info, warn};

use crate::{
    config::{
        InvocationSurface, ReviewConfig,
        repo_index::{RepoIndexError, resolve_repo_index},
    },
    integrations::{
        NullAnalyzeClient, NullSearchClient,
        github::{AuthStrategy, GithubClient},
    },
    models::ReviewResult,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, run_review},
    service::AppState,
};

use super::{
    MCP_REVIEW_ALLOW_POSTING, MCP_REVIEW_TRIGGER, ToolError, deps_from_state, mcp_run_mode,
    require_str, wrap_result, wrap_tool_error,
};

/// The search index one `review_pr` call reviews against (#8649).
///
/// Why: the startup index belongs to whatever repo the server was launched
/// in; a registry that cannot be read must degrade to NO index, never to that
/// one.
/// What: `Resolved` carries the PR repo's index id; `DiffOnly` carries the
/// operator-facing notice for a degraded, diff-only review.
/// Test: `two_repos_resolve_to_their_own_indexes_in_one_server`,
/// `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrIndex {
    /// The PR repo's own index.
    Resolved(String),
    /// No index could be resolved; review the diff alone, loudly.
    DiffOnly(String),
}

impl PrIndex {
    /// The per-call config and deps for this index.
    ///
    /// Why: `DiffOnly` must disconnect search AND analyze from any index, as
    /// `ReviewConfig::resolve_source_root`'s diff-only path does, or a healthy
    /// daemon would still be queried against the startup index.
    /// What: `Resolved` clones `base` with `search_index` set and explicit.
    /// `DiffOnly` clears `search_index`, relaxes both `require_*` flags, and
    /// swaps `deps.search`/`deps.analyze` for null clients carrying the notice,
    /// so the context gate returns `Degraded` with that notice as its reason.
    /// Test: `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`.
    pub(crate) fn apply(
        self,
        base: &ReviewConfig,
        mut deps: ReviewDeps,
    ) -> (ReviewConfig, ReviewDeps) {
        let mut config = base.clone();
        config.search_index_explicit = true;
        match self {
            PrIndex::Resolved(index) => config.search_index = index,
            PrIndex::DiffOnly(notice) => {
                config.search_index = String::new();
                config.context.require_search = Some(false);
                config.context.require_analyze = false;
                deps.search = Arc::new(NullSearchClient::new(notice.clone()));
                deps.analyze = Some(Arc::new(NullAnalyzeClient::new(notice)));
            }
        }
        (config, deps)
    }
}

/// Resolve the index `review_pr` uses for `owner/repo`, or the degrade.
///
/// Why: an unreadable registry is a search outage; on the interactive surface
/// that degrades unless the operator required search. An unregistered repo is
/// a configuration fault and never degrades (#6687).
/// What: `resolve_repo_index` against `state.search`, the startup index as
/// the pin. `RepoIndexError::Registry` becomes [`PrIndex::DiffOnly`] (logged at
/// `warn`) when `effective_require_search(Interactive)` is false; every other
/// error, and `Registry` when search is required, is returned.
/// Test: `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`,
/// `registry_failure_is_an_error_when_search_is_required`,
/// `missing_index_error_names_the_repo_and_the_index_id`.
pub(crate) async fn resolve_pr_index(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<PrIndex, RepoIndexError> {
    let pinned = Some(state.config.search_index.as_str());
    match resolve_repo_index(state.search.as_ref(), owner, repo, pinned).await {
        Ok(index) => Ok(PrIndex::Resolved(index)),
        Err(e @ RepoIndexError::Registry { .. })
            if !state
                .config
                .context
                .effective_require_search(InvocationSurface::Interactive) =>
        {
            let notice = format!(
                "{e} — search is not required for this call, so this review is DEGRADED: \
                 diff only, no code context and no static analysis, not the server's startup \
                 index (#8649)"
            );
            warn!("{notice}");
            Ok(PrIndex::DiffOnly(notice))
        }
        Err(e) => Err(e),
    }
}

/// Execute the `review_pr` tool.
///
/// Why: lets Claude Code trigger a full GitHub PR review via MCP without
/// requiring the user to invoke the CLI manually.
/// What: [`review_pr_with`] under the production runner, [`run_resolved`].
/// Test: `call_tool_review_pr_no_token_returns_error`,
/// `call_tool_review_pr_degrades_past_an_unreadable_registry`.
pub(super) async fn call_review_pr(args: &Value, state: &AppState) -> Result<Value, ToolError> {
    review_pr_with(args, state, run_resolved).await
}

/// The production runner: `run_review` under the per-call config it is given.
async fn run_resolved(config: ReviewConfig, input: ReviewInput, deps: ReviewDeps) -> ReviewResult {
    run_review(&config, input, deps).await
}

/// `review_pr` with the review step injected.
///
/// Why: the original #8649 bug was the review running under the server's
/// config; taking the runner as a parameter lets a test see exactly the
/// config and deps the review receives.
/// What: resolves the [`PrIndex`] first — an unresolvable one is an in-band
/// error naming the repo and index id, returned before any GitHub call — then
/// resolves the GitHub token, builds `ReviewDeps` and the `ReviewInput`,
/// applies the index to both, and passes them to `run`.
/// Test: `resolved_config_reaches_the_review_for_each_repo`,
/// `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`.
pub(crate) async fn review_pr_with<R, F>(
    args: &Value,
    state: &AppState,
    run: R,
) -> Result<Value, ToolError>
where
    R: FnOnce(ReviewConfig, ReviewInput, ReviewDeps) -> F,
    F: Future<Output = ReviewResult>,
{
    let owner = require_str(args, "owner")?;
    let repo = require_str(args, "repo")?;
    let pr = args
        .get("pr")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::InvalidParams("missing or non-integer 'pr'".into()))?;

    // #8649: the PR repo's own index, never the server-startup one.
    let index = match resolve_pr_index(state, owner, repo).await {
        Ok(index) => index,
        Err(e) => return Ok(wrap_tool_error(&e.to_string())),
    };

    let reviewer_model = args
        .get("reviewer_model")
        .and_then(Value::as_str)
        .unwrap_or(&state.config.role_models.reviewer.model)
        .to_string();

    // Resolve GitHub token.
    let client = GithubClient::new()
        .map_err(|e| ToolError::InvalidParams(format!("failed to build HTTP client: {e}")))?;
    let token = AuthStrategy::select(mcp_run_mode(&state.config), None)
        .resolve_token(&client, &state.config, owner)
        .await
        .map_err(|e| ToolError::InvalidParams(format!("GitHub auth failed: {e}")))?;

    let diff_source = DiffSource::Github {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr,
        token,
    };

    let deps = deps_from_state(state, &reviewer_model).await?;
    let (config, deps) = index.apply(&state.config, deps);
    let input = ReviewInput {
        diff_source,
        reviewer_model: reviewer_model.clone(),
        write_log: false,
        print_result: false,
        trigger: MCP_REVIEW_TRIGGER,
        run_mode: mcp_run_mode(&state.config),
        allow_posting: MCP_REVIEW_ALLOW_POSTING,
        caller_context: crate::pipeline::runner::CallerContext::default(),
        // Search-unreachable semantics fix: the MCP tool surface can never post
        // to a real PR (`allow_posting: false` above), so a search outage
        // safely defaults to a loud DEGRADED diff-only review instead of a
        // hard-Skip — see `InvocationSurface`. #8649: that includes an outage
        // met while resolving the index (`PrIndex::DiffOnly`).
        surface: InvocationSurface::Interactive,
    };

    info!(owner, repo, pr, reviewer_model, index = %config.search_index, "mcp: review_pr");
    let result = run(config, input, deps).await;
    Ok(wrap_result(&result))
}

// #8649: per-call search-index resolution for `review_pr`.
#[cfg(test)]
#[path = "tools_pr_index_tests.rs"]
mod tests;
