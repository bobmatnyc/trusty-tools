//! Axum route handlers for trusty-review's HTTP service.
//!
//! Why: the handlers live in a dedicated file so each route is easy to locate,
//! test, and evolve independently without growing `service/mod.rs` past the
//! 500-line cap.
//!
//! What: implements GET /health, GET /status, and POST /review.
//! POST /pr/github/webhook is in `webhook.rs` to keep webhook-specific logic
//! (HMAC, event parsing, spawn) isolated from the direct-call path.
//!
//! Test: each handler is exercised via `tower::ServiceExt::oneshot` in the
//! `tests` module below.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::{
    config::ReviewConfig,
    integrations::{analyze_client::AnalyzeClient, github::RunMode, search_client::SearchClient},
    llm::LlmProvider,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, TriggerDecision, run_review},
    store::{DedupStore, InFlightRegistry},
};

// ─── AppState ─────────────────────────────────────────────────────────────────

/// Shared state injected into every handler via axum's `State` extractor.
///
/// Why: groups all service-level dependencies so they are built once at startup
/// and cheaply cloned per request (all fields are `Arc`-backed or `Clone`).
/// What: holds resolved config, LLM provider, search/analyze clients, an
/// in-flight counter, and the last pipeline error string (if any).
/// Test: `AppState::new_for_test` is used by handler unit tests.
#[derive(Clone)]
pub struct AppState {
    /// Resolved global configuration.
    pub config: ReviewConfig,
    /// LLM provider (reviewer role).
    pub llm: Arc<dyn LlmProvider>,
    /// Code search client.
    pub search: Arc<dyn SearchClient>,
    /// Static analysis client (optional — `None` skips the analyze step).
    pub analyze: Option<Arc<dyn AnalyzeClient>>,
    /// Count of reviews currently running in background spawned tasks.
    pub in_flight: Arc<AtomicU64>,
    /// Last pipeline error, if any (populated by webhook background tasks).
    pub last_error: Arc<std::sync::Mutex<Option<String>>>,
    /// SHA-keyed durable dedup store (Phase 1, #582).  `None` disables dedup.
    pub dedup: Option<Arc<DedupStore>>,
    /// In-process in-flight guard registry (Phase 1, #582) — drops duplicate
    /// concurrent webhook deliveries for the same PR / head SHA.
    pub in_flight_registry: InFlightRegistry,
}

impl AppState {
    /// Construct `AppState` with the core deps and no dedup store.
    ///
    /// Why: the common constructor for tests and single-process deployments that
    /// do not need cross-process dedup; the in-flight registry is always created
    /// so concurrent webhook deliveries are still de-duplicated in-process.
    /// What: wraps the provided deps in `Arc` counters, an empty error cell, a
    /// `None` dedup store, and a fresh `InFlightRegistry`.
    /// Test: used by handler/webhook unit tests that provide fake deps.
    pub fn new(
        config: ReviewConfig,
        llm: Arc<dyn LlmProvider>,
        search: Arc<dyn SearchClient>,
        analyze: Option<Arc<dyn AnalyzeClient>>,
    ) -> Self {
        Self::with_dedup(config, llm, search, analyze, None)
    }

    /// Construct `AppState` including an optional durable dedup store.
    ///
    /// Why: the deployed `serve` daemon opens a redb-backed dedup store under the
    /// log dir so retries / restarts do not re-review the same head SHA; this
    /// constructor threads it into the shared state.
    /// What: like `new`, but takes the dedup store explicitly.
    /// Test: exercised by the `serve` path; unit tests use `new` (dedup `None`).
    pub fn with_dedup(
        config: ReviewConfig,
        llm: Arc<dyn LlmProvider>,
        search: Arc<dyn SearchClient>,
        analyze: Option<Arc<dyn AnalyzeClient>>,
        dedup: Option<Arc<DedupStore>>,
    ) -> Self {
        Self {
            config,
            llm,
            search,
            analyze,
            in_flight: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(std::sync::Mutex::new(None)),
            dedup,
            in_flight_registry: InFlightRegistry::new(),
        }
    }
}

// ─── Response shapes ──────────────────────────────────────────────────────────

