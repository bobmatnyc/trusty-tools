//! GitHub webhook handler for POST /pr/github/webhook.
//!
//! Why: isolates all webhook-specific concerns — HMAC verification, event
//! parsing, action filtering, and async dispatch — from the general handler
//! path.  The handler must acknowledge quickly (returning 202) and execute
//! the review pipeline in a background task (spec REV-705).
//!
//! What: `handle_github_webhook` validates the `X-Hub-Signature-256` header,
//! parses the JSON body for `pull_request` events, and spawns the pipeline
//! as a detached `tokio::task` only for `review_requested` actions.
//! All other event types receive 200 with no dispatch (spec REV-702).
//!
//! Phase 1 (#582) wires in:
//!  - Trigger classification (REV-703): the requested reviewer decides
//!    force-live vs force-dry-run, threaded into the runner.
//!  - In-process in-flight guard (REV-705): a PR-level guard at dispatch and a
//!    SHA-level guard inside the spawned task drop duplicate concurrent runs.
//!  - Durable dedup store + live posting: handled inside the runner.
//!
//! Deferred to later phases:
//!  - Author exclusion (Phase 3 / #584; `PR_INTELLIGENCE_EXCLUDED_AUTHORS`)
//!
//! Test: see `webhook_tests.rs` (split out to keep this file under the cap).

use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{
    integrations::github::{RunMode, webhook::verify_webhook_signature},
    pipeline::{DiffSource, ReviewDeps, ReviewInput, classify_review_request, run_review},
    service::handlers::AppState,
};

// ─── Webhook payload shapes ───────────────────────────────────────────────────

/// GitHub `pull_request` event payload (minimal fields we care about).
///
/// Why: we only need `action`, `pull_request.number`, and
/// `repository.{owner.login, name}` to dispatch the pipeline; all other fields
/// are ignored to keep the parser minimal and forward-compatible.
/// What: serde ignores unknown fields by default.
/// Test: `webhook_payload_deserialises`.
#[derive(Debug, Deserialize)]
pub struct PullRequestEvent {
    /// Webhook action (e.g. `"review_requested"`, `"opened"`, `"closed"`).
    pub action: String,
    /// Pull request information.
    pub pull_request: PrInfo,
    /// Repository information.
    pub repository: RepoInfo,
    /// Requested reviewer (present on `review_requested` action).
    #[serde(default)]
    pub requested_reviewer: Option<ReviewerInfo>,
}

/// Minimal PR metadata extracted from the webhook payload.
///
/// Why: we need the PR number to dispatch the pipeline.
/// What: carries `number` (the PR id) and optional `head.sha` for dedup.
/// Test: covered by `webhook_payload_deserialises`.
#[derive(Debug, Deserialize)]
pub struct PrInfo {
    /// Pull request number.
    pub number: u64,
    /// PR author login (used for author-exclusion filtering).
    pub user: UserInfo,
    /// Head commit info.
    #[serde(default)]
    pub head: Option<HeadInfo>,
}

/// GitHub user info (minimal).
#[derive(Debug, Deserialize)]
pub struct UserInfo {
    /// GitHub login.
    pub login: String,
}

/// Head commit info from a PR.
#[derive(Debug, Deserialize, Default)]
pub struct HeadInfo {
    /// Head commit SHA.
    pub sha: String,
}

/// Repository info (owner + name).
#[derive(Debug, Deserialize)]
pub struct RepoInfo {
    /// Repository name (without owner prefix).
    pub name: String,
    /// Repository owner.
    pub owner: UserInfo,
}

/// Requested reviewer info (present on `review_requested` action).
#[derive(Debug, Deserialize)]
pub struct ReviewerInfo {
    /// Reviewer login.
    pub login: String,
}

/// Acknowledgement response for accepted webhooks.
///
/// Why: GitHub expects a 2xx response quickly; we return 202 with a JSON body
/// confirming receipt so GitHub's delivery log shows something meaningful.
/// What: `{"status":"accepted","pr":<number>}`.
/// Test: `webhook_dispatches_review_requested`.
#[derive(Debug, Serialize)]
struct WebhookAck {
    status: &'static str,
    pr: u64,
}

// ─── Handler ──────────────────────────────────────────────────────────────────

