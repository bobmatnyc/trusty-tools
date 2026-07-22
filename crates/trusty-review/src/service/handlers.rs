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
use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::{
    config::{InvocationSurface, ReviewConfig},
    integrations::{analyze_client::AnalyzeClient, github::RunMode, search_client::SearchClient},
    llm::LlmProvider,
    pipeline::{DiffSource, ReviewDeps, ReviewInput, TriggerDecision, run_review},
    service::inference_probe::{InferenceProbe, InferenceStatus},
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
    /// LLM provider (verifier role, Phase 2 #583).  `None` disables the
    /// verification round for service-path reviews.
    pub verifier: Option<Arc<dyn LlmProvider>>,
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
    /// Short-TTL cache for the inference-reachability probe (#719).
    ///
    /// Why: /health and review_health need to report whether the configured LLM
    /// provider is actually accepting requests, not just whether the service
    /// process is alive.  The probe is cached so repeated health polls don't
    /// hammer the provider.
    pub inference_probe: InferenceProbe,
    /// Shutdown signal sender for outcome-poll background tasks (issue #1421).
    ///
    /// Why: background outcome-poll tasks use `tokio::select!` on the corresponding
    /// receiver so they are cancelled on daemon shutdown rather than becoming orphans.
    /// What: an `Arc<Sender<bool>>` shared across clones; sending `true` cancels all
    /// active poll tasks. Created fresh in every constructor; `serve()` sends `true`
    /// after `axum::serve` returns.
    /// Test: `webhook_closed_merged_schedules_outcome_poll` in `webhook_tests.rs`
    /// verifies the task is registered; orphan-prevention is structural (select!).
    pub shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
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
        Self::with_verifier_and_dedup(config, llm, None, search, analyze, dedup)
    }

    /// Construct `AppState` with an explicit verifier provider and dedup store.
    ///
    /// Why: the deployed `serve` daemon builds a verifier provider (Phase 2,
    /// #583) so the verification round runs on webhook-driven reviews; this
    /// constructor threads it in alongside the dedup store.  The simpler `new` /
    /// `with_dedup` constructors keep `verifier = None` for tests and callers
    /// that do not exercise verification.
    /// What: like `with_dedup`, but takes the verifier provider explicitly.
    /// Test: exercised by the `serve` path; unit tests use `new` (verifier None).
    pub fn with_verifier_and_dedup(
        config: ReviewConfig,
        llm: Arc<dyn LlmProvider>,
        verifier: Option<Arc<dyn LlmProvider>>,
        search: Arc<dyn SearchClient>,
        analyze: Option<Arc<dyn AnalyzeClient>>,
        dedup: Option<Arc<DedupStore>>,
    ) -> Self {
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        Self {
            config,
            llm,
            verifier,
            search,
            analyze,
            in_flight: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(std::sync::Mutex::new(None)),
            dedup,
            in_flight_registry: InFlightRegistry::new(),
            inference_probe: InferenceProbe::default(),
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }
}

// ─── Response shapes ──────────────────────────────────────────────────────────

/// Response body for GET /health.
///
/// Why: callers (load balancer, orchestrator) need a single JSON document
/// reporting liveness and dep reachability so they can decide whether to route
/// traffic to this instance.  MPM uses the `inference` field to gate whether to
/// attempt a `review_pr` call at all (closes #719).
/// What: mirrors spec REV-706; `deps.trusty_search.reachable` reflects a
/// non-blocking background probe; `inference` reflects the short-TTL
/// inference-reachability probe (see `InferenceProbe`).  `status` is `"degraded"`
/// when inference is not `"ok"` OR any `required` dep is unreachable (#722).
/// Test: `health_returns_ok_json`, `health_inference_ok_when_llm_ok`,
/// `health_inference_auth_error_sets_degraded`,
/// `health_required_dep_down_sets_degraded`,
/// `health_optional_dep_down_stays_ok`.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// `"ok"` when inference is healthy AND all required deps are reachable;
    /// `"degraded"` when inference is not `"ok"` OR a required dep is unreachable.
    pub status: &'static str,
    /// Pipeline version (e.g. `"tr-0.1"`).
    pub version: &'static str,
    /// Whether the service is in dry-run mode.
    pub dry_run: bool,
    /// Configured reviewer model slug.
    pub reviewer_model: String,
    /// Inference-reachability probe result (#719).  One of: `"ok"`,
    /// `"unreachable"`, `"auth_error"`, `"unknown"`.
    pub inference: InferenceStatus,
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
/// severity of a `false` without reading the docs.  `state` (#3658) adds a
/// tri-state view so a caller can distinguish "confirmed down" from "probe
/// timed out" — a slow-but-up dependency is operationally different from a
/// hard-down one, but both previously collapsed into `reachable: false`.
/// What: `required` is hardcoded per dep; `reachable` and `state` come from a
/// single bounded probe (see `bounded_probe`).  `reachable` is kept for
/// backward compatibility with existing consumers (`true` iff `state == Ok`).
/// Test: verified in `health_returns_ok_json`,
/// `health_stalled_dep_returns_timeout_state`.
#[derive(Debug, Serialize)]
pub struct DepInfo {
    /// Whether this dep is required for the service to function.
    pub required: bool,
    /// Whether the dep responded to a liveness probe at last check.
    /// `true` iff `state == DepState::Ok`.  Kept for back-compat: existing
    /// consumers gate on this single boolean field (#3658 is additive).
    pub reachable: bool,
    /// Tri-state probe result: `ok`, `unreachable`, or `timeout` (#3658).
    pub state: DepState,
}