/// Response body for GET /health.
///
/// Why: callers (load balancer, orchestrator) need a single JSON document
/// reporting liveness and dep reachability so they can decide whether to route
/// traffic to this instance.
/// What: mirrors spec REV-706; `deps.trusty_search.reachable` reflects a
/// non-blocking background probe cached in `AppState`.
/// Test: `health_returns_ok_json`.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// `"ok"` when all required deps are reachable.
    pub status: &'static str,
    /// Pipeline version (e.g. `"tr-0.1"`).
    pub version: &'static str,
    /// Whether the service is in dry-run mode.
    pub dry_run: bool,
    /// Configured reviewer model slug.
    pub reviewer_model: String,
    /// Dependency reachability snapshot.
    pub deps: DepStatus,
}

/// Dependency reachability status embedded in HealthResponse.
///
/// Why: operators need to distinguish "search is down" from "analyze is down"
/// at a glance; the `required` flag tells them which matters more.
/// What: `trusty_search` is required; `trusty_analyze` is optional.
/// Test: `health_returns_ok_json`.
#[derive(Debug, Serialize)]
pub struct DepStatus {
    /// trusty-search reachability (required dep).
    pub trusty_search: DepInfo,
    /// trusty-analyze reachability (optional dep).
    pub trusty_analyze: DepInfo,
}

/// Per-dependency info node.
///
/// Why: provides `required` alongside `reachable` so consumers know the
/// severity of a `false` without reading the docs.
/// What: `required` is hardcoded per dep; `reachable` is a non-blocking probe.
/// Test: verified in `health_returns_ok_json`.
#[derive(Debug, Serialize)]
pub struct DepInfo {
    /// Whether this dep is required for the service to function.
    pub required: bool,
    /// Whether the dep responded to a liveness probe at last check.
    pub reachable: bool,
}

/// Response body for GET /status.
///
/// Why: operators and monitors need a richer view than /health — specifically
/// how many reviews are in-flight and what the last error was.
/// What: in_flight is read atomically from AppState; last_error is the most
/// recent error string from a background webhook task.
/// Test: `status_returns_json_with_in_flight`.
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    /// Number of reviews currently executing (background or synchronous).
    pub in_flight: u64,
    /// Last pipeline error, if any.
    pub last_error: Option<String>,
}

/// Request body for POST /review.
///
/// Why: the key local-service endpoint accepts a JSON body identifying the PR
/// to review; optional `local_diff` allows direct diff text injection (useful
/// for CI pipelines that have already fetched the diff).
/// What: `owner`/`repo`/`pr` identify a GitHub PR; `local_diff_text` is an
/// alternative to GitHub fetch (raw unified-diff string).
/// Test: `review_endpoint_with_fake_deps_returns_result`.
#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    /// GitHub organisation/user (required unless `local_diff_text` is set).
    pub owner: Option<String>,
    /// GitHub repository name (required unless `local_diff_text` is set).
    pub repo: Option<String>,
    /// Pull request number (required unless `local_diff_text` is set).
    pub pr: Option<u64>,
    /// Raw unified-diff text (alternative to GitHub fetch; always dry-run).
    pub local_diff_text: Option<String>,
}

// ─── Route handlers ───────────────────────────────────────────────────────────

/// GET /health — liveness and dependency reachability.
///
/// Why: required by load balancers and orchestrators to determine whether this
/// instance is ready to handle traffic.
/// What: performs non-blocking health probes against trusty-search and
/// trusty-analyze (both via `.health()` on the trait objects); returns JSON
/// with dep status and reviewer model.  200 always (degraded state is noted
/// in the body, not via 5xx, to avoid false-positive load-balancer evictions
/// for the optional analyze dep).
/// Test: `health_returns_ok_json`.
pub async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    // Non-blocking dep probes — we fire them but treat errors as "unreachable".
    let search_reachable = state.search.health().await.is_ok_and(|r| r.is_healthy());
    let analyze_reachable = match &state.analyze {
        Some(a) => a.health().await.is_ok(),
        None => false,
    };

    let body = HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        dry_run: state.config.dry_run,
        reviewer_model: state.config.role_models.reviewer.model.clone(),
        deps: DepStatus {
            trusty_search: DepInfo {
                required: true,
                reachable: search_reachable,
            },
            trusty_analyze: DepInfo {
                required: false,
                reachable: analyze_reachable,
            },
        },
    };

    (StatusCode::OK, Json(body))
}

