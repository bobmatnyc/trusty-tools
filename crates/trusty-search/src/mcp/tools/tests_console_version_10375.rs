//! `console_metrics` reports the serving daemon's version (#10375).
//!
//! Why: the report carried the bridge binary's compile-time version, so after
//! a launchd restart onto a newer binary it lagged `search_health`, which
//! reports the daemon's own.
//! What: drives `console_metrics` through `dispatch` against a mock daemon
//! whose `search.health` names a version that differs from this build's.
//! Test: this file.

use serde_json::{json, Value};

use super::test_daemon::{mock_daemon, unreachable_server};
use super::tests::req;
use super::McpServer;

/// Call `console_metrics` and return the parsed report.
async fn metrics_report(server: &McpServer) -> Value {
    let resp = server
        .dispatch(req(
            "tools/call",
            json!({ "name": "console_metrics", "arguments": {} }),
        ))
        .await;
    let result = resp.result.expect("tools/call always returns a result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("a text content node");
    serde_json::from_str(text).unwrap_or_else(|e| panic!("report is JSON: {e}\n{text}"))
}

/// The report names the daemon's version, not the bridge's.
#[tokio::test]
async fn console_metrics_reports_the_daemons_version() {
    let daemon = mock_daemon(|method, _| match method {
        "search.health" => Ok(json!({ "status": "ok", "version": "999.0.0", "indexes": 0 })),
        _ => Ok(json!({ "indexes": [] })),
    })
    .await;
    let report = metrics_report(&daemon.server()).await;
    assert_ne!(env!("CARGO_PKG_VERSION"), "999.0.0");
    assert_eq!(report["version"], "999.0.0", "report: {report}");
}

/// With no daemon to ask, the report falls back to the bridge's own version.
#[tokio::test]
async fn console_metrics_falls_back_to_the_bridge_version_when_unreachable() {
    let report = metrics_report(&unreachable_server()).await;
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"), "{report}");
}
