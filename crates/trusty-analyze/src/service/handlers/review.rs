//! Route handlers for diff review and GitHub PR review.
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
//! Test: `review_endpoint_requires_index_id`, `review_endpoint_surfaces_search_failure_as_502`,
//! `review_endpoint_rejects_malformed_diff`, and
//! `retired_webhook_route_is_not_registered` in `service/tests_review.rs`.

use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Bytes,
    extract::{Query, State},
    response::Json,
};
use serde::Deserialize;

use crate::service::events::{AnalyzerAppState, ApiError};

#[derive(Deserialize)]
pub struct ReviewQueryParams {
    /// Index ID to cross-reference the diff against in trusty-search. Required:
    /// review pulls the index's chunk corpus so the report reflects already-
    /// computed complexity for the touched files.
    pub index_id: Option<String>,
}

/// Why: PR review is most valuable before code lands; this endpoint lets CI
/// and tooling POST a raw unified diff and get a structured quality report.
/// Like every other analysis route, `/review` is backed by trusty-search — it
/// fetches the named index's chunk corpus so the report can surface
/// trusty-search's already-computed complexity for the files the diff touches.
/// What: reads the request body as a unified diff (`text/x-patch`), requires a
/// `?index_id=` query param (400 if missing), fetches the index corpus via the
/// shared `TrustySearchClient`, runs `analyze_diff_with_client`, and returns
/// the `ReviewReport` as JSON. This endpoint is deliberately deterministic and
/// LLM-free — opt into the LLM narrative via `POST /analyze/deep`.
/// Test: `review_endpoint_requires_index_id` checks the 400 path;
/// `review_endpoint_rejects_malformed_diff` checks malformed-diff handling.
pub async fn review_diff_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Query(params): Query<ReviewQueryParams>,
    body: Bytes,
) -> Result<Json<crate::core::ReviewReport>, ApiError> {
    let index_id = params
        .index_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing required 'index_id' query parameter"))?;
    let diff = std::str::from_utf8(&body)
        .map_err(|e| ApiError::bad_request(format!("diff body is not valid UTF-8: {e}")))?;
    let report = crate::core::analyze_diff_with_client(diff, &state.search, index_id)
        .await
        .map_err(|e| match e {
            crate::core::ReviewError::MalformedHunkHeader(_) => {
                ApiError::bad_request(format!("invalid diff: {e}"))
            }
            crate::core::ReviewError::Search(_) => ApiError::bad_gateway(format!("{e}")),
        })?;
    Ok(Json(report))
}

/// Why: lets CI and tooling analyze a GitHub PR by number without having to
/// fetch the diff themselves — the daemon fetches it, runs the review, and
/// optionally posts a comment back.
/// What: reads `GITHUB_TOKEN` from the environment (400 if absent), fetches the
/// PR's unified diff from the GitHub API, runs `analyze_diff_with_client`
/// against the request's `index_id`, posts a markdown comment when
/// `post_comment` is true, and returns the `ReviewReport` JSON.
/// Test: `github_pr_endpoint_requires_token` checks the missing-token 400 path.
pub async fn review_github_pr_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<crate::core::GithubPrRequest>,
) -> Result<Json<crate::core::ReviewReport>, ApiError> {
    let token = std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN).map_err(|_| {
        ApiError::bad_request("GITHUB_TOKEN environment variable is not set on the daemon")
    })?;
    // Why: GitHub API calls can take several seconds on large diffs; without
    // timeouts the handler thread hangs indefinitely, exhausting the axum
    // worker pool under concurrent PR review requests.
    // What: 30 s per-request + 5 s connect timeout, matching the pattern used
    // by `TrustySearchClient` in `src/core/client.rs`.
    // Test: `github_pr_endpoint_requires_token` exercises this code path.
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
    Ok(Json(report))
}
