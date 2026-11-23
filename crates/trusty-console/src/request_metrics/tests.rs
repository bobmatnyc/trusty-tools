//! Tests for the console's request counters (#10907).
//!
//! Why: the counters feed `console.console_metrics`; a wrong key, an unbounded
//! key, or a request the middleware never sees would each mislead the TUI.
//! What: unit tests on classification and recording, then the real router
//! driven with `tower::ServiceExt::oneshot`.
//! Test: this module IS the suite for `request_metrics/mod.rs`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::*;
use crate::server::{AppState, build_router_with_webhooks};
use crate::webhook::WebhookIngress;
use crate::webhook::spool::Spool;

fn sample(route: Option<&str>, path: &str, method: Method, status: u16) -> RequestSample {
    RequestSample::classify(route, path, &method, status, Duration::from_millis(4))
}

/// Why: one request must land in the totals, its route, its upstream and its
/// webhook source, with status, method and latency each split out.
/// What: records two samples and reads the counters back.
/// Test: this test itself.
#[test]
fn record_splits_one_request_across_every_breakdown() {
    let metrics = RequestMetrics::new();
    metrics.record(&sample(
        Some("/api/search/{*path}"),
        "/api/search/indexes",
        Method::GET,
        200,
    ));
    let mut slow = sample(
        Some("/api/search/{*path}"),
        "/api/search/indexes",
        Method::POST,
        502,
    );
    slow.elapsed = Duration::from_millis(10);
    metrics.record(&slow);

    let counters = metrics.lock();
    let route = &counters.by_route["/api/search/{*path}"];
    assert_eq!(route.requests, 2);
    assert_eq!(route.by_status["200"], 1);
    assert_eq!(route.by_status["502"], 1);
    assert_eq!(route.by_method["GET"], 1);
    assert_eq!(route.by_method["POST"], 1);
    assert_eq!(route.latency_total_us, 14_000);
    assert_eq!(route.latency_max_us, 10_000);
    assert_eq!(counters.by_upstream["trusty-search"], *route);
    assert_eq!(counters.totals, *route);
    assert!(counters.by_source.is_empty(), "not a webhook route");
}

/// Why: every map key must come from a closed set (module docs), and each
/// fallback key is a branch of its own.
/// What: a table of raw inputs and the keys they must reduce to.
/// Test: this test itself.
#[test]
fn classify_maps_unbounded_input_onto_closed_sets() {
    // Unmatched: a raw path never becomes a route key.
    let unmatched = sample(None, "/x/1/2", Method::GET, 404);
    assert_eq!(unmatched.route, UNMATCHED_ROUTE);
    assert_eq!(unmatched.upstream, None);
    // An extension method is OTHER; a standard one keeps its name.
    let odd = Method::from_bytes(b"PROPFIND").expect("extension method");
    assert_eq!(
        sample(Some("/health"), "/health", odd, 405).method,
        OTHER_METHOD
    );
    assert_eq!(
        sample(Some("/health"), "/health", Method::DELETE, 405).method,
        "DELETE"
    );

    // (matched template, raw path, upstream, webhook source)
    type Case = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
    );
    let cases: &[Case] = &[
        // Fixed bridges.
        (
            "/proxy/memory/{*path}",
            "/proxy/memory/p",
            Some("trusty-memory"),
            None,
        ),
        (
            "/api/analyze/{*path}",
            "/api/analyze/a",
            Some("trusty-analyze"),
            None,
        ),
        (
            "/api/secrets/list",
            "/api/secrets/list",
            Some("trusty-secrets"),
            None,
        ),
        // Generic proxy: the key decides, through the proxy allowlist.
        (
            "/api/{service}/{*path}",
            "/api/review/x",
            Some("trusty-review"),
            None,
        ),
        (
            "/proxy/{daemon}/{*path}",
            "/proxy/agents/x",
            Some("trusty-agents"),
            None,
        ),
        ("/api/{service}/{*path}", "/api/whatever/x", None, None),
        // Console-local route: no upstream.
        ("/api/console/services", "/api/console/services", None, None),
        // Webhooks: a known source, and an unknown one.
        (WEBHOOK_ROUTE, "/api/webhooks/review", None, Some("review")),
        (
            WEBHOOK_ROUTE,
            "/api/webhooks/attacker-chosen",
            None,
            Some(UNKNOWN_SOURCE),
        ),
    ];
    for (matched, path, upstream, source) in cases {
        let got = sample(Some(matched), path, Method::POST, 200);
        assert_eq!(got.route, *matched, "{path}");
        assert_eq!(got.method, "POST", "{path}");
        assert_eq!(got.upstream, *upstream, "{path}");
        assert_eq!(got.webhook_source, *source, "{path}");
    }
}

