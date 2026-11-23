//! #10908: `console_metrics` over a service's socket, against stub peers.

use serde_json::json;
use trusty_common::console_metrics::{ServiceHealth, make_report, serialise_report};

use super::analyze_probe_tests::{DEADLINE, Peer, serve, socket_dir};
use super::metrics_uds::fetch_uds_metrics;
use super::services::{MetricsSource, ServiceId};

/// One newline-terminated JSON-RPC answer carrying `result`, leaked so the
/// stub peer can hold it for the test's life.
fn answer(result: serde_json::Value) -> &'static [u8] {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string() + "\n";
    Box::leak(frame.into_boxed_str()).as_bytes()
}

fn events_report() -> trusty_common::console_metrics::ConsoleMetricsReport {
    make_report(
        "trusty-events",
        "Events",
        "0.2.0",
        ServiceHealth::Ok,
        json!({"events": 3}),
        1,
    )
}

#[test]
fn metrics_sources_prefer_uds_for_events_and_mpm() {
    assert!(matches!(
        ServiceId::Events.metrics_source(),
        Some(MetricsSource::Uds("events.console_metrics"))
    ));
    assert_eq!(
        ServiceId::Mpm.metrics_source(),
        Some(MetricsSource::UdsMcpDispatch)
    );
    assert!(matches!(
        ServiceId::Search.metrics_source(),
        Some(MetricsSource::Stdio("trusty-search", _))
    ));
    assert_eq!(ServiceId::Console.metrics_source(), None);
}

#[tokio::test]
async fn events_console_metrics_over_uds_yields_a_report() {
    let (_dir, socket) = socket_dir();
    let report = events_report();
    serve(
        &socket,
        Peer::Reply(answer(serde_json::to_value(&report).expect("encode"))),
    );
    let source = ServiceId::Events.metrics_source().expect("a source");
    let got = fetch_uds_metrics(source, &socket, DEADLINE)
        .await
        .expect("a report");
    assert_eq!(got.service_id, "trusty-events");
    assert_eq!(got.metrics, json!({"events": 3}));
}

#[tokio::test]
async fn mpm_console_metrics_over_mcp_dispatch_yields_a_report() {
    let (_dir, socket) = socket_dir();
    let mut report = events_report();
    report.service_id = "trusty-mpm".into();
    let envelope = json!({"jsonrpc": "2.0", "id": 1,
        "result": serialise_report(&report).expect("envelope")});
    serve(&socket, Peer::Reply(answer(envelope)));
    let got = fetch_uds_metrics(MetricsSource::UdsMcpDispatch, &socket, DEADLINE)
        .await
        .expect("a report");
    assert_eq!(got.service_id, "trusty-mpm");
}

#[tokio::test]
async fn a_uds_metrics_error_answer_is_unavailable_with_its_reason() {
    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"unknown method\"}}\n"),
    );
    let err = fetch_uds_metrics(
        MetricsSource::Uds("events.console_metrics"),
        &socket,
        DEADLINE,
    )
    .await
    .expect_err("an error answer is no report");
    assert!(err.contains("-32601"), "{err}");
}
