//! Route handlers for diff review, GitHub PR review, and webhook delivery.
//!
//! Why: Extracted from `service/mod.rs` to keep the "review + webhook"
//! surface isolated. These handlers share a common theme: they accept external
//! content (a diff or a PR number), run deterministic analysis against it, and
//! optionally post results back to GitHub. The LLM narrative pass lives in
//! `handlers/deep.rs` to keep this file under the 500-line cap.
//!
//! What: Three public handlers (`review_diff_handler`, `review_github_pr_handler`,
//! `github_webhook_handler`) plus their private helper (`process_pr_webhook`).
//!
//! Test: `review_endpoint_requires_index_id`, `review_endpoint_surfaces_search_failure_as_502`,
//! `review_endpoint_rejects_malformed_diff`, and all `webhook_*` tests in
//! `service/tests_review.rs`.

use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
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

/// Why: GitHub can push `pull_request` events to this endpoint so PRs are
/// reviewed automatically the moment they open or update — no CI step needed.
/// The HMAC is the only thing proving a delivery came from GitHub, so an
/// unset secret rejects every delivery rather than trusting the payload
/// (#5173; matches `trusty-review`'s `handle_github_webhook`).
/// What: requires a non-empty secret from app state or `GITHUB_WEBHOOK_SECRET`
/// and verifies `X-Hub-Signature-256` against it (401 on either failure),
/// checks the event is a `pull_request` with an actionable `action`, extracts
/// the PR coordinates, spawns a background task to fetch+analyze+comment, and
/// returns 202 Accepted immediately so GitHub's delivery doesn't time out.
/// Test: `webhook_rejects_when_no_secret_configured` (unset-secret 401),
/// `webhook_rejects_bad_signature` (bad-HMAC 401), and
/// `webhook_ignores_non_pr_event` (202 + no work) cover the guard rails.
pub async fn github_webhook_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    // 1. Signature verification. The secret comes from app state if set,
    //    otherwise from GITHUB_WEBHOOK_SECRET.
    let secret = state
        .webhook_secret
        .clone()
        .or_else(|| std::env::var("GITHUB_WEBHOOK_SECRET").ok())
        .filter(|s| !s.is_empty());
    // #5173: an unset secret rejects — skipping verification let any network
    // peer inject PR coordinates into the analyze pipeline.
    let Some(secret) = secret else {
        tracing::warn!(
            "GITHUB_WEBHOOK_SECRET is not configured — rejecting webhook delivery with 401"
        );
        return Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "webhook secret not configured".to_string(),
        });
    };
    let sig = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::core::verify_webhook_signature(&secret, &body, sig) {
        return Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "X-Hub-Signature-256 verification failed".to_string(),
        });
    }

    // 2-4. Event filter, action filter and PR coordinates all come from the
    // shared classifier so this route and the UDS drain cannot disagree about
    // what a webhook means (#5192).
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let target = match crate::webhook_drain::classify_pr_event(event, &body) {
        crate::webhook_drain::PrEventVerdict::Actionable(target) => target,
        // Acknowledge so GitHub stops retrying, but do no work.
        crate::webhook_drain::PrEventVerdict::Ignored(_) => return Ok(StatusCode::ACCEPTED),
        crate::webhook_drain::PrEventVerdict::Malformed(reason) => {
            return Err(ApiError::bad_request(reason));
        }
    };

    // 5. Spawn the analysis off the request path so GitHub gets a fast 202.
    let search = state.search.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::webhook_drain::run_pr_analysis(&search, &target).await {
            tracing::warn!(
                "github webhook PR {}/{}#{} processing failed: {e:#}",
                target.owner,
                target.repo,
                target.pr
            );
        }
    });

    Ok(StatusCode::ACCEPTED)
}
