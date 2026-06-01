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
//! Deferred (MVP scope):
//!  - Author exclusion (spec REV-704 / `PR_INTELLIGENCE_EXCLUDED_AUTHORS`)
//!  - In-process dedup guard (spec REV-705 / REV-101a)
//!  - GitHub comment posting (still dry-run only)
//!
//! Test: `webhook_rejects_bad_hmac`, `webhook_ignores_non_review_requested`,
//! `webhook_dispatches_review_requested`.

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
    integrations::github::webhook::verify_webhook_signature,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, run_review},
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

    info!(
        pr = pr_number,
        owner = %owner,
        repo = %repo,
        reviewer = ?event.requested_reviewer.as_ref().map(|r| &r.login),
        "webhook dispatching review for review_requested"
    );

    // MVP deferred: author exclusion (spec REV-704) and in-process dedup
    // (spec REV-705 / REV-101a) are not implemented.  These will be added
    // in Stage 5 along with comment posting.

    // ── Step 5: spawn background review task ─────────────────────────────
    let state_clone = state.clone();
    tokio::spawn(async move {
        state_clone
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let deps = ReviewDeps {
            llm: Arc::clone(&state_clone.llm),
            search: Arc::clone(&state_clone.search),
            analyze: state_clone.analyze.clone(),
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
mod tests {
    use super::*;
    use crate::{
        integrations::search_client::{
            HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
            SearchResult,
        },
        llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
        service::handlers::AppState,
    };
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use tower::ServiceExt as _;

    // ── Fake LLM ─────────────────────────────────────────────────────────────

    struct FakeLlm;

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake"
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: r#"LGTM.
```json
{"verdict":"APPROVE","summary":"ok","findings":[]}
```"#
                    .to_string(),
                model: req.model.clone(),
                input_tokens: 10,
                output_tokens: 5,
                latency_ms: 1,
                cost_usd: 0.0,
            })
        }
    }

    // ── Fake search ───────────────────────────────────────────────────────────

    struct FakeSearch;

    #[async_trait]
    impl SearchClient for FakeSearch {
        async fn health(&self) -> Result<SearchHealth, SearchClientError> {
            Ok(SearchHealth {
                status: "ok".to_string(),
                embedder: true,
            })
        }

        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Ok(vec![])
        }

        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Ok(vec![])
        }
    }

    // ── HMAC helper ───────────────────────────────────────────────────────────

    fn make_sig(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn test_state_with_secret(secret: &str) -> AppState {
        let mut config = crate::config::ReviewConfig::load(None);
        config.github_webhook_secret = secret.to_string();
        AppState::new(config, Arc::new(FakeLlm), Arc::new(FakeSearch), None)
    }

    // ── Payload helper ────────────────────────────────────────────────────────

    fn review_requested_payload(action: &str) -> Vec<u8> {
        serde_json::json!({
            "action": action,
            "pull_request": {
                "number": 42,
                "user": { "login": "alice" },
                "head": { "sha": "abc123" }
            },
            "repository": {
                "name": "backend",
                "owner": { "login": "acme" }
            },
            "requested_reviewer": { "login": "trusty-review[bot]" }
        })
        .to_string()
        .into_bytes()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn webhook_payload_deserialises() {
        let payload = review_requested_payload("review_requested");
        let event: PullRequestEvent = serde_json::from_slice(&payload).unwrap();
        assert_eq!(event.action, "review_requested");
        assert_eq!(event.pull_request.number, 42);
        assert_eq!(event.repository.name, "backend");
        assert_eq!(event.repository.owner.login, "acme");
    }

    #[tokio::test]
    async fn webhook_rejects_bad_hmac() {
        let secret = "test-secret"; // pragma: allowlist secret
        let state = test_state_with_secret(secret);
        let router = crate::service::build_router(state);
        let payload = review_requested_payload("review_requested");

        let request = Request::builder()
            .method(Method::POST)
            .uri("/pr/github/webhook")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", "sha256=badhex0000")
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_rejects_missing_secret_config() {
        // When GITHUB_WEBHOOK_SECRET is empty in config, all webhooks → 401.
        let state = test_state_with_secret("");
        let router = crate::service::build_router(state);
        let payload = review_requested_payload("review_requested");

        let request = Request::builder()
            .method(Method::POST)
            .uri("/pr/github/webhook")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", "sha256=anything")
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_ignores_non_pull_request_event() {
        let secret = "test-secret"; // pragma: allowlist secret
        let state = test_state_with_secret(secret);
        let router = crate::service::build_router(state);

        let payload = br#"{"zen":"design for failure"}"#;
        let sig = make_sig(secret, payload);

        let request = Request::builder()
            .method(Method::POST)
            .uri("/pr/github/webhook")
            .header("x-github-event", "ping") // not pull_request
            .header("x-hub-signature-256", sig)
            .header("content-type", "application/json")
            .body(Body::from(payload.as_slice()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webhook_ignores_non_review_requested_action() {
        let secret = "test-secret"; // pragma: allowlist secret
        let state = test_state_with_secret(secret);
        let router = crate::service::build_router(state);

        let payload = review_requested_payload("opened"); // not review_requested
        let sig = make_sig(secret, &payload);

        let request = Request::builder()
            .method(Method::POST)
            .uri("/pr/github/webhook")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", sig)
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webhook_accepts_review_requested_returns_202() {
        let secret = "test-secret"; // pragma: allowlist secret
        let state = test_state_with_secret(secret);
        let router = crate::service::build_router(state);

        let payload = review_requested_payload("review_requested");
        let sig = make_sig(secret, &payload);

        let request = Request::builder()
            .method(Method::POST)
            .uri("/pr/github/webhook")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", sig)
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        // 202 Accepted — pipeline spawned in background.
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
}