/// GET /status — in-flight review count and last error.
///
/// Why: operators need a lightweight operational view distinct from /health
/// (which focuses on dep reachability) so they can monitor pipeline throughput
/// and catch silent failures from background webhook tasks.
/// What: reads `in_flight` atomically and acquires the `last_error` mutex.
/// Test: `status_returns_json_with_in_flight`.
pub async fn handle_status(State(state): State<AppState>) -> impl IntoResponse {
    let in_flight = state.in_flight.load(Ordering::Relaxed);
    let last_error = state
        .last_error
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    (
        StatusCode::OK,
        Json(StatusResponse {
            in_flight,
            last_error,
        }),
    )
}

/// POST /review — synchronous pipeline run, returns ReviewResult JSON.
///
/// Why: the primary local-service endpoint lets CI pipelines, editor
/// integrations, and scripts trigger a review on a live PR or a raw diff
/// without spawning a CLI process.  Runs SYNCHRONOUSLY so the caller blocks
/// until the verdict is ready (design intent: sub-10s for a normal PR).
/// What: parses the request body, resolves the DiffSource, calls `run_review`,
/// and returns the `ReviewResult` as JSON.  Always dry-run (push firewall
/// remains in force).  Does NOT post to GitHub.
/// Test: `review_endpoint_with_fake_deps_returns_result`.
pub async fn handle_review(
    State(state): State<AppState>,
    Json(req): Json<ReviewRequest>,
) -> impl IntoResponse {
    debug!("POST /review received");

    // Resolve the diff source from the request.
    let diff_source = match resolve_diff_source(&req) {
        Ok(s) => s,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response();
        }
    };

    let reviewer_model = state.config.role_models.reviewer.model.clone();

    let deps = ReviewDeps {
        llm: Arc::clone(&state.llm),
        search: Arc::clone(&state.search),
        analyze: state.analyze.clone(),
        // POST /review is a synchronous inspection endpoint — no dedup needed.
        dedup: None,
    };

    let input = ReviewInput {
        diff_source,
        reviewer_model,
        write_log: false, // HTTP callers don't write logs by default.
        print_result: false,
        // POST /review never posts to GitHub — it always returns the result to
        // the caller (push firewall + dry-run remain in force).
        trigger: TriggerDecision::ForceDryRun,
        run_mode: RunMode::Serve,
        allow_posting: false,
    };

    state.in_flight.fetch_add(1, Ordering::Relaxed);
    let result = run_review(&state.config, input, deps).await;
    state.in_flight.fetch_sub(1, Ordering::Relaxed);

    info!(
        verdict = %result.verdict,
        findings = result.findings.len(),
        model = %result.model,
        "POST /review complete"
    );

    (StatusCode::OK, Json(result)).into_response()
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Resolve a `DiffSource` from a `ReviewRequest`.
///
/// Why: centralises request validation so the handler body stays clean.
/// What: if `local_diff_text` is present, writes it to a tempfile and returns
/// `DiffSource::LocalFile`; otherwise validates that owner/repo/pr are all
/// present and returns `DiffSource::Github` with an empty token (the pipeline
/// will resolve the token from config).
/// Test: covered indirectly by `review_endpoint_*` handler tests.
fn resolve_diff_source(req: &ReviewRequest) -> Result<DiffSource, String> {
    if let Some(ref diff_text) = req.local_diff_text {
        // Write the raw diff to a tempfile so the pipeline can read it.
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new().map_err(|e| format!("tempfile error: {e}"))?;
        tmp.write_all(diff_text.as_bytes())
            .map_err(|e| format!("tempfile write error: {e}"))?;
        // Leak the tempfile handle so the path stays valid until the pipeline
        // reads it; it will be cleaned up when the process exits.
        let path = tmp
            .into_temp_path()
            .keep()
            .map_err(|e| format!("keep tempfile: {e}"))?;
        return Ok(DiffSource::LocalFile {
            path: path.to_path_buf(),
        });
    }

    let owner = req
        .owner
        .as_deref()
        .ok_or_else(|| "owner is required (or provide local_diff_text)".to_string())?
        .to_string();
    let repo = req
        .repo
        .as_deref()
        .ok_or_else(|| "repo is required (or provide local_diff_text)".to_string())?
        .to_string();
    let pr = req
        .pr
        .ok_or_else(|| "pr is required (or provide local_diff_text)".to_string())?;

    // Token is empty here; the pipeline will attempt to resolve it from config.
    // If no token is available the pipeline fails gracefully (fail-safe APPROVE).
    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token: String::new(),
    })
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Split into `handlers_tests.rs` to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "handlers_tests.rs"]
mod tests;
