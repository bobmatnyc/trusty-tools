//! RPC handlers for diff review and GitHub PR review.
//!
//! Why: Extracted from `service/mod.rs` to keep the review surface isolated.
//! These handlers share a common theme: they accept external content (a diff or
//! a PR number), run deterministic analysis against it, and optionally post
//! results back to GitHub. The LLM narrative pass lives in `handlers/deep.rs` to
//! keep this file under the 500-line cap.
//!
//! What: two public handlers, `review_diff_handler` and
//! `review_github_pr_handler`.
//!
//! #5181 removed `github_webhook_handler` and its `POST /webhooks/github` route.
//! GitHub now reaches this crate only through `trusty-console`'s
//! `/api/webhooks/{source}` and the UDS listener in `webhook_listener`; the
//! pipeline those deliveries run is `webhook_drain::run_pr_analysis`, which this
//! module used to call.
//!
//! Test: `rpc_review_requires_a_non_empty_index_id`,
//! `rpc_review_reports_an_unreachable_search_daemon`,
//! `rpc_review_rejects_a_malformed_diff` in `service/rpc_tests.rs`.

use serde::Deserialize;

use crate::service::events::{AnalyzerAppState, ApiError};

/// Params for `analyze.review`.
///
/// Why (#6287): the diff used to be the raw `POST /review` body with the index
/// in the query string. A JSON-RPC frame carries one `params` object, so both
/// arrive as fields — and the diff is a JSON string, which is what removes the
/// UTF-8 validity check the byte body needed.
/// What: `index_id` names the corpus to cross-reference; `diff` is the unified
/// diff text.
#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    /// Index ID to cross-reference the diff against in trusty-search. Required:
    /// review pulls the index's chunk corpus so the report reflects already-
    /// computed complexity for the touched files.
    pub index_id: String,
    /// The unified diff to review.
    pub diff: String,
}

/// Why: PR review is most valuable before code lands; this method lets CI
/// and tooling hand over a raw unified diff and get a structured quality
/// report. Like every other analysis method, review is backed by trusty-search
/// — it fetches the named index's chunk corpus so the report can surface
/// trusty-search's already-computed complexity for the files the diff touches.
/// What: requires a non-empty `index_id`, fetches the index corpus via the
/// shared `TrustySearchClient`, runs `analyze_diff_with_client`, and returns
/// the `ReviewReport`. Deliberately deterministic and LLM-free — opt into the
/// LLM narrative via `analyze.deep_analysis`.
/// Test: `rpc_review_requires_a_non_empty_index_id`,
/// `rpc_review_rejects_a_malformed_diff`.
pub async fn review_diff_handler(
    state: &AnalyzerAppState,
    req: ReviewRequest,
) -> Result<crate::core::ReviewReport, ApiError> {
    // #6287: an empty string decodes fine but names no corpus, so the check the
    // `Option<String>` query param used to make is still owed.
    if req.index_id.is_empty() {
        return Err(ApiError::bad_request("missing required 'index_id' field"));
    }
    crate::core::analyze_diff_with_client(&req.diff, &state.search, &req.index_id)
        .await
        .map_err(|e| match e {
            crate::core::ReviewError::MalformedHunkHeader(_) => {
                ApiError::bad_request(format!("invalid diff: {e}"))
            }
            crate::core::ReviewError::Search(_) => ApiError::bad_gateway(format!("{e}")),
        })
}

/// Why: lets CI and tooling analyze a GitHub PR by number without having to
/// fetch the diff themselves — the daemon fetches it, runs the review, and
/// optionally posts a comment back.
/// What: reads `GITHUB_TOKEN` from the environment (400 if absent), fetches the
/// PR's unified diff from the GitHub API, runs `analyze_diff_with_client`
/// against the request's `index_id`, posts a markdown comment when
/// `post_comment` is true, and returns the `ReviewReport` JSON.
/// Test: `rpc_github_pr_requires_a_token` checks the missing-token path.
pub async fn review_github_pr_handler(
    state: &AnalyzerAppState,
    req: crate::core::GithubPrRequest,
) -> Result<crate::core::ReviewReport, ApiError> {
    let token = std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN).map_err(|_| {
        ApiError::bad_request("GITHUB_TOKEN environment variable is not set on the daemon")
    })?;
    // Why: GitHub API calls can take several seconds on large diffs; without
    // timeouts the handler task hangs indefinitely, and the socket read timeout
    // does not cover a handler (`RpcServeOptions::read_timeout` bounds the
    // request read only), so nothing else would ever cut it off.
    // What: 30 s per-request + 5 s connect timeout, matching the pattern used
    // by `TrustySearchClient` in `src/core/client.rs`.
    // Test: `rpc_github_pr_requires_a_token` exercises this code path.
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest ClientBuilder is infallible with valid config");
    let diff = crate::core::fetch_pr_diff(&client, &req.owner, &req.repo, req.pr, &token)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("fetch PR diff: {e}")))?;
    let report = crate::core::analyze_diff_with_client(&diff, &state.search, &req.index_id)
        .await
        .map_err(|e| match e {
            crate::core::ReviewError::MalformedHunkHeader(_) => {
                ApiError::bad_request(format!("invalid diff: {e}"))
            }
            crate::core::ReviewError::Search(_) => ApiError::bad_gateway(format!("{e}")),
        })?;
    if req.post_comment {
        let markdown = crate::core::format_review_as_markdown(&report);
        crate::core::post_pr_comment(&client, &req.owner, &req.repo, req.pr, &markdown, &token)
            .await
            .map_err(|e| ApiError::bad_gateway(format!("post PR comment: {e}")))?;
    }
    Ok(report)
}
