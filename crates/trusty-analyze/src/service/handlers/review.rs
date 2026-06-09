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

use crate::core::TrustySearchClient;
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
    let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
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
/// What: verifies the `X-Hub-Signature-256` HMAC against `GITHUB_WEBHOOK_SECRET`
/// (skipped with a warning when the secret is unset), checks the event is a
/// `pull_request` with an actionable `action`, extracts the PR coordinates,
/// spawns a background task to fetch+analyze+comment, and returns 202 Accepted
/// immediately so GitHub's delivery doesn't time out.
/// Test: `webhook_rejects_bad_signature` (401 path) and
/// `webhook_ignores_non_pr_event` (202 + no work) cover the guard rails.
pub async fn github_webhook_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    // 1. Signature verification (when a secret is configured). The secret
    //    comes from app state if set, otherwise from GITHUB_WEBHOOK_SECRET.
    let secret = state
        .webhook_secret
        .clone()
        .or_else(|| std::env::var("GITHUB_WEBHOOK_SECRET").ok())
        .filter(|s| !s.is_empty());
    match secret {
        Some(secret) => {
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
        }
        None => {
            tracing::warn!(
                "no webhook secret configured — skipping webhook signature verification"
            );
        }
    }

    // 2. Only handle pull_request events.
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if event != "pull_request" {
        // Acknowledge so GitHub stops retrying, but do no work.
        return Ok(StatusCode::ACCEPTED);
    }

    // 3. Parse the payload and filter to actionable actions.
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("webhook body is not valid JSON: {e}")))?;
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(action, "opened" | "synchronize" | "reopened") {
        return Ok(StatusCode::ACCEPTED);
    }

    // 4. Extract PR coordinates.
    let pr = payload
        .get("pull_request")
        .and_then(|p| p.get("number"))
        .and_then(|n| n.as_u64());
    let owner = payload
        .get("repository")
        .and_then(|r| r.get("owner"))
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())
        .map(str::to_owned);
    let repo = payload
        .get("repository")
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned);
    let head_sha = payload
        .get("pull_request")
        .and_then(|p| p.get("head"))
        .and_then(|h| h.get("sha"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();

    let (Some(pr), Some(owner), Some(repo)) = (pr, owner, repo) else {
        return Err(ApiError::bad_request(
            "webhook payload missing pull_request.number or repository owner/name",
        ));
    };

    // 5. Spawn the analysis off the request path so GitHub gets a fast 202.
    let search = state.search.clone();
    tokio::spawn(async move {
        if let Err(e) = process_pr_webhook(search, &owner, &repo, pr, &head_sha).await {
            tracing::warn!("github webhook PR {owner}/{repo}#{pr} processing failed: {e:#}");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

/// Background worker for an accepted PR webhook: fetch the diff, run the
/// review, and post a comment.
///
/// Why: keeps the webhook handler's response path fast — all the slow I/O
/// (GitHub API, trusty-search) happens here in a spawned task.
/// What: requires `GITHUB_TOKEN`; uses `repo` itself as the trusty-search
/// index ID (the conventional 1:1 mapping). The `head_sha` is logged as a
/// cache/correlation key.
/// Test: covered indirectly — the webhook handler tests exercise the guard
/// rails; this function is only reached with a valid token + reachable search.
async fn process_pr_webhook(
    search: TrustySearchClient,
    owner: &str,
    repo: &str,
    pr: u64,
    head_sha: &str,
) -> Result<()> {
    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; cannot process webhook PR"))?;
    tracing::info!("processing webhook PR {owner}/{repo}#{pr} (head {head_sha})");
    // Why: this background task fetches a potentially large diff and posts a
    // comment — without timeouts it hangs indefinitely on a slow GitHub API,
    // leaking the spawned task for the lifetime of the process.
    // What: 30 s per-request + 5 s connect timeout, matching the pattern in
    // `review_github_pr_handler` and `TrustySearchClient`.
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest ClientBuilder is infallible with valid config");
    let diff = crate::core::fetch_pr_diff(&client, owner, repo, pr, &token).await?;
    let report = crate::core::analyze_diff_with_client(&diff, &search, repo).await?;
    let markdown = crate::core::format_review_as_markdown(&report);
    crate::core::post_pr_comment(&client, owner, repo, pr, &markdown, &token).await?;
    tracing::info!("posted webhook review comment to {owner}/{repo}#{pr}");
    Ok(())
}
