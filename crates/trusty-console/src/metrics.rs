//! Metrics proxy handlers for the console API.
//!
//! Why: The Svelte SPA is prohibited from calling daemon ports directly; this
//! module provides `/api/console/metrics/*` endpoints that the console backend
//! owns and populates by proxying to the relevant daemons internally.
//! What: Currently exposes one handler (`metrics_analyze_handler`) for
//! `GET /api/console/metrics/analyze` that reads from the polled service
//! snapshot, fans out to the analyze daemon's `/health` and `/indexes` routes,
//! and returns a stable `AnalyzeMetrics` JSON shape.
//! Test: `test_metrics_analyze_absent` in `server.rs` exercises the absent-daemon
//! path (cold cache, no URL) and asserts HTTP 200 with `{"reachable":false}`.

use axum::{extract::State, response::IntoResponse};
use serde::Serialize;

use crate::server::AppState;

// ─── response type ────────────────────────────────────────────────────────────

/// Response shape for `GET /api/console/metrics/analyze`.
///
/// Why: The console SPA renders trusty-analyze metrics natively (no iframe, no
/// direct daemon HTTP calls from the browser).  This struct is the canonical
/// JSON contract between the console backend and the Svelte UI.
/// What: Aggregates the daemon health state plus whatever quality/hotspot data
/// is available from the running trusty-analyze instance.  All fields after
/// `reachable` are optional so the shape degrades gracefully when the daemon is
/// absent or the search backend is not connected.
/// Test: `test_metrics_analyze_absent` in `server.rs`.
#[derive(Debug, Serialize)]
pub struct AnalyzeMetrics {
    /// Whether trusty-analyze answered its `/health` probe.
    pub reachable: bool,
    /// Version string from the daemon's `/health` response, if reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether the analyze daemon can reach trusty-search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_reachable: Option<bool>,
    /// Number of indexes visible to trusty-analyze (from `GET /indexes`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_count: Option<usize>,
}

// ─── handler ──────────────────────────────────────────────────────────────────

/// `GET /api/console/metrics/analyze` — return live metrics from trusty-analyze.
///
/// Why: The Svelte SPA must call ONLY console-owned `/api/console/*` endpoints;
/// it must never talk directly to the daemon's own port.  This handler is the
/// console's owned facade over the analyze daemon.
/// What: Reads the trusty-analyze URL from the polled service snapshot.  If the
/// daemon is reachable it fetches `/health` and `/indexes`, then assembles and
/// returns an `AnalyzeMetrics` JSON object.  If the daemon is absent or the
/// cache is cold it returns a minimal `{"reachable":false}` response (still
/// HTTP 200 — the UI decides how to display the absent state).
/// Test: `test_metrics_analyze_absent` in `server.rs`.
pub async fn metrics_analyze_handler(State(state): State<AppState>) -> axum::response::Response {
    // Resolve the live analyze URL from the cached service snapshot.
    let analyze_url: Option<String> = if let Some(snap) = state.poller_cache().snapshot().await {
        snap.services
            .iter()
            .find(|s| s.id == "trusty-analyze")
            .and_then(|s| s.url.clone())
    } else {
        None
    };

    let Some(base_url) = analyze_url else {
        return axum::Json(AnalyzeMetrics {
            reachable: false,
            version: None,
            search_reachable: None,
            index_count: None,
        })
        .into_response();
    };

    let client = state.http_client();

    // Fetch /health from the analyze daemon.
    let health_resp = client
        .get(format!("{base_url}/health"))
        .send()
        .await
        .ok()
        .and_then(|r| {
            if r.status().is_success() {
                Some(r)
            } else {
                None
            }
        });

    let Some(health_body) = health_resp else {
        return axum::Json(AnalyzeMetrics {
            reachable: false,
            version: None,
            search_reachable: None,
            index_count: None,
        })
        .into_response();
    };

    let health_json: serde_json::Value = match health_body.json().await {
        Ok(v) => v,
        Err(_) => serde_json::Value::Null,
    };

    let version = health_json
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let search_reachable = health_json
        .get("search_reachable")
        .and_then(|v| v.as_bool());

    // Fetch /indexes to count visible indexes.
    let index_resp = client
        .get(format!("{base_url}/indexes"))
        .send()
        .await
        .ok()
        .and_then(|r| {
            if r.status().is_success() {
                Some(r)
            } else {
                None
            }
        });

    let index_count: Option<usize> = match index_resp {
        Some(r) => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.as_array().map(|a| a.len())),
        None => None,
    };

    axum::Json(AnalyzeMetrics {
        reachable: true,
        version,
        search_reachable,
        index_count,
    })
    .into_response()
}