/// POST /pr/github/webhook — validate HMAC, filter events, dispatch pipeline.
///
/// Why: GitHub sends webhook payloads to this endpoint when a pull_request
/// event fires; we must respond within a few seconds so we spawn the review
/// as a background task and return 202 immediately.
/// What: validates the `X-Hub-Signature-256` HMAC against `GITHUB_WEBHOOK_SECRET`,
/// parses the JSON body, dispatches only on `pull_request` action
/// `review_requested`, ignores all other events.  Background task updates
/// `AppState::last_error` on pipeline failure.
/// Test: `webhook_rejects_bad_hmac`, `webhook_ignores_non_review_requested`,
/// `webhook_dispatches_review_requested`.
pub async fn handle_github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // ── Step 1: HMAC validation ───────────────────────────────────────────
    let secret = &state.config.github_webhook_secret;
    if secret.is_empty() {
        warn!("GITHUB_WEBHOOK_SECRET is not set — rejecting all webhook requests");
        return (StatusCode::UNAUTHORIZED, "Webhook secret not configured").into_response();
    }

    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_webhook_signature(secret, &body, signature) {
        warn!("webhook HMAC verification failed");
        return (StatusCode::UNAUTHORIZED, "Invalid signature").into_response();
    }

    // ── Step 2: determine event type ──────────────────────────────────────
    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if event_type != "pull_request" {
        debug!(event_type, "ignoring non pull_request webhook event");
        return (StatusCode::OK, "ignored").into_response();
    }

    // ── Step 3: parse the pull_request event ─────────────────────────────
    let event: PullRequestEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("failed to parse pull_request webhook payload: {e}");
            return (StatusCode::BAD_REQUEST, "invalid JSON payload").into_response();
        }
    };

    // ── Step 4: action filtering — only review_requested dispatches ───────
    // Per spec REV-702: only `review_requested` triggers a pipeline run.
    if event.action != "review_requested" {
        debug!(action = %event.action, pr = event.pull_request.number, "pull_request event ignored (not review_requested)");
        return (StatusCode::OK, "ignored").into_response();
    }

    let pr_number = event.pull_request.number;
    let owner = event.repository.owner.login.clone();
    let repo = event.repository.name.clone();
    let head_sha = event
        .pull_request
        .head
        .as_ref()
        .map(|h| h.sha.clone())
        .unwrap_or_default();

    // ── Step 5: trigger classification (REV-703) ──────────────────────────
    // The requested reviewer decides force-live vs force-dry-run.
    let requested_login = event.requested_reviewer.as_ref().map(|r| r.login.as_str());
    let trigger = classify_review_request(&state.config, requested_login);

    info!(
        pr = pr_number,
        owner = %owner,
        repo = %repo,
        reviewer = ?requested_login,
        trigger = ?trigger,
        "webhook dispatching review for review_requested"
    );

    // ── Step 6: PR-level in-flight guard (REV-705) ────────────────────────
    // Claim the PR slot before spawning so two near-simultaneous deliveries for
    // the same PR do not both run.  The guard is moved into the spawned task so
    // it is held for the lifetime of the review and released (RAII) on completion.
    let pr_guard = match state
        .in_flight_registry
        .try_acquire_pr(&owner, &repo, pr_number)
    {
        Some(g) => g,
        None => {
            debug!(
                pr = pr_number,
                "a review for this PR is already in flight — dropping duplicate delivery"
            );
            return (StatusCode::OK, "already in flight").into_response();
        }
    };

    // Author exclusion (Phase 3 / #584) is intentionally not implemented here.

    // ── Step 7: spawn background review task ─────────────────────────────
    let state_clone = state.clone();
    tokio::spawn(async move {
        // Hold the PR guard for the whole task; it releases on drop.
        let _pr_guard = pr_guard;

        // SHA-level guard: drop a duplicate run for the exact same head commit.
        let _sha_guard = if head_sha.is_empty() {
            None
        } else {
            match state_clone
                .in_flight_registry
                .try_acquire_sha(&owner, &repo, pr_number, &head_sha)
            {
                Some(g) => Some(g),
                None => {
                    debug!(
                        pr = pr_number,
                        head_sha = %head_sha,
                        "a review for this head SHA is already in flight — skipping"
                    );
                    return;
                }
            }
        };

        state_clone
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let deps = ReviewDeps {
            llm: Arc::clone(&state_clone.llm),
            search: Arc::clone(&state_clone.search),
            analyze: state_clone.analyze.clone(),
            dedup: state_clone.dedup.clone(),
        };

        let reviewer_model = state_clone.config.role_models.reviewer.model.clone();
        let input = ReviewInput {
            diff_source: DiffSource::Github {
                owner: owner.clone(),
                repo: repo.clone(),
                pr: pr_number,
                token: String::new(), // resolved from config by the pipeline
            },
            reviewer_model,
            write_log: true, // background webhook tasks write the log
            print_result: false,
            // Service mode: App auth; the trigger decides live vs dry; posting is
            // permitted (the trigger / config still gate whether it actually posts).
            trigger,
            run_mode: RunMode::Serve,
            allow_posting: true,
        };

        let result = run_review(&state_clone.config, input, deps).await;
        state_clone
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

        if let Some(ref err) = result.error {
            warn!(pr = pr_number, error = %err, "webhook pipeline completed with error");
            if let Ok(mut guard) = state_clone.last_error.lock() {
                *guard = Some(err.clone());
            }
        } else {
            info!(
                pr = pr_number,
                verdict = %result.verdict,
                posted = result.posted,
                findings = result.findings.len(),
                "webhook pipeline complete"
            );
        }
    });

    // Respond 202 immediately — the review runs in the background.
    (
        StatusCode::ACCEPTED,
        Json(WebhookAck {
            status: "accepted",
            pr: pr_number,
        }),
    )
        .into_response()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "webhook_tests.rs"]
mod tests;
