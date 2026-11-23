//! The console's own HTTP request and webhook counters (#10907, D95).
//!
//! Why: every other service reports `console_metrics`; the console, which
//! fronts the whole fleet's HTTP traffic, reported nothing about itself. The
//! TUI reads these over `console.console_metrics` on the console socket.
//! What: [`RequestMetrics`] keeps in-memory counters since process start, and
//! [`record_requests`] is the router-wide middleware that feeds it. Each
//! request is counted by matched route template, status and method, with
//! latency; requests to a proxied or UDS-bridged service are also counted per
//! upstream service; `POST /api/webhooks/{source}` is also counted per source.
//! [`RequestMetrics::report`] renders the counters as a
//! [`ConsoleMetricsReport`] whose `metrics` follows the `by_*` convention:
//! `by_route`, `by_upstream` and `by_source` are objects keyed by that
//! dimension's value, each holding a [`TrafficStats`] body.
//!
//! **Bounded keys.** Every key comes from a closed set, so a caller cannot
//! grow the maps: a route is the matched template (never the raw path) or
//! [`UNMATCHED_ROUTE`]; a method outside the standard set is [`OTHER_METHOD`];
//! an upstream comes from a fixed table; a webhook source outside the relay's
//! known sources is [`UNKNOWN_SOURCE`].
//!
//! Latency is measured to the response head, so a streamed body (SSE) counts
//! the time to its first byte, not the stream's lifetime.
//!
//! Test: `tests.rs` beside this file.

// See #10907

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Value, json};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth, make_report};
use trusty_common::console_rpc::CONSOLE_SERVICE;
use trusty_common::webhook_relay::{ANALYZE_SOURCE, REVIEW_SOURCE};

#[cfg(test)]
mod tests;

/// The route key for a request no route matched (a 404 from the fallback).
pub const UNMATCHED_ROUTE: &str = "unmatched";
/// The method key for any method outside the standard nine.
pub const OTHER_METHOD: &str = "OTHER";
/// The source key for a webhook naming a source the relay does not know.
pub const UNKNOWN_SOURCE: &str = "unknown";
/// The webhook ingress route template.
pub const WEBHOOK_ROUTE: &str = "/api/webhooks/{source}";
/// Bumped when the shape of the report's `metrics` changes.
pub const METRICS_SCHEMA_VERSION: u32 = 1;

/// Route templates whose upstream is a fixed service.
const FIXED_UPSTREAMS: &[(&str, &str)] = &[
    ("/api/search/", "trusty-search"),
    ("/proxy/search/", "trusty-search"),
    ("/api/memory/", "trusty-memory"),
    ("/proxy/memory/", "trusty-memory"),
    ("/api/analyze/", "trusty-analyze"),
    ("/proxy/analyze/", "trusty-analyze"),
    ("/api/secrets", "trusty-secrets"),
];

/// Route templates whose upstream is the proxy key in the second path segment.
const PROXY_ROUTES: &[&str] = &["/api/{service}/{*path}", "/proxy/{daemon}/{*path}"];

/// Counts and latency for one key of one breakdown.
///
/// Why: one body shape for routes, upstreams and webhook sources, so a reader
/// renders every `by_*` entry the same way.
/// What: `requests` is the total; `by_status` and `by_method` split it;
/// latency is kept as a total and a maximum in microseconds and rendered as
/// `latency_ms: {mean, max}` by [`TrafficStats::to_json`].
/// Test: `record_splits_one_request_across_every_breakdown`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TrafficStats {
    /// Requests counted.
    pub requests: u64,
    /// Requests per HTTP status code, keyed by the decimal code.
    pub by_status: BTreeMap<String, u64>,
    /// Requests per HTTP method.
    pub by_method: BTreeMap<String, u64>,
    /// Summed latency, microseconds (saturating).
    pub latency_total_us: u64,
    /// Largest single latency, microseconds.
    pub latency_max_us: u64,
}

impl TrafficStats {
    fn add(&mut self, status: &str, method: &str, elapsed_us: u64) {
        self.requests += 1;
        *self.by_status.entry(status.to_string()).or_default() += 1;
        *self.by_method.entry(method.to_string()).or_default() += 1;
        self.latency_total_us = self.latency_total_us.saturating_add(elapsed_us);
        self.latency_max_us = self.latency_max_us.max(elapsed_us);
    }