/// Why: the poisoned-lock arm recovers instead of dropping metrics.
/// What: poisons the lock from a panicking thread, then records and reads.
/// Test: this test itself.
#[test]
fn a_poisoned_lock_keeps_counting() {
    let metrics = Arc::new(RequestMetrics::new());
    let poisoner = Arc::clone(&metrics);
    let joined = std::thread::spawn(move || {
        let _guard = poisoner.counters.lock().expect("first lock");
        panic!("poison the request-metrics lock");
    })
    .join();
    assert!(joined.is_err(), "the thread must have panicked");
    assert!(metrics.counters.is_poisoned());

    metrics.record(&sample(Some("/health"), "/health", Method::GET, 200));
    assert_eq!(metrics.metrics_json(0)["totals"]["requests"], 1);
}

/// Why: the TUI reads the report the same way it reads every service's
/// `console_metrics`, and renders each `by_*` key as a nested table.
/// What: checks the report's identity fields and the payload's shape.
/// Test: this test itself.
#[test]
fn report_follows_the_by_convention() {
    let metrics = RequestMetrics::new();
    metrics.record(&sample(
        Some(WEBHOOK_ROUTE),
        "/api/webhooks/analyze",
        Method::POST,
        202,
    ));

    let report = metrics.report(42);
    assert_eq!(report.service_id, "trusty-console");
    assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(report.metrics_schema_version, METRICS_SCHEMA_VERSION);
    let m = &report.metrics;
    assert_eq!(m["uptime_secs"], 42);
    for key in ["by_route", "by_upstream", "by_source"] {
        assert!(m[key].is_object(), "{key} must be an object");
    }
    let source = &m["by_source"]["analyze"];
    assert_eq!(source["requests"], 1);
    assert_eq!(source["by_status"]["202"], 1);
    assert_eq!(source["latency_ms"]["mean"], 4.0);
    assert_eq!(source["latency_ms"]["max"], 4.0);
    assert_eq!(m["by_upstream"], serde_json::json!({}));
}

async fn send(router: &axum::Router, method: Method, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    router
        .clone()
        .oneshot(request)
        .await
        .expect("router is infallible")
        .status()
}

/// Why: the middleware must see every request the console serves — local
/// routes, bridges, the generic proxy, the merged webhook router and the 404
/// fallback — and file each under the right keys.
/// What: drives the real router with one request of each kind, then reads
/// the payload the UDS method serves.
/// Test: this test itself.
#[tokio::test]
async fn router_counts_requests_by_route_upstream_and_source() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(Vec::new()).with_memory_socket(tmp.path().join("absent.sock"));
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = WebhookIngress::new(spool, String::new(), "KEY_ENV".to_string(), Vec::new());
    let router = build_router_with_webhooks(state.clone(), Default::default(), ingress);

    assert_eq!(send(&router, Method::GET, "/health").await, StatusCode::OK);
    let unmatched = send(&router, Method::GET, "/no/such/route").await;
    let memory = send(&router, Method::GET, "/api/memory/palaces").await;
    let agents = send(&router, Method::GET, "/api/agents/x").await;
    let review_hook = send(&router, Method::POST, "/api/webhooks/review").await;
    let bogus_hook = send(&router, Method::POST, "/api/webhooks/bogus").await;

    let m = state.request_metrics().metrics_json(0);
    let status = |s: StatusCode| s.as_u16().to_string();
    assert_eq!(m["totals"]["requests"], 6);
    assert_eq!(m["by_route"]["/health"]["by_status"]["200"], 1);
    assert_eq!(
        m["by_route"][UNMATCHED_ROUTE]["by_status"][status(unmatched)],
        1
    );
    assert_eq!(m["by_route"][WEBHOOK_ROUTE]["by_method"]["POST"], 2);
    assert_eq!(
        m["by_upstream"]["trusty-memory"]["by_status"][status(memory)],
        1
    );
    assert_eq!(
        m["by_upstream"]["trusty-agents"]["by_status"][status(agents)],
        1
    );
    assert_eq!(
        m["by_source"]["review"]["by_status"][status(review_hook)],
        1
    );
    assert_eq!(
        m["by_source"][UNKNOWN_SOURCE]["by_status"][status(bogus_hook)],
        1
    );
    let upstreams = m["by_upstream"].as_object().expect("object");
    assert_eq!(
        upstreams.len(),
        2,
        "only bridged/proxied routes: {upstreams:?}"
    );
    assert!(
        m["by_route"]
            .as_object()
            .expect("object")
            .keys()
            .all(|k| k != "/no/such/route")
    );
    assert!(matches!(
        m["by_route"]["/health"]["latency_ms"]["max"],
        Value::Number(_)
    ));
}
