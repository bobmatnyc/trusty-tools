//! `console_metrics` over a service's Unix socket (#10908).
//!
//! Why: owner ruling D95 has each service report its metrics over UDS, pulled
//! by the reader (ADR-0032). The Events tile has no stdio MCP command at all,
//! so without this its service view could never show a report; MPM serves
//! the same tool through `mpm.mcp.dispatch` on its socket.
//! What: [`fetch_uds_metrics`] makes one bounded framed call for a
//! [`MetricsSource::Uds`] (the reply is the report) or a
//! [`MetricsSource::UdsMcpDispatch`] (the reply is an MCP response whose
//! `result` is a `tools/call` envelope, decoded by [`parse_report`]).
//! Test: `events_console_metrics_over_uds_yields_a_report`,
//! `mpm_console_metrics_over_mcp_dispatch_yields_a_report`,
//! `a_uds_metrics_error_answer_is_unavailable_with_its_reason`.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::console_metrics::{CONSOLE_METRICS_METHOD, ConsoleMetricsReport, parse_report};
use trusty_common::uds::server::RpcResponse;

use super::services::MetricsSource;

/// The MPM daemon method that carries one whole MCP request.
pub const MPM_MCP_DISPATCH: &str = "mpm.mcp.dispatch";

/// One `console_metrics` call over `socket` for a UDS `source`.
///
/// Why (#10908): the reader pulls each service's report over its own socket.
/// What: builds the frame `source` needs, sends it under `deadline`, and
/// decodes the reply; every failure is an operator-facing `Err` string.
/// Test: `events_console_metrics_over_uds_yields_a_report`,
/// `mpm_console_metrics_over_mcp_dispatch_yields_a_report`,
/// `a_uds_metrics_error_answer_is_unavailable_with_its_reason`.
pub async fn fetch_uds_metrics(
    source: MetricsSource,
    socket: &Path,
    deadline: Duration,
) -> Result<ConsoleMetricsReport, String> {
    let (method, params) = match source {
        MetricsSource::Uds(method) => (method, json!({})),
        MetricsSource::UdsMcpDispatch => (
            MPM_MCP_DISPATCH,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": CONSOLE_METRICS_METHOD, "arguments": {}}}),
        ),
        MetricsSource::Stdio(binary, _) => {
            return Err(format!("{binary} serves console_metrics over stdio only"));
        }
    };
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let sent = tokio::time::timeout(
        deadline,
        trusty_common::uds::send_framed_request::<_, RpcResponse>(socket, &request, deadline),
    )
    .await;
    let response = match sent {
        Err(_) => return Err(format!("{method} timed out after {}s", deadline.as_secs())),
        Ok(Err(e)) => return Err(format!("{method}: {e}")),
        Ok(Ok(response)) => response,
    };
    if let Some(error) = response.error {
        return Err(format!(
            "{method} answered an error (code {}): {}",
            error.code, error.message
        ));
    }
    let body = response
        .result
        .ok_or_else(|| format!("{method} answered no result"))?;
    match source {
        MetricsSource::UdsMcpDispatch => mcp_report(&body),
        _ => {
            serde_json::from_value(body).map_err(|e| format!("{method} answer did not decode: {e}"))
        }
    }
}

/// The report inside one MCP response envelope.
fn mcp_report(envelope: &Value) -> Result<ConsoleMetricsReport, String> {
    if let Some(error) = envelope.get("error") {
        return Err(format!("console_metrics tool failed: {error}"));
    }
    let result = envelope
        .get("result")
        .ok_or_else(|| "MCP response carries no result".to_string())?;
    parse_report(result).map_err(|e| format!("{e:#}"))
}
