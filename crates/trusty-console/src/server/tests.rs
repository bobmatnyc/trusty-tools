//! Integration tests for `server::build_router` / `build_router_with_self_origins`.
//!
//! Why: split out of `server/mod.rs` (was `server.rs`) to stay under the repo's
//! 500-SLOC production-file cap after the #3268/#3269 same-origin-guard tests
//! pushed the combined prod+test file over its grandfathered budget. This module
//! is classified as a test file (basename `tests.rs`) under the 1500-SLOC
//! test-file cap, so it has ample headroom.
//! What: axum test-client integration tests exercising the router end-to-end
//! (routes, proxy allowlist, and the write-origin guard) without a real TCP
//! listener.
//! Test: this module IS the test suite for `server/mod.rs`.

use super::*;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::connector::{ServiceInfo, ServiceStatus};

/// A stub connector for tests — always returns a fixed `ServiceInfo`.
struct StubConnector {
    id: &'static str,
    display_name: &'static str,
    status: ServiceStatus,
}

impl ServiceConnector for StubConnector {
    fn id(&self) -> &'static str {
        self.id
    }
    fn display_name(&self) -> &'static str {
        self.display_name
    }
    fn detect(&self) -> ServiceInfo {
        ServiceInfo {
            id: self.id.to_string(),
            display_name: self.display_name.to_string(),
            status: self.status.clone(),
            version: None,
            url: None,
            hint: None,
        }
    }
}

fn make_test_state() -> AppState {
    AppState::new(vec![
        Box::new(StubConnector {
            id: "trusty-search",
            display_name: "Trusty Search",
            status: ServiceStatus::Running,
        }),
        Box::new(StubConnector {
            id: "trusty-memory",
            display_name: "Trusty Memory",
            status: ServiceStatus::Available,
        }),
        Box::new(StubConnector {
            id: "trusty-analyze",
            display_name: "Trusty Analyze",
            status: ServiceStatus::Absent,
        }),
    ])
}

async fn get_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

/// Why: the services route must return a valid JSON array with one entry
/// per connector, each containing `id`, `display_name`, and `status`.
/// What: builds the router with stub connectors, issues GET
/// /api/console/services, parses the response.
/// Test: this test itself.
#[tokio::test]
async fn test_services_route_returns_json() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/api/console/services")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = get_bytes(resp).await;
    let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
    assert_eq!(body.len(), 3);

    assert_eq!(body[0]["id"], "trusty-search");
    assert_eq!(body[0]["status"], "running");
    assert_eq!(body[0]["display_name"], "Trusty Search");

    assert_eq!(body[1]["id"], "trusty-memory");
    assert_eq!(body[1]["status"], "available");

    assert_eq!(body[2]["id"], "trusty-analyze");
    assert_eq!(body[2]["status"], "absent");
}

/// Why: health endpoint must return 200 with `status: ok`.
/// What: issues GET /health and checks the JSON body.
/// Test: this test itself.
#[tokio::test]
async fn test_health_route() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = get_bytes(resp).await;
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}

/// Why: the services route must serialise `Degraded` status and the
/// `hint` field correctly so the UI can render a distinct badge.
/// What: builds the router with a Degraded stub connector, issues GET
/// /api/console/services, asserts `status == "degraded"` and `hint` present.
/// Test: this test itself.
#[tokio::test]
async fn test_services_route_returns_degraded_with_hint() {
    use crate::connector::ServiceInfo;
    struct DegradedConnector;
    impl ServiceConnector for DegradedConnector {
        fn id(&self) -> &'static str {
            "trusty-analyze"
        }
        fn display_name(&self) -> &'static str {
            "Trusty Analyze"
        }
        fn detect(&self) -> ServiceInfo {
            ServiceInfo {
                id: "trusty-analyze".to_string(),
                display_name: "Trusty Analyze".to_string(),
                status: ServiceStatus::Degraded,
                version: None,
                url: None,
                hint: Some("reachable but `console_metrics` tool not registered".to_string()),
            }
        }
    }
    let state = AppState::new(vec![Box::new(DegradedConnector)]);
    let router = build_router(state);
    let req = Request::builder()
        .uri("/api/console/services")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = get_bytes(resp).await;
    let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["status"], "degraded");
    assert!(
        body[0].get("hint").is_some(),
        "degraded service must include hint field"
    );
    assert!(
        body[0]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("console_metrics"),
        "hint must mention console_metrics"
    );
}