    /// The wire form: counts plus `latency_ms: {mean, max}` in milliseconds.
    pub fn to_json(&self) -> Value {
        let mean_us = self
            .latency_total_us
            .checked_div(self.requests)
            .unwrap_or_default();
        json!({
            "requests": self.requests,
            "by_status": self.by_status,
            "by_method": self.by_method,
            "latency_ms": {
                "mean": mean_us as f64 / 1000.0,
                "max": self.latency_max_us as f64 / 1000.0,
            },
        })
    }
}

/// One finished request, already reduced to bounded keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSample {
    /// Matched route template, or [`UNMATCHED_ROUTE`].
    pub route: String,
    /// Normalised method (see [`method_key`]).
    pub method: &'static str,
    /// Response status code.
    pub status: u16,
    /// Time to the response head.
    pub elapsed: Duration,
    /// The proxied or bridged service, when the route has one.
    pub upstream: Option<&'static str>,
    /// The webhook source, for the webhook ingress route only.
    pub webhook_source: Option<&'static str>,
}

impl RequestSample {
    /// Reduce a request to bounded keys.
    ///
    /// Why: the middleware hands over raw request parts; every key that
    /// reaches a map must come from a closed set (module docs).
    /// What: `route` is `matched` or [`UNMATCHED_ROUTE`]; the upstream comes
    /// from [`upstream_for`]; the source from [`webhook_source_for`].
    /// Test: `classify_maps_unbounded_input_onto_closed_sets`.
    pub fn classify(
        matched: Option<&str>,
        path: &str,
        method: &Method,
        status: u16,
        elapsed: Duration,
    ) -> Self {
        let upstream = matched.and_then(|route| upstream_for(route, path));
        let webhook_source = (matched == Some(WEBHOOK_ROUTE)).then(|| webhook_source_for(path));
        Self {
            route: matched.unwrap_or(UNMATCHED_ROUTE).to_string(),
            method: method_key(method),
            status,
            elapsed,
            upstream,
            webhook_source,
        }
    }
}

/// The method key: one of the nine standard methods, else [`OTHER_METHOD`].
pub fn method_key(method: &Method) -> &'static str {
    [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::HEAD,
        Method::OPTIONS,
        Method::CONNECT,
        Method::TRACE,
    ]
    .iter()
    .zip([
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "CONNECT", "TRACE",
    ])
    .find_map(|(known, key)| (known == method).then_some(key))
    .unwrap_or(OTHER_METHOD)
}

/// The upstream service a matched route forwards to, if any.
///
/// What: a fixed-prefix bridge route names its service; a generic proxy route
/// names the service its key maps to through the proxy allowlist; an unknown
/// proxy key (which the proxy answers `404`) and every console-local route
/// have no upstream.
/// Test: `classify_maps_unbounded_input_onto_closed_sets`.
pub fn upstream_for(route: &str, path: &str) -> Option<&'static str> {
    if let Some((_, id)) = FIXED_UPSTREAMS
        .iter()
        .find(|(prefix, _)| route.starts_with(prefix))
    {
        return Some(id);
    }
    if PROXY_ROUTES.contains(&route) {
        return path
            .split('/')
            .nth(2)
            .and_then(crate::proxy::routes::full_id);
    }
    None
}

/// The webhook source a `POST /api/webhooks/{source}` path names, bounded to
/// the relay's known sources; anything else is [`UNKNOWN_SOURCE`].
pub fn webhook_source_for(path: &str) -> &'static str {
    let named = path.strip_prefix("/api/webhooks/").unwrap_or_default();
    [REVIEW_SOURCE, ANALYZE_SOURCE]
        .into_iter()
        .find(|known| *known == named)
        .unwrap_or(UNKNOWN_SOURCE)
}

#[derive(Debug, Default)]
struct Counters {
    totals: TrafficStats,
    by_route: BTreeMap<String, TrafficStats>,
    by_upstream: BTreeMap<&'static str, TrafficStats>,
    by_source: BTreeMap<&'static str, TrafficStats>,
}