/// Tri-state outcome of a single bounded dependency probe (#3658).
///
/// Why: post-#722 the dep probe correctly reported reachability, but a slow
/// (not down) dependency and a hard-down dependency both collapsed into
/// `reachable: false`, with no bound on how long the probe could take. This
/// type distinguishes "probe returned an error / unhealthy response"
/// (`Unreachable`) from "probe did not complete within the internal deadline"
/// (`Timeout`), so operators can tell "trusty-search is down" apart from
/// "trusty-search is just slow right now".
/// What: serialises as a lowercase string (`"ok"`, `"unreachable"`,
/// `"timeout"`) via `#[serde(rename_all = "snake_case")]`.
/// Test: `dep_state_serialises_lowercase`, `bounded_probe_*` in
/// `handlers_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepState {
    /// Probe completed within the deadline and reported a healthy dependency.
    Ok,
    /// Probe completed within the deadline but reported an error or an
    /// unhealthy response (e.g. embedder not ready).
    Unreachable,
    /// Probe did not complete within `dep_probe_timeout()` — the dependency
    /// is slow, not necessarily down.
    Timeout,
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
/// alternative to GitHub fetch (raw unified-diff string).  `pr_description`,
/// `pr_discussion`, and `referenced_code` are OPTIONAL caller-supplied context
/// (#1618): they let a caller hand the reviewer/verifier the PR prose, the human
/// review/issue discussion (author rationale), and any related/referenced source
/// the diff depends on.  All three are most important on the `local_diff_text`
/// path, where there is NO GitHub fetch, so the caller is the only source of this
/// context.  They are `Option`, default `None`, so existing callers are unaffected.
/// Test: `review_request_deserializes_without_optional_context`,
/// `review_request_deserializes_with_optional_context`.
#[derive(Debug, Default, Deserialize)]
pub struct ReviewRequest {
    /// GitHub organisation/user (required unless `local_diff_text` is set).
    pub owner: Option<String>,
    /// GitHub repository name (required unless `local_diff_text` is set).
    pub repo: Option<String>,
    /// Pull request number (required unless `local_diff_text` is set).
    pub pr: Option<u64>,
    /// Raw unified-diff text (alternative to GitHub fetch; always dry-run).
    pub local_diff_text: Option<String>,
    /// Caller-supplied PR body/description prose (#1618).  Rendered to the
    /// reviewer as a `## PR Description` section and passed to the verifier as
    /// author rationale.  On the local-diff path this is the only source of the
    /// PR description (no GitHub fetch).
    #[serde(default)]
    pub pr_description: Option<String>,
    /// Caller-supplied, concatenated human review/issue comments — the author's
    /// rationale (#1618).  Rendered to the reviewer as a `## PR Discussion /
    /// Author Rationale` section and passed to the verifier so it can refute a
    /// finding the author has already empirically addressed.
    #[serde(default)]
    pub pr_discussion: Option<String>,
    /// Caller-supplied referenced/related code or domain context the diff depends
    /// on (#1618), e.g. an upstream source/contract file.  Rendered to the
    /// reviewer as a `## Referenced Code` section.
    #[serde(default)]
    pub referenced_code: Option<String>,
}

// ─── Status computation ───────────────────────────────────────────────────────