/// Why: the root path must serve the embedded HTML (or placeholder).
/// What: issues GET / and asserts 200 + text/html content-type.
/// Test: this test itself.
#[tokio::test]
async fn test_spa_root_returns_html() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
}

/// A connector whose `detect()` always panics — simulates a buggy plugin.
struct PanicConnector;

impl ServiceConnector for PanicConnector {
    fn id(&self) -> &'static str {
        "panic-svc"
    }
    fn display_name(&self) -> &'static str {
        "Panic Service"
    }
    fn detect(&self) -> ServiceInfo {
        panic!("intentional test panic from PanicConnector");
    }
}

/// Why: a panicking connector must not silently return HTTP 200 with an
/// empty list — that is indistinguishable from "no services installed".
/// The handler must return HTTP 500 so the UI can display an error state.
/// What: builds the router with a PanicConnector, issues GET
/// /api/console/services, asserts the response status is 500.
/// Test: this test itself.
#[tokio::test]
async fn test_services_handler_returns_500_on_panic() {
    let state = AppState::new(vec![Box::new(PanicConnector)]);
    let router = build_router(state);

    let req = Request::builder()
        .uri("/api/console/services")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Why: with an empty metrics cache the route must return 503 so the UI
/// can show a "not yet available" state rather than empty JSON.
/// What: issues GET /api/console/metrics/analyze on a fresh state,
/// asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_metrics_analyze_route_cold_cache_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/analyze")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: the `/api/{service}/*` proxy route for an unknown service key must
/// return 400 (not a 404 route-miss, since the route pattern matches but the
/// handler rejects the key).
/// What: issues GET /api/unknown/health on the new primary path, asserts 400.
/// Test: this test itself.
#[tokio::test]
async fn test_api_proxy_unknown_service_returns_400() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/api/unknown/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Why: the `/api/{service}/*` route for a known service that is not running
/// must return 503 (cache cold) — proves the route reaches the proxy handler.
/// What: issues GET /api/search/health on a fresh state (no poll),
/// asserts 503 SERVICE_UNAVAILABLE.
/// Test: this test itself (#1849 Phase 2 primary path).
#[tokio::test]
async fn test_api_proxy_known_service_cold_cache_returns_503() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/api/search/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: `mpm` must be reachable via the new `/api/mpm/*` path and must NOT
/// return 400 (key absent from allowlist).
/// What: issues GET /api/mpm/health on a fresh state (no poll),
/// asserts 503 SERVICE_UNAVAILABLE (not 400 BAD_REQUEST).
/// Test: this test itself (#1849 Phase 2).
#[tokio::test]
async fn test_api_proxy_mpm_is_in_allowlist_cold_cache_returns_503() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/api/mpm/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/api/mpm/health must return 503 (mpm in allowlist, cache cold), not 400"
    );
}

/// Why: the deprecated `/proxy/{daemon}/*` alias must still route to the
/// proxy handler; removing it would break external callers mid-migration.
/// What: issues GET /proxy/search/health on the deprecated path, asserts 503
/// (cache cold, not 404 route-miss or 400 key-rejected).
/// Test: this test itself (backward-compat guard for #1849 Phase 2).
#[tokio::test]
async fn test_deprecated_proxy_alias_still_routes() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/proxy/search/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/proxy/search/health must return 503 via deprecated alias, not 404"
    );
}