/// The console's request counters since process start.
///
/// Why: shared by the HTTP middleware (writer) and the UDS method (reader).
/// What: one mutex over plain maps; a record is a handful of map updates, so
/// the lock is never held across an await.
/// Test: `tests.rs` beside this file.
#[derive(Debug, Default)]
pub struct RequestMetrics {
    counters: Mutex<Counters>,
}

impl RequestMetrics {
    /// An empty set of counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// The counters, recovering them from a poisoned lock.
    ///
    /// Why: the guarded data is plain counters with no cross-field invariant a
    /// panicking writer could leave half-made, so dropping metrics forever
    /// after one panic would lose information and protect nothing.
    /// Test: `a_poisoned_lock_keeps_counting`.
    fn lock(&self) -> MutexGuard<'_, Counters> {
        self.counters.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("request metrics lock was poisoned; recovering the counters");
            poisoned.into_inner()
        })
    }

    /// Count one finished request in every breakdown it belongs to.
    ///
    /// Test: `record_splits_one_request_across_every_breakdown`.
    pub fn record(&self, sample: &RequestSample) {
        let status = sample.status.to_string();
        let elapsed_us = u64::try_from(sample.elapsed.as_micros()).unwrap_or(u64::MAX);
        let mut counters = self.lock();
        counters.totals.add(&status, sample.method, elapsed_us);
        counters
            .by_route
            .entry(sample.route.clone())
            .or_default()
            .add(&status, sample.method, elapsed_us);
        if let Some(upstream) = sample.upstream {
            counters.by_upstream.entry(upstream).or_default().add(
                &status,
                sample.method,
                elapsed_us,
            );
        }
        if let Some(source) = sample.webhook_source {
            counters
                .by_source
                .entry(source)
                .or_default()
                .add(&status, sample.method, elapsed_us);
        }
    }

    /// The counters as the report's `metrics` payload.
    ///
    /// What: `{uptime_secs, totals, by_route, by_upstream, by_source}`; every
    /// `by_*` value is an object of [`TrafficStats::to_json`] bodies.
    /// Test: `report_follows_the_by_convention`.
    pub fn metrics_json(&self, uptime_secs: u64) -> Value {
        fn table<K: AsRef<str>>(map: &BTreeMap<K, TrafficStats>) -> Value {
            Value::Object(
                map.iter()
                    .map(|(key, stats)| (key.as_ref().to_string(), stats.to_json()))
                    .collect(),
            )
        }
        let counters = self.lock();
        json!({
            "uptime_secs": uptime_secs,
            "totals": counters.totals.to_json(),
            "by_route": table(&counters.by_route),
            "by_upstream": table(&counters.by_upstream),
            "by_source": table(&counters.by_source),
        })
    }

    /// The `console.console_metrics` answer.
    ///
    /// Why: D95 — the console reports itself with the same report type every
    /// other service's `console_metrics` returns.
    /// What: service id [`CONSOLE_SERVICE`], status `ok` (the console is
    /// answering), [`METRICS_SCHEMA_VERSION`], and [`Self::metrics_json`].
    /// Test: `report_follows_the_by_convention`.
    pub fn report(&self, uptime_secs: u64) -> ConsoleMetricsReport {
        make_report(
            CONSOLE_SERVICE,
            "Console",
            env!("CARGO_PKG_VERSION"),
            ServiceHealth::Ok,
            self.metrics_json(uptime_secs),
            METRICS_SCHEMA_VERSION,
        )
    }
}

/// Router-wide middleware: time the request and count it.
///
/// Why: one layer sees every route, including the merged webhook and secrets
/// sub-routers and the 404 fallback, so no handler has to remember to count.
/// What: reads the matched template, path and method, runs the inner service,
/// then records a [`RequestSample`] built by [`RequestSample::classify`].
/// Test: `router_counts_requests_by_route_upstream_and_source`.
pub async fn record_requests(
    State(metrics): State<Arc<RequestMetrics>>,
    request: Request,
    next: Next,
) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    metrics.record(&RequestSample::classify(
        matched.as_deref(),
        &path,
        &method,
        response.status().as_u16(),
        started.elapsed(),
    ));
    response
}
