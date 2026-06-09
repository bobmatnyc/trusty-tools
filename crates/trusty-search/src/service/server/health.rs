//! `GET /health` and `POST /upgrade` handlers.
//!
//! Why: Health probes are polled at 2s intervals by external orchestrators;
//! keeping them in a focused module makes response-shape changes easy to review.
//! What: `health_handler` returns daemon liveness + embedder status + resource
//! metrics. `upgrade_handler` drives `cargo install` and triggers a self-restart.
//! Test: `health_handler_reports_indexes_and_uptime` and related tests in
//! `super::tests`.
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::state::{SearchAppState, WarmBootSummary};

/// Response shape for `GET /health` (issue #34 + #35 + #38 + #282 + #537).
#[derive(Serialize)]
pub(super) struct HealthResponse {
    pub(super) status: &'static str,
    pub(super) version: &'static str,
    pub(super) indexes: usize,
    pub(super) uptime_secs: u64,
    pub(super) embedder: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedder_error: Option<String>,
    pub(super) rss_mb: u64,
    pub(super) rss_limit_mb: u64,
    pub(super) disk_bytes: u64,
    pub(super) cpu_pct: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedder_info: Option<EmbedderInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) embedderd_rss_mb: Option<u64>,
    pub(super) background_reindex_queue_depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) update_available: Option<String>,
    /// Warm-boot summary: how many indexes loaded vs. skipped and by what
    /// reason (issue #873). Always present after warm-boot completes; all
    /// fields are `0`/`false` on a fresh start before warm-boot runs.
    ///
    /// Why: makes the "cargo install dropped FDA" symptom (`indexes:2` from
    /// `~102`) immediately visible without tailing logs. The `warm_boot_degraded`
    /// boolean is the machine-readable flag external monitors should poll.
    pub(super) warmboot_summary: WarmBootSummary,
}

/// Embedding-model metadata surfaced by `GET /health` (issue #38).
///
/// Why: the redesigned web UI's Health view shows which model is loaded, its
/// output dimension, and whether ONNX is dispatching to CPU / CoreML / CUDA.
/// Operators previously had to read the daemon startup log for this.
/// What: a small serialisable struct derived from the live `Arc<dyn Embedder>`
/// — `dimension` comes from `Embedder::dimension()`, `provider` from
/// `Embedder::provider()`, and `quantized` is inferred from the provider-
/// agnostic default model (the daemon ships the INT8 `AllMiniLML6V2Q`).
/// Test: `health_includes_embedder_info_when_ready` builds a state with a
/// `MockEmbedder` and asserts the block is present with a 384-dim value.
#[derive(Serialize)]
pub(super) struct EmbedderInfo {
    /// Vector dimensionality reported by the embedder (384 for all-MiniLM-L6).
    dimension: usize,
    /// Active ONNX execution provider: `"CPU"`, `"CoreML"`, or `"CUDA"`.
    provider: String,
    /// Whether the loaded model is the INT8-quantized variant. The daemon
    /// defaults to `AllMiniLML6V2Q` (quantized); a missing quantized model
    /// falls back to full precision.
    quantized: bool,
}

pub(super) async fn health_handler(
    State(state): State<Arc<SearchAppState>>,
) -> Json<HealthResponse> {
    // Why: open-mpm (and other external integrators) probe `/health` to detect
    // a running trusty-search daemon before spawning their own. Including
    // `indexes` count lets the caller verify the daemon is not only alive but
    // also has the expected registry populated (issue #34).
    // What: returns `{ status, version, indexes, uptime_secs }` where
    // `indexes` is the number of registered IndexHandles in the registry
    // and `uptime_secs` is wall-clock seconds since AppState construction.
    // Test: register N indexes, GET /health, assert `indexes == N` and
    // `uptime_secs >= 0`.
    let embedder_error = state.current_embedder_error();
    let embedder_status = if state.is_embedder_ready() {
        "ready"
    } else if state.embedder.is_some()
        || state
            .embedder_slot
            .try_read()
            .map(|g| g.is_some())
            .unwrap_or(false)
    {
        // Slot populated but readiness flag not yet flipped — treat as ready.
        "ready"
    } else if embedder_error.is_some() {
        // Init task failed or timed out (issue #121). Callers must not retry
        // forever — report a terminal error state so operators can intervene.
        "error"
    } else {
        // Daemon is up but embedder still loading. Callers should retry
        // mutating endpoints; `/health` itself always returns 200 so
        // `trusty-search start`'s readiness probe succeeds quickly.
        "initializing"
    };
    // Issue #35: sample process RSS + CPU. The sampler is shared behind a
    // Mutex because sysinfo derives CPU% from the delta between refreshes.
    let (rss_mb, cpu_pct) = {
        let mut metrics = state.sys_metrics.lock().await;
        metrics.sample()
    };
    // `rss_limit_mb` mirrors the resolved TRUSTY_MEMORY_LIMIT_MB soft cap.
    // `memory_limit_mb()` returns `None` when no limit is configured.
    let rss_limit_mb = crate::core::memguard::memory_limit_mb().unwrap_or(0);
    let disk_bytes = state.disk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    // Issue #38: surface model detail (dimension + provider) once the embedder
    // is wired so the admin UI's Health view doesn't need a separate request.
    let embedder_info = state.current_embedder().await.map(|e| {
        let dimension = e.dimension();
        EmbedderInfo {
            dimension,
            provider: e.provider().as_str().to_string(),
            // The daemon defaults to the INT8-quantized AllMiniLML6V2Q model;
            // a 384-dim embedder is the quantized all-MiniLM-L6 variant.
            quantized: dimension == trusty_common::embedder::EMBED_DIM,
        }
    });
    // Issue #282: sample the sidecar's current RSS (None when not running).
    let embedderd_rss_mb = state
        .current_embedderd_pid()
        .and_then(crate::core::memguard::current_rss_mb_for_pid);
    let update_available = state.update_available.lock().ok().and_then(|g| g.clone());
    // Issue #873: surface the warm-boot summary so a post-`cargo install` FDA
    // regression (`indexes:2` instead of `~102`) is visible without tailing logs.
    let warmboot_summary = state
        .warmboot_summary
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        indexes: state.registry.list().len(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        embedder: embedder_status,
        embedder_error,
        rss_mb,
        rss_limit_mb,
        disk_bytes,
        cpu_pct,
        embedder_info,
        embedderd_rss_mb,
        // Issue #458: expose the background reindex backlog so operators can
        // watch the startup storm drain without reading daemon logs.
        background_reindex_queue_depth: crate::service::reindex::background_reindex_queue_depth(),
        update_available,
        warmboot_summary,
    })
}