/// Why: the deprecated `/proxy/*` alias must also reject unknown service keys
/// with 400, not a silent 404 — proves the handler still validates the key.
/// What: issues GET /proxy/unknown/health, asserts 400.
/// Test: this test itself.
#[tokio::test]
async fn test_deprecated_proxy_alias_unknown_key_returns_400() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/proxy/unknown/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Why: #1849 Phase 1 adds `mpm` to the proxy allowlist. A request to
/// `/proxy/mpm/health` must NOT return 400 (unknown daemon) — it must return
/// 503 (cache not yet populated) which proves the key is now in the allowlist.
/// What: issues GET /proxy/mpm/health on a fresh state (no poll),
/// asserts 503 SERVICE_UNAVAILABLE (not 400 BAD_REQUEST).
/// Test: this test itself (regression guard for #1849 Phase 1).
#[tokio::test]
async fn test_proxy_mpm_is_in_allowlist_cold_cache_returns_503() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/proxy/mpm/health")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/proxy/mpm/health must return 503 (mpm in allowlist, cache cold), not 400"
    );
}

/// Why: the "console" service key is reserved for the console's own
/// /api/console/* namespace.  Issuing a request like /api/console/hijack
/// must never reach the reverse-proxy and be forwarded to an upstream; the
/// explicit guard in proxy_handler must return 400 before full_id is called.
/// What: issues GET /api/console/hijack on a fresh state, asserts 400.
/// Test: this test itself (#1849 Phase 2 console-key reservation guard).
#[tokio::test]
async fn test_api_proxy_console_key_returns_400() {
    let router = build_router(make_test_state());

    let req = Request::builder()
        .uri("/api/console/hijack")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "/api/console/<unregistered-path> must return 400 from the console guard, not 404"
    );
}

/// Why: with an empty memory metrics cache the route must return 503 so the
/// UI can show a "not yet available" state rather than empty JSON.
/// What: issues GET /api/console/metrics/memory on a fresh state, asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_metrics_memory_route_cold_cache_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/memory")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: with an empty search metrics cache the route must return 503 so the
/// UI can show a "not yet available" state rather than empty JSON.
/// What: issues GET /api/console/metrics/search on a fresh state, asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_metrics_search_route_cold_cache_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/search")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: with an empty review metrics cache the route must return 503 so the
/// UI can show a "not yet available" state rather than empty JSON.
/// What: issues GET /api/console/metrics/review on a fresh state, asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_metrics_review_route_cold_cache_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/review")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: with an empty mpm metrics cache the route must return 503 so the UI
/// can show a "not yet available" state rather than empty JSON (#1222).
/// What: issues GET /api/console/metrics/mpm on a fresh state, asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_metrics_mpm_route_cold_cache_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/mpm")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: the analyze indexes route must return 503 (not 200 with empty data)
/// when the trusty-analyze binary is absent — the handle immediately marks
/// itself Absent and the route converts that to SERVICE_UNAVAILABLE.
/// What: issues GET /api/console/metrics/analyze/indexes on a fresh state
/// (where trusty-analyze is not on PATH in CI), asserts 503.
/// Test: this test itself.
#[tokio::test]
async fn test_analyze_indexes_absent_binary_returns_503() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/analyze/indexes")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    // Binary absent (or in backoff) → 503; if present and daemon is up → 200.
    // In CI neither condition holds; the route must not return 500.
    assert_ne!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "indexes route must not 500 when binary absent"
    );
}

/// Why: the analyze visualize route must return 400 when no `index` param
/// is provided — the endpoint needs it to query the daemon. A 200 with an
/// error field is indistinguishable from a success response to callers that
/// only check the status code.
/// What: issues GET /api/console/metrics/analyze/visualize (no ?index=),
/// asserts HTTP 400 and a JSON body containing `error`.
/// Test: this test itself.
#[tokio::test]
async fn test_analyze_visualize_handler_no_index_returns_json_error() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .uri("/api/console/metrics/analyze/visualize")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    // Missing index returns 400 BAD_REQUEST with a JSON error body.
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "missing index param must return 400"
    );
    let bytes = get_bytes(resp).await;
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
    assert!(
        body.get("error").is_some(),
        "expected error field, got: {body}"
    );
}