/// Compute the top-level health status string from inference and dep results.
///
/// Why: the status decision was previously duplicated between the HTTP handler
/// and the MCP tool path, and it only considered `inference` — not required-dep
/// reachability.  This helper centralises the rule so both paths are consistent
/// and #722 is fixed: a required dep that is unreachable degrades status.
/// `Unknown` inference (probe timed out) does NOT degrade status (#739):
/// a slow Bedrock cold-start must not falsely report "degraded" — the operator's
/// real review calls have a ~300 s budget, far beyond the probe window.
/// What: returns `"ok"` only when `inference` is `Ok` or `Unknown` (timed-out
/// probe — could not confirm but not a hard failure) AND every dep with
/// `required == true` also has `reachable == true`.  Returns `"degraded"` when
/// inference is `Unreachable` or `AuthError` OR a required dep is unreachable.
/// Non-required deps never influence the result.
/// Test: `health_status_ok_all_good`, `health_status_degraded_required_dep_down`,
/// `health_status_degraded_inference_auth_error`,
/// `health_status_ok_optional_dep_down`,
/// `health_status_ok_inference_unknown` in `handlers_status_tests.rs`.
pub fn compute_status(inference: InferenceStatus, deps: &DepStatus) -> &'static str {
    let required_deps_ok = deps.trusty_search.reachable || !deps.trusty_search.required;
    // `Unknown` (probe timed out) is treated the same as `Ok` for the purpose of
    // computing the top-level status: we do not degrade because we couldn't confirm
    // reachability within the probe window (#739).
    let inference_ok = inference.is_ok() || inference == InferenceStatus::Unknown;
    if inference_ok && required_deps_ok {
        "ok"
    } else {
        "degraded"
    }
}

// ─── Bounded dependency probing (#3658) ───────────────────────────────────────

/// Return the per-dependency-probe hard timeout, consulting
/// `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS`.
///
/// Why: #3658 — the trusty-search/trusty-analyze reachability probes in
/// `handle_health` previously had no internal bound.  A slow (not down)
/// dependency could hang the whole health handler indefinitely, because the
/// only bound was each client's own HTTP-transport timeout (30 s for search,
/// 5 s for analyze) — far longer than any reasonable caller deadline.  This
/// timeout is deliberately short (default 2 s) and fully decoupled from those
/// client-level timeouts so `/health` always answers promptly regardless of
/// dependency latency.
/// What: reads `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS` from the environment;
/// parses as `u64` seconds; falls back to `DEFAULT_DEP_PROBE_TIMEOUT_SECS` (2)
/// on any parse failure, unset variable, or a value of 0 (to prevent an
/// accidentally-zero timeout from making every probe report `timeout`).
/// Test: `dep_probe_timeout_default`, `dep_probe_timeout_env_override`,
/// `dep_probe_timeout_env_invalid_falls_back`,
/// `dep_probe_timeout_env_zero_falls_back` in `handlers_tests.rs`.
pub(crate) fn dep_probe_timeout() -> Duration {
    const DEFAULT_DEP_PROBE_TIMEOUT_SECS: u64 = 2;
    const ENV_VAR: &str = "TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS";

    let secs = std::env::var(ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_DEP_PROBE_TIMEOUT_SECS);

    Duration::from_secs(secs)
}

/// Run a single dependency probe future under a strict internal deadline.
///
/// Why: centralises the "wrap in `tokio::time::timeout`, map the outcome to a
/// `DepState`" logic so both `handle_health` and the MCP `review_health` tool
/// get the same bounded-latency guarantee (#3658) instead of duplicating it.
/// What: awaits `fut` under `tokio::time::timeout(timeout, fut)`.  A
/// completed `Ok(v)` is passed to `is_healthy` to decide `DepState::Ok` vs
/// `DepState::Unreachable`; a completed `Err(_)` is `DepState::Unreachable`;
/// an elapsed deadline is `DepState::Timeout` — deliberately distinct from
/// `Unreachable` so a slow-but-up dependency is never reported as hard-down.
/// Test: `bounded_probe_ok_on_healthy_response`,
/// `bounded_probe_unreachable_on_error`,
/// `bounded_probe_unreachable_on_unhealthy_response`,
/// `bounded_probe_timeout_on_stalled_future` in `handlers_tests.rs`.
async fn bounded_probe<Fut, T, E>(
    fut: Fut,
    timeout: Duration,
    is_healthy: impl FnOnce(T) -> bool,
) -> DepState
where
    Fut: std::future::Future<Output = Result<T, E>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(v)) => {
            if is_healthy(v) {
                DepState::Ok
            } else {
                DepState::Unreachable
            }
        }
        Ok(Err(_)) => DepState::Unreachable,
        Err(_elapsed) => DepState::Timeout,
    }
}

