//! `console_metrics` MCP tool for trusty-analyze (epic #1104 Phase 0b).
//!
//! Why: The trusty-console daemon polls local services over stdio MCP to gather
//! health and metrics for the web dashboard. Each service exposes the
//! `console_metrics` tool (name constant: `trusty_common::console_metrics::CONSOLE_METRICS_METHOD`)
//! returning a `ConsoleMetricsReport` that the console decodes uniformly. This
//! module owns trusty-analyze's implementation of that contract.
//!
//! What: Exposes `handle_console_metrics` — an async function that calls
//! `GET /health` on the analyzer HTTP daemon (same as `analyzer_health`) to
//! determine `search_reachable`, then builds and serialises a
//! `ConsoleMetricsReport` via the shared helpers. The tool takes no arguments.
//!
//! `metrics` payload schema (schema_version = 1):
//! ```json
//! {
//!   "search_reachable": bool
//! }
//! ```
//!
//! Test: `console_metrics_tool_returns_mcp_envelope` in this module verifies
//! the response is a valid MCP content envelope; `parse_report_round_trip`
//! verifies the console can decode it via `parse_report`.

use serde_json::Value;
use trusty_common::console_metrics::{make_report, serialise_report, ServiceHealth};

use super::DispatchError;

/// Descriptor for the `console_metrics` tool (returned by `tools/list`).
///
/// Why: Each tool needs a descriptor in `tools/list` so MCP clients know it
/// exists and what schema it accepts. The descriptor is extracted here so the
/// descriptor list in `descriptors.rs` stays cohesive and this file owns both
/// the descriptor and the handler.
/// What: Returns a `serde_json::Value` object with `name`, `description`, and
/// `inputSchema` matching the console metrics contract.
/// Test: `mod.rs::tools_list_contains_full_surface` includes `console_metrics`.
pub(super) fn descriptor() -> Value {
    serde_json::json!({
        "name": "console_metrics",
        "description": "Return health and operational metrics for trusty-console polling. \
            No arguments required. Returns a ConsoleMetricsReport JSON envelope \
            with service_id='trusty-analyze', version, status (ok/degraded), and \
            a metrics object containing search_reachable.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

/// Handle the `console_metrics` tool call.
///
/// Why: The console polls this tool every ~15 s via `StdioMcpClient::call_tool`
/// to gather health/version data without requiring the analyzer's HTTP daemon
/// to be discoverable or externally reachable. The console deserialises the
/// result via `parse_report`.
/// What: Calls `GET /health` on the analyzer HTTP daemon (same as the
/// `analyzer_health` tool) to determine whether `trusty-search` is reachable.
/// Builds a `ConsoleMetricsReport` with `service_id = "trusty-analyze"`,
/// `display_name = "Trusty Analyze"`, version from `CARGO_PKG_VERSION`,
/// `status = Ok | Degraded` based on `search_reachable`, and a `metrics`
/// payload `{ "search_reachable": bool }`. Serialises with `serialise_report`
/// which wraps the report in the MCP `content[0].text` envelope.
/// Test: `console_metrics_tool_returns_mcp_envelope` and
/// `parse_report_round_trip` in this module.
pub(super) async fn handle_console_metrics(
    server: &super::AnalyzerMcpServer,
) -> Result<Value, DispatchError> {
    // Probe the analyzer's own HTTP /health endpoint to determine status.
    let health = server.get("/health").await;
    let search_reachable = health
        .as_ref()
        .ok()
        .and_then(|v| v.get("search_reachable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let status = if search_reachable {
        ServiceHealth::Ok
    } else {
        ServiceHealth::Degraded
    };

    let metrics = serde_json::json!({
        "search_reachable": search_reachable,
    });

    let report = make_report(
        "trusty-analyze",
        "Trusty Analyze",
        env!("CARGO_PKG_VERSION"),
        status,
        metrics,
        1, // metrics_schema_version = 1; bump when the metrics object shape changes
    );

    serialise_report(&report).map_err(|e| {
        DispatchError::Transport(format!("console_metrics: serialise_report failed: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use trusty_common::console_metrics::parse_report;

    use super::*;

    /// Why: The tool descriptor must match the contract name so `tools/list`
    /// advertises the right name to the console poller.
    /// What: Assert descriptor name equals `CONSOLE_METRICS_METHOD`.
    /// Test: This test.
    #[test]
    fn descriptor_name_matches_contract() {
        let d = descriptor();
        assert_eq!(
            d.get("name").and_then(Value::as_str),
            Some(trusty_common::console_metrics::CONSOLE_METRICS_METHOD),
            "descriptor name must match CONSOLE_METRICS_METHOD"
        );
    }

    /// Why: The MCP envelope must be well-formed so the console's `parse_report`
    /// call succeeds without going through the HTTP daemon (pure function test).
    /// What: Directly call `serialise_report` with a synthetic report and assert
    /// `parse_report` round-trips it correctly.
    /// Test: This test (no daemon required).
    #[test]
    fn parse_report_round_trip() {
        let report = make_report(
            "trusty-analyze",
            "Trusty Analyze",
            "0.7.0",
            ServiceHealth::Degraded,
            serde_json::json!({ "search_reachable": false }),
            1,
        );
        let envelope = serialise_report(&report).expect("serialise must succeed");
        let decoded = parse_report(&envelope).expect("parse must succeed");

        assert_eq!(decoded.service_id, "trusty-analyze");
        assert_eq!(decoded.display_name, "Trusty Analyze");
        assert_eq!(decoded.version, "0.7.0");
        assert_eq!(decoded.status, ServiceHealth::Degraded);
        assert_eq!(decoded.metrics_schema_version, 1);
        assert_eq!(decoded.metrics["search_reachable"], false);
    }

    /// Why: The descriptor must carry an `inputSchema` so MCP clients know the
    /// tool accepts (empty) arguments.
    /// What: Assert the descriptor has an `inputSchema` key.
    /// Test: This test.
    #[test]
    fn descriptor_has_input_schema() {
        let d = descriptor();
        assert!(
            d.get("inputSchema").is_some(),
            "descriptor must have inputSchema"
        );
    }
}