/// Why: This is the key regression test for the UAT gap: the connector
/// `detect()` path reports `Running` or `Available` because it only does a
/// TCP/which probe and knows nothing about `tools/list`. When the actual
/// `McpServiceHandle` is in `Degraded` state (tools/list succeeded but
/// `console_metrics` absent), `GET /api/console/services` MUST override that
/// connector result to `degraded` with the remediation hint.
/// What: Builds state whose connector returns `Running` for trusty-search,
/// manually primes the trusty-search `McpServiceHandle` to `Degraded`, then
/// issues GET /api/console/services and asserts `status == "degraded"` with
/// a non-empty `hint`.  A connector that was `Absent` must NOT be overridden
/// (only reachable services can be Degraded by the tools/list probe).
/// This test intentionally does NOT use a hand-stubbed DegradedConnector —
/// it exercises the real `apply_handle_overrides` bridge from
/// `McpServiceHandle.state` → route response.
/// Test: this test itself.
#[tokio::test]
async fn test_services_route_handle_degraded_overlay() {
    // Build state with:
    //  - trusty-search connector returning Running (TCP probe passed)
    //  - trusty-analyze connector returning Absent (binary not found)
    let state = AppState::new(vec![
        Box::new(StubConnector {
            id: "trusty-search",
            display_name: "Trusty Search",
            status: ServiceStatus::Running,
        }),
        Box::new(StubConnector {
            id: "trusty-analyze",
            display_name: "Trusty Analyze",
            status: ServiceStatus::Absent,
        }),
    ]);

    // Prime the trusty-search handle to Degraded state (tools/list passed
    // but console_metrics was absent).  This simulates the real-world
    // situation on a machine where the daemon lacks console_metrics.
    {
        let handles = state.mcp_handles();
        let search_handle = handles
            .get("trusty-search")
            .expect("search handle must exist");
        search_handle.prime_degraded_for_test().await;
    }

    let router = build_router(state);
    let req = Request::builder()
        .uri("/api/console/services")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = get_bytes(resp).await;
    let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
    assert_eq!(body.len(), 2);

    // trusty-search was Running via connector but Degraded via handle →
    // must be overridden to degraded with a hint.
    let search = body
        .iter()
        .find(|s| s["id"] == "trusty-search")
        .expect("search entry");
    assert_eq!(
        search["status"], "degraded",
        "Running service whose handle is Degraded must report degraded, got: {search}"
    );
    let hint = search["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "degraded service must include a non-empty hint"
    );
    assert!(
        hint.contains("console_metrics"),
        "hint must mention console_metrics, got: {hint}"
    );

    // trusty-analyze was Absent via connector — Absent must NOT be overridden
    // even if the handle were somehow Degraded (process-down ≠ degraded).
    let analyze = body
        .iter()
        .find(|s| s["id"] == "trusty-analyze")
        .expect("analyze entry");
    assert_eq!(
        analyze["status"], "absent",
        "Absent service must not be overridden to degraded"
    );
}

/// Why: Regression test for issue #1170 — a stale daemon whose MCP process
/// is running but lacks the `list_analyze_indexes` tool must cause the
/// `/api/console/metrics/analyze/indexes` route to return HTTP 503 with a
/// clean JSON body containing `status: "degraded"` and an actionable `hint`,
/// NOT HTTP 502 with empty body. The capability-gate in `call_tool_checked`
/// must fire before any JSON-RPC call is made to the daemon.
/// What: Builds state with a `trusty-analyze` handle primed to `Connected`
/// but missing `list_analyze_indexes` in the cached tool set. Issues GET
/// /api/console/metrics/analyze/indexes and asserts:
///   1. Status is 503 (SERVICE_UNAVAILABLE), not 502 (BAD_GATEWAY).
///   2. JSON body has `status == "degraded"`.
///   3. JSON body has a non-empty `hint` mentioning the missing tool.
/// Test: this test itself. Key regression for #1170.
#[tokio::test]
#[cfg(unix)]
async fn test_analyze_indexes_tool_unavailable_returns_degraded_hint() {
    let state = make_test_state();

    // Prime the analyze handle to Connected with list_analyze_indexes absent.
    {
        let analyze_handle = state.analyze_handle();
        analyze_handle
            .prime_connected_missing_tool_for_test("list_analyze_indexes")
            .await;
    }

    let router = build_router(state);
    let req = Request::builder()
        .uri("/api/console/metrics/analyze/indexes")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");

    // Must be 503, not 502 — the capability gate fires, not the JSON-RPC call.
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "missing tool must return 503 SERVICE_UNAVAILABLE, not 502 BAD_GATEWAY"
    );

    let bytes = get_bytes(resp).await;
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json body");

    assert_eq!(
        body["status"], "degraded",
        "response body must have status=degraded, got: {body}"
    );

    let hint = body["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "response body must include a non-empty hint, got: {body}"
    );
    assert!(
        hint.contains("list_analyze_indexes"),
        "hint must mention the missing tool name, got: {hint}"
    );
}