/// Request body for `POST /upgrade` (issue #537).
///
/// Why: typed body avoids raw JSON field extraction in the handler, and serde
/// provides friendly error messages for malformed requests.
/// What: mirrors the MCP tool schema: `check` (default true) and `confirm`.
/// Test: the MCP `upgrade` tool calls this endpoint.
#[derive(Deserialize)]
pub(super) struct UpgradeRequest {
    #[serde(default = "bool_true")]
    check: bool,
    #[serde(default)]
    confirm: bool,
}

/// `POST /upgrade` — check for or install a new trusty-search version (issue #537).
///
/// Why: Exposes the upgrade workflow over HTTP so the MCP dispatcher (which
/// calls the daemon's REST API) can trigger an upgrade and receive the response
/// before the daemon self-exits. Never silently auto-installs.
///
/// What:
/// - `check=true` or `confirm=false`: query crates.io and return version info.
/// - `confirm=true`: install via `cargo install --locked`, health-gate, then
///   schedule a 500 ms delayed exit (to flush this response) and return the
///   result. When launchd-supervised the daemon exits non-zero so launchd
///   respawns with the new binary. When unsupervised a restart hint is returned.
///
/// Test: manual via `curl -X POST http://127.0.0.1:$(trusty-search port)/upgrade \
///  -H 'Content-Type: application/json' -d '{"check":true}'`.
pub(super) async fn upgrade_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(body): Json<UpgradeRequest>,
) -> Json<serde_json::Value> {
    let crate_name = env!("CARGO_PKG_NAME");
    let current = env!("CARGO_PKG_VERSION");

    let info = trusty_common::update::check_crates_io(crate_name, current).await;

    let (latest, is_update) = match &info {
        Some(u) => (u.latest.as_str(), true),
        None => (current, false),
    };

    if body.check || !body.confirm {
        let msg = if is_update {
            format!(
                "Update available: {crate_name} {latest} (you have {current}). \
                 POST with confirm=true to install."
            )
        } else {
            format!("{crate_name} {current} is already up to date.")
        };
        return Json(serde_json::json!({
            "status": "checked",
            "current": current,
            "latest": latest,
            "update_available": is_update,
            "message": msg
        }));
    }

    if !is_update {
        return Json(serde_json::json!({
            "status": "up_to_date",
            "current": current,
            "message": format!("{crate_name} {current} is already up to date.")
        }));
    }

    let latest_owned = latest.to_string();
    let crate_name_owned = crate_name.to_string();
    let update_slot = state.update_available.clone();
    let response = serde_json::json!({
        "status": "installing",
        "current": current,
        "latest": latest_owned,
        "message": format!(
            "Installing {crate_name} {latest_owned} — daemon will restart \
             under launchd (or print a restart hint if not supervised)."
        )
    });

    // Spawn the install on a delayed task so this handler can return the
    // response to the HTTP client (and thus to the MCP caller) before the
    // process might exit. 500 ms gives the TCP stack time to flush.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        match trusty_common::update::upgrade_and_restart(&crate_name_owned, &crate_name_owned).await
        {
            Ok(Some(hint)) => {
                tracing::info!("{hint}");
                eprintln!("{hint}");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("upgrade_and_restart failed: {e:#}");
                eprintln!("[trusty-search] upgrade failed: {e:#}");
                if let Ok(mut g) = update_slot.lock() {
                    *g = None;
                }
            }
        }
    });

    Json(response)
}

fn bool_true() -> bool {
    true
}