/// Probe trusty-search and trusty-analyze reachability concurrently, each
/// bounded by `dep_probe_timeout()` (#3658).
///
/// Why: shared by the HTTP `/health` handler and the MCP `review_health` tool
/// so both paths get the same bounded-latency guarantee and the same
/// tri-state dep status, instead of each duplicating an unbounded sequential
/// probe.  Probing concurrently (via `tokio::join!`) — rather than
/// sequentially — bounds the total dep-probe latency to `dep_probe_timeout()`
/// regardless of how many deps are probed or how slow each one is, per the
/// issue's requirement that overall `/health` latency stay bounded (~2 s
/// worst case) independent of every dependency.
/// What: runs `state.search.health()` and (if configured)
/// `state.analyze.health()` concurrently, each wrapped in `bounded_probe`;
/// returns the fully-populated `DepStatus`.  An unconfigured `analyze` client
/// is reported as `DepState::Unreachable` (unchanged back-compat behaviour:
/// `reachable: false`).
/// Test: `health_stalled_dep_returns_timeout_state`,
/// `health_fast_dep_healthy_path_unchanged`,
/// `health_hard_down_dep_reachable_false` in `handlers_tests.rs`.
pub async fn probe_deps(state: &AppState) -> DepStatus {
    let timeout = dep_probe_timeout();

    let search_probe = bounded_probe(state.search.health(), timeout, |r| r.is_healthy());
    let analyze_probe = async {
        match &state.analyze {
            Some(a) => bounded_probe(a.health(), timeout, |_| true).await,
            None => DepState::Unreachable,
        }
    };
    let (search_state, analyze_state) = tokio::join!(search_probe, analyze_probe);

    DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: search_state == DepState::Ok,
            state: search_state,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: analyze_state == DepState::Ok,
            state: analyze_state,
        },
    }
}

// ─── Route handlers ───────────────────────────────────────────────────────────

/// GET /health — liveness, dependency reachability, and inference probe.
///
/// Why: required by load balancers and orchestrators to determine whether this
/// instance is ready to handle traffic.  MPM uses the `inference` field to
/// gate whether to attempt a `review_pr` call (closes #719).
/// What: performs bounded, concurrent health probes against trusty-search and
/// trusty-analyze via `probe_deps` (each capped at `dep_probe_timeout()`,
/// default 2 s, decoupled from any client's own HTTP timeout — #3658); runs
/// the cached inference-reachability probe (10 s TTL, timeout configurable via
/// `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS`, default 10 s — see #739) against the
/// configured LLM provider; returns JSON with dep status, reviewer model, and
/// inference result.  HTTP 200 always (degraded state is noted in the body,
/// not via 5xx, to avoid false-positive load-balancer evictions).  When
/// inference is `"unreachable"` or `"auth_error"` OR a required dep is
/// unreachable, `status` becomes `"degraded"`.  An inference probe timeout
/// returns `"unknown"` (not `"degraded"`) so a slow Bedrock cold-start does not
/// falsely degrade status (#739); a dep probe timeout reports
/// `deps.trusty_search.state: "timeout"` with `reachable: false` (#3658).
/// Test: `health_inference_ok_when_llm_ok`,
/// `health_inference_auth_error_sets_degraded`,
/// `health_required_dep_down_sets_degraded`,
/// `health_optional_dep_down_stays_ok`,
/// `health_stalled_dep_returns_timeout_state`.
pub async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    // Bounded, concurrent dep probes (#3658) — total latency capped at
    // `dep_probe_timeout()` regardless of dep count or individual slowness.
    let deps = probe_deps(&state).await;

    // Cached inference-reachability probe (#719).
    let reviewer_model = state.config.role_models.reviewer.model.clone();
    let inference = state
        .inference_probe
        .probe(&state.llm, &reviewer_model)
        .await;

    // #722: status is "degraded" when inference fails OR any required dep is down.
    let status = compute_status(inference, &deps);

    let body = HealthResponse {
        status,
        version: env!("CARGO_PKG_VERSION"),
        dry_run: state.config.dry_run,
        reviewer_model,
        inference,
        deps,
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
        verifier: state.verifier.clone(),
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
        // Thread caller-supplied PR context (#1618) into the runner.  On the
        // local-diff path this is the only source of PR description / discussion /
        // referenced code (no GitHub fetch).
        caller_context: crate::pipeline::runner::CallerContext {
            pr_description: req.pr_description.clone(),
            pr_discussion: req.pr_discussion.clone(),
            referenced_code: req.referenced_code.clone(),
        },
        // POST /review is out of scope for the interactive-degrade default
        // (unaffected by the search-unreachable semantics fix) — keeps the
        // strict `Hosted` default, unchanged behaviour.
        surface: InvocationSurface::default(),
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
/// present and returns `DiffSource::Github` with an empty token placeholder.
/// `pipeline::runner::run_review` resolves the real token from config via
/// `resolve_diff_token` before fetching the diff (#1880) — this function never
/// talks to GitHub itself.
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

    // Token is empty here; `run_review` resolves it from config via
    // `resolve_diff_token` before the diff fetch (#1880). If no token can be
    // resolved the review fails closed (UNKNOWN, `result.error` set) rather
    // than sending an empty-token request that surfaces as an opaque 401.
    Ok(DiffSource::Github {
        owner,
        repo,
        pr,
        token: String::new(),
    })
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Split into focused sibling files to keep every file under the 500-line cap:
//   handlers_tests.rs        — fakes, state builders, and basic handler tests.
//   handlers_status_tests.rs — compute_status unit tests + dep-degradation (#722).

#[cfg(test)]
#[path = "handlers_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "handlers_status_tests.rs"]
mod status_tests;