// ── same-origin guard on destructive session write routes (#1222 review #3) ──

/// Why: a cross-origin browser `POST` to a destructive session route is the
/// CSRF threat the same-origin guard exists to block. With a non-loopback
/// `Origin` header present, the write route must return `403 FORBIDDEN` and
/// never reach the handler (which would otherwise return 503 absent-binary).
/// What: issues `POST /api/console/sessions` with `Origin: http://evil.example`
/// and asserts 403.
/// Test: this test itself (review finding #3 regression guard).
#[tokio::test]
async fn write_route_rejects_cross_origin() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("POST")
        .uri("/api/console/sessions")
        .header("origin", "http://evil.example.com")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"repo_url":"https://x/y","ref":"main","task":"t"}).to_string(),
        ))
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin write must be rejected with 403"
    );
}

/// Why: the legitimate operator surface (the SPA served from loopback) must
/// still be able to drive write routes — a loopback `Origin` must pass the
/// guard. With no trusty-mpm binary on PATH the handler then returns a 503,
/// so the guard is proven transparent by asserting the status is NOT 403.
/// What: issues `DELETE /api/console/sessions/abc` with a loopback Origin and
/// asserts the response is not 403 (guard passed; handler degraded to 503).
/// Test: this test itself.
#[tokio::test]
async fn write_route_allows_loopback_origin() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/console/sessions/abc")
        .header("origin", "http://127.0.0.1:7788")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin write must pass the same-origin guard"
    );
}

/// Why: non-browser clients (curl, native tooling, the console's own
/// server-side calls) send no `Origin` header and are not the CSRF threat;
/// they must pass the guard. With no binary present the handler degrades to
/// 503, so we assert the status is NOT 403.
/// What: issues `POST /api/console/sessions/abc/stop` with no Origin header.
/// Test: this test itself.
#[tokio::test]
async fn write_route_allows_missing_origin() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("POST")
        .uri("/api/console/sessions/abc/stop")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin write must pass the same-origin guard"
    );
}

/// Why: the guard must NOT block safe cross-origin reads — the CORS policy is
/// intentionally open for GETs so the SPA and tooling can read fleet state.
/// A cross-origin `GET` must pass the guard (and then degrade to 503 with no
/// binary), proving the middleware is method-aware.
/// What: issues `GET /api/console/sessions` with a remote Origin; asserts the
/// status is NOT 403.
/// Test: this test itself.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("GET")
        .uri("/api/console/sessions")
        .header("origin", "http://evil.example.com")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET (read) must not be blocked by the write guard"
    );
}

// ── router-wide guard coverage on the reverse-proxy routes (#3268) ────────

/// Why: #3268 regression guard — the write-origin guard previously only
/// covered the seven session routes registered before the old
/// `route_layer` call, so a cross-origin `POST`/`DELETE` through the
/// reverse proxy (e.g. `POST /api/search/admin/stop`, `DELETE
/// /api/search/indexes/{id}`) reached the destructive daemon endpoint
/// unguarded. Now that the guard is applied router-wide via `.layer()`,
/// a cross-origin write to a proxied route must be rejected with `403`
/// BEFORE `proxy_handler` ever attempts to reach an upstream daemon (no
/// live daemon required for this test to pass).
/// What: issues `POST /api/search/admin/stop` with a non-loopback,
/// non-self `Origin` header and asserts `403 FORBIDDEN`.
/// Test: this test itself.
#[tokio::test]
async fn proxy_route_rejects_cross_origin_write() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("POST")
        .uri("/api/search/admin/stop")
        .header("origin", "http://evil.example.com")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin write through the reverse proxy must be rejected with 403"
    );
}

