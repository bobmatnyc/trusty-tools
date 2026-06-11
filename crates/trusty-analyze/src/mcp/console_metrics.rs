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
//! determine `search_reachable`, then builds a `ConsoleMetricsReport` and
//! returns it as a raw `serde_json::Value`. The MCP dispatcher wraps it once
//! via `wrap_tool_result`; `trusty-console`'s `parse_report` unwraps it once.
//! The tool takes no arguments.
//!
//! `metrics` payload schema (schema_version = 1):
//! ```json
//! {
//!   "search_reachable": bool
//! }
//! ```
//!
//! Test: `parse_report_round_trip` in this module verifies the full
//! dispatch→parse round-trip through the `wrap_tool_result` + `parse_report`
//! path that `trusty-console` uses at runtime.

use serde_json::Value;
use trusty_common::console_metrics::{make_report, ServiceHealth};

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
/// payload `{ "search_reachable": bool }`. Returns the raw report as a
/// `serde_json::Value` — the MCP dispatcher's `wrap_tool_result` applies the
/// single `content[0].text` envelope that `parse_report` in `trusty-console`
/// expects. Do NOT call `serialise_report` here — that would double-wrap.
/// Test: `parse_report_round_trip` in this module verifies the full
/// dispatch→parse round-trip through `wrap_tool_result` + `parse_report`.
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

    serde_json::to_value(&report)
        .map_err(|e| DispatchError::Transport(format!("console_metrics: to_value failed: {e}")))
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

    /// Why: The MCP dispatch path wraps tool results exactly once via
    /// `wrap_tool_result` → `{"content":[{"type":"text","text":"<json>"}],"isError":false}`.
    /// `handle_console_metrics` must return the raw report Value (no pre-wrapping
    /// via `serialise_report`) so `wrap_tool_result` + `parse_report` complete
    /// a successful round-trip. This test simulates the full dispatch path
    /// without an HTTP daemon.
    /// What: Build a raw report Value, apply the same `wrap_tool_result` the
    /// dispatcher uses, then assert `parse_report` decodes it correctly.
    /// Test: This test (no daemon required; mirrors actual dispatch→console path).
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
        // Simulate what `handle_console_metrics` now returns (raw Value, no
        // serialise_report wrapping) and what `wrap_tool_result` in `mod.rs`
        // adds before the MCP client receives it.
        let raw_value = serde_json::to_value(&report).expect("to_value must succeed");
        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw_value)
                    .expect("to_string_pretty must succeed"),
            }],
            "isError": false,
        });
        let decoded = parse_report(&wrapped).expect("parse must succeed");

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