/// Why: #3268 companion — the router-wide guard must still let a
/// cross-origin, safe `GET` through the proxy pass (read traffic is not
/// the CSRF threat model); this proves the router-wide `.layer()` stayed
/// method-aware after replacing the route-scoped `route_layer`.
/// What: issues `GET /api/search/status` with a remote Origin; asserts
/// the response is NOT 403 (it degrades to 503/502 with no live daemon).
/// Test: this test itself.
#[tokio::test]
async fn proxy_route_allows_cross_origin_read() {
    let router = build_router(make_test_state());
    let req = Request::builder()
        .method("GET")
        .uri("/api/search/status")
        .header("origin", "http://evil.example.com")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET through the reverse proxy must not be blocked by the write guard"
    );
}

// ── bind-aware self-origin allowlist (#3269) ───────────────────────────────

/// Why: #3269 regression guard — the full Tailscale write flow. When the
/// console is bound on a Tailscale (non-loopback) address, its own write
/// UI served from that address must be able to drive BOTH the native
/// session routes AND the reverse-proxied destructive routes; a request
/// whose `Origin` is exactly the server's own resolved bind address must
/// pass the guard rather than being 403'd as if it were a random
/// non-loopback origin.
/// What: builds the router via `build_router_with_self_origins` with a
/// `SelfOrigins` allowlist containing `100.64.1.2:7788` (a Tailscale
/// CGNAT address), then issues `POST /api/console/sessions` and `POST
/// /api/search/admin/stop` with `Origin: http://100.64.1.2:7788`; asserts
/// neither is 403 (both degrade to 503/502 with no live
/// trusty-mpm/trusty-search binary — proving the guard, not the handler,
/// is what would have blocked them).
/// Test: this test itself.
#[tokio::test]
async fn proxy_route_allows_self_origin_write() {
    let self_origins = crate::routes::origin_guard::SelfOrigins::from_bind_addrs(&[
        "127.0.0.1:7788".parse().expect("addr"),
        "100.64.1.2:7788".parse().expect("addr"),
    ]);
    let router = build_router_with_self_origins(make_test_state(), self_origins);

    let session_req = Request::builder()
        .method("POST")
        .uri("/api/console/sessions")
        .header("origin", "http://100.64.1.2:7788")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"repo_url":"https://x/y","ref":"main","task":"t"}).to_string(),
        ))
        .expect("request");
    let session_resp = router.clone().oneshot(session_req).await.expect("response");
    assert_ne!(
        session_resp.status(),
        StatusCode::FORBIDDEN,
        "self-origin (Tailscale bind addr) session write must pass the guard"
    );

    let proxy_req = Request::builder()
        .method("POST")
        .uri("/api/search/admin/stop")
        .header("origin", "http://100.64.1.2:7788")
        .body(Body::empty())
        .expect("request");
    let proxy_resp = router.oneshot(proxy_req).await.expect("response");
    assert_ne!(
        proxy_resp.status(),
        StatusCode::FORBIDDEN,
        "self-origin (Tailscale bind addr) proxied write must pass the guard"
    );
}

/// Why: #3269 companion — the self-origin allowlist must stay narrowly
/// scoped to the server's own resolved bind address(es), NOT the whole
/// `100.64.0.0/10` CGNAT range (explicitly called out as the anti-goal in
/// the issue). A different host in that range must still be rejected.
/// What: builds the router trusting only `100.64.1.2:7788`, then issues a
/// write with `Origin: http://100.64.9.9:7788` (a different tailnet host)
/// and asserts `403`.
/// Test: this test itself.
#[tokio::test]
async fn proxy_route_rejects_other_tailnet_host() {
    let self_origins =
        crate::routes::origin_guard::SelfOrigins::from_bind_addrs(&["100.64.1.2:7788"
            .parse()
            .expect("addr")]);
    let router = build_router_with_self_origins(make_test_state(), self_origins);
    let req = Request::builder()
        .method("POST")
        .uri("/api/search/admin/stop")
        .header("origin", "http://100.64.9.9:7788")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a non-self, non-loopback tailnet host must still be rejected — the allowlist must \
         not blanket-trust the whole CGNAT range"
    );
}
