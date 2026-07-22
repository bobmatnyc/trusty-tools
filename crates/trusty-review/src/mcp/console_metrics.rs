//! `console_metrics` MCP tool for trusty-review (issue #1163).
//!
//! Why: The trusty-console daemon polls local services over stdio MCP to gather
//! health and metrics for the web dashboard. Each service exposes the
//! `console_metrics` tool (name constant: `trusty_common::console_metrics::CONSOLE_METRICS_METHOD`)
//! returning a `ConsoleMetricsReport` that the console decodes uniformly. This
//! module owns trusty-review's implementation of that contract.
//!
//! What: Exposes `descriptor` (tool descriptor for `tools/list`) and
//! `handle_console_metrics` — an async function that reuses the same
//! dep-reachability and inference probes as `review_health` to determine
//! service health, then builds a `ConsoleMetricsReport` and returns it as a
//! raw `serde_json::Value`. The MCP dispatcher wraps it via `wrap_tool_result`;
//! `trusty-console`'s `parse_report` unwraps it. The tool takes no arguments.
//!
//! `metrics` payload schema (metrics_schema_version = 1):
//! ```json
//! {
//!   "reviewer_model": string,
//!   "dry_run": bool,
//!   "version": string,
//!   "inference": string,           // "ok" | "auth_error" | "timeout" | ...
//!   "search_reachable": bool,      // required dep
//!   "analyze_reachable": bool      // optional dep
//! }
//! ```
//!
//! Test: `descriptor_name_matches_contract`, `parse_report_round_trip`,
//! `console_metrics_ok_state`, `console_metrics_degraded_search_down`.

use serde_json::Value;

use trusty_common::console_metrics::{ServiceHealth, make_report};

use crate::service::{
    AppState,
    handlers::{compute_status, probe_deps},
};

// ─── Descriptor ──────────────────────────────────────────────────────────────

/// Tool descriptor for `console_metrics` (returned by `tools/list`).
///
/// Why: Each MCP tool needs a descriptor so the console poller knows it exists
/// and what schema it accepts. The descriptor is defined here so it co-locates
/// with the handler and can be included in the tool-list via `descriptors()`.
/// What: Returns a `serde_json::Value` object with `name`, `description`, and
/// `inputSchema` matching the console metrics contract.
/// Test: `descriptor_name_matches_contract`.
pub(crate) fn descriptor() -> Value {
    serde_json::json!({
        "name": trusty_common::console_metrics::CONSOLE_METRICS_METHOD,
        "description": "Return health and operational metrics for trusty-console polling. \
            No arguments required. Returns a ConsoleMetricsReport JSON envelope \
            with service_id='trusty-review', version, status (ok/degraded/error), and \
            a metrics object containing reviewer_model, dry_run, inference status, \
            search_reachable, and analyze_reachable.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// Handle the `console_metrics` tool call for trusty-review.
///
/// Why: The console polls this tool every ~15 s via `StdioMcpClient::call_tool`
/// to gather health/version data without requiring the review HTTP daemon to be
/// discoverable or externally reachable. The console deserialises the result
/// via `parse_report`.
///
/// What: Probes the search dep (non-blocking health call, same as `review_health`),
/// probes the analyze dep (optional), probes inference via the cached
/// `InferenceProbe`. Builds a `ConsoleMetricsReport` with:
/// - `service_id = "trusty-review"`, `display_name = "Trusty Review"`
/// - `version` from `CARGO_PKG_VERSION`
/// - `status = Ok` when inference is "ok" and required dep (search) is reachable;
///   `status = Degraded` when reachable but inference or required dep is impaired;
///   `status = Error` when inference probe itself fails catastrophically (rare)
/// - `metrics` payload: `{reviewer_model, dry_run, version, inference, search_reachable, analyze_reachable}`
/// - `metrics_schema_version = 1`
///
/// Returns the raw report as a `serde_json::Value` — the MCP dispatcher's
/// `wrap_value` applies the single `content[0].text` envelope that `parse_report`
/// in `trusty-console` expects. Do NOT call `serialise_report` here — that would
/// double-wrap.
///
/// Test: `console_metrics_ok_state`, `console_metrics_degraded_search_down`.
pub(crate) async fn handle_console_metrics(state: &AppState) -> Value {
    let reviewer_model = state.config.role_models.reviewer.model.clone();
    let version = env!("CARGO_PKG_VERSION");

    // Bounded, concurrent dep probes (#3658) — same helper the HTTP /health
    // handler and review_health use, so a slow (not down) dep cannot hang
    // this console poll either.
    let deps = probe_deps(state).await;

    // Cached inference-reachability probe (same as review_health).
    let inference = state
        .inference_probe
        .probe(&state.llm, &reviewer_model)
        .await;

    // status is "degraded" when inference fails OR any required dep is down.
    let health_str = compute_status(inference, &deps);

    let status = if health_str == "ok" {
        ServiceHealth::Ok
    } else if health_str == "error" {
        ServiceHealth::Error
    } else {
        ServiceHealth::Degraded
    };

    let metrics = serde_json::json!({
        "reviewer_model": reviewer_model,
        "dry_run": state.config.dry_run,
        "version": version,
        "inference": inference,
        "search_reachable": deps.trusty_search.reachable,
        "analyze_reachable": deps.trusty_analyze.reachable,
        "search_state": deps.trusty_search.state,
        "analyze_state": deps.trusty_analyze.state,
    });

    let report = make_report(
        "trusty-review",
        "Trusty Review",
        version,
        status,
        metrics,
        1, // metrics_schema_version = 1; bump when the metrics object shape changes
    );

    serde_json::to_value(&report).unwrap_or_else(|e| {
        serde_json::json!({
            "error": format!("console_metrics: to_value failed: {e}")
        })
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::{
        config::ReviewConfig,
        integrations::search_client::{
            EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient,
            SearchClientError, SearchResult,
        },
        llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
        service::AppState,
    };
    use trusty_common::console_metrics::parse_report;

    // ── Stub LLM providers ────────────────────────────────────────────────────

    struct OkLlm;

    #[async_trait]
    impl LlmProvider for OkLlm {
        fn name(&self) -> &str {
            "ok-cm-stub"
        }
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: "ok".into(),
                model: req.model.clone(),
                input_tokens: 1,
                output_tokens: 1,
                latency_ms: 0,
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    struct AuthErrorLlm;

    #[async_trait]
    impl LlmProvider for AuthErrorLlm {
        fn name(&self) -> &str {
            "auth-error-cm-stub"
        }
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::AccessDenied("bad key".into()))
        }
    }

    // ── Stub search clients ───────────────────────────────────────────────────

    struct OkSearch;

    #[async_trait]
    impl SearchClient for OkSearch {
        async fn health(&self) -> Result<SearchHealth, SearchClientError> {
            Ok(SearchHealth {
                status: "ok".into(),
                embedder: EmbedderState::Bool(true),
            })
        }
        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Ok(vec![])
        }
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Ok(vec![])
        }
    }

    struct DownSearch;

    #[async_trait]
    impl SearchClient for DownSearch {
        async fn health(&self) -> Result<SearchHealth, SearchClientError> {
            Err(SearchClientError::Unavailable("down".to_string()))
        }
        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Err(SearchClientError::Unavailable("down".to_string()))
        }
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Err(SearchClientError::Unavailable("down".to_string()))
        }
    }

    fn ok_state() -> AppState {
        AppState::new(
            ReviewConfig::load(None),
            Arc::new(OkLlm),
            Arc::new(OkSearch),
            None,
        )
    }

    fn degraded_state() -> AppState {
        AppState::new(
            ReviewConfig::load(None),
            Arc::new(OkLlm),
            Arc::new(DownSearch),
            None,
        )
    }

    fn auth_error_state() -> AppState {
        AppState::new(
            ReviewConfig::load(None),
            Arc::new(AuthErrorLlm),
            Arc::new(OkSearch),
            None,
        )
    }

    // ── Descriptor tests ──────────────────────────────────────────────────────

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

    // ── Round-trip test ───────────────────────────────────────────────────────

    /// Why: The MCP dispatch path wraps tool results exactly once via
    /// `wrap_value` → `{"content":[{"type":"text","text":"<json>"}],"isError":false}`.
    /// `handle_console_metrics` must return the raw report Value (not pre-wrapped)
    /// so `wrap_value` + `parse_report` complete a successful round-trip.
    /// What: Build a raw report Value, apply the same wrapping the dispatcher
    /// uses, then assert `parse_report` decodes it correctly.
    /// Test: This test (no daemon required; mirrors actual dispatch→console path).
    #[test]
    fn parse_report_round_trip() {
        use trusty_common::console_metrics::make_report;

        let report = make_report(
            "trusty-review",
            "Trusty Review",
            "0.1.0",
            ServiceHealth::Degraded,
            json!({
                "reviewer_model": "test-model",
                "dry_run": true,
                "version": "0.1.0",
                "inference": "auth_error",
                "search_reachable": false,
                "analyze_reachable": false,
            }),
            1,
        );
        let raw_value = serde_json::to_value(&report).expect("to_value must succeed");
        // Simulate what wrap_value in tools.rs adds:
        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw_value)
                    .expect("to_string_pretty must succeed"),
            }],
            "isError": false,
        });
        let decoded = parse_report(&wrapped).expect("parse must succeed");

        assert_eq!(decoded.service_id, "trusty-review");
        assert_eq!(decoded.display_name, "Trusty Review");
        assert_eq!(decoded.version, "0.1.0");
        assert_eq!(decoded.status, ServiceHealth::Degraded);
        assert_eq!(decoded.metrics_schema_version, 1);
        assert_eq!(decoded.metrics["search_reachable"], false);
        assert_eq!(decoded.metrics["dry_run"], true);
    }

    // ── Handler tests ─────────────────────────────────────────────────────────

    /// Why: When inference is ok and search is reachable, the report must have
    /// status="ok", search_reachable=true, and include reviewer_model + dry_run.
    /// What: Build ok_state, call handle_console_metrics, decode via parse_report.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_ok_state() {
        let state = ok_state();
        let raw = handle_console_metrics(&state).await;

        // Simulate the wrap_value the dispatcher applies:
        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw).expect("to_string_pretty"),
            }],
            "isError": false,
        });
        let report = parse_report(&wrapped).expect("parse must succeed");
        assert_eq!(report.service_id, "trusty-review");
        assert_eq!(report.display_name, "Trusty Review");
        assert_eq!(report.status, ServiceHealth::Ok);
        assert_eq!(report.metrics["search_reachable"], true);
        assert!(report.metrics["reviewer_model"].is_string());
        assert!(report.metrics["dry_run"].is_boolean());
        assert!(report.metrics["version"].is_string());
        assert_eq!(report.metrics["inference"], "ok");
    }

    /// Why: When the required dep (trusty-search) is down, status must be "degraded"
    /// and search_reachable=false even if inference is ok.
    /// What: Build degraded_state (search down), call handle_console_metrics, decode.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_degraded_search_down() {
        let state = degraded_state();
        let raw = handle_console_metrics(&state).await;

        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw).expect("to_string_pretty"),
            }],
            "isError": false,
        });
        let report = parse_report(&wrapped).expect("parse must succeed");
        assert_eq!(report.status, ServiceHealth::Degraded);
        assert_eq!(report.metrics["search_reachable"], false);
    }

    /// Why: When inference fails (auth error), status must be "degraded" and
    /// inference field reflects the error type.
    /// What: Build auth_error_state, call handle_console_metrics, decode.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_degraded_auth_error() {
        let state = auth_error_state();
        let raw = handle_console_metrics(&state).await;

        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw).expect("to_string_pretty"),
            }],
            "isError": false,
        });
        let report = parse_report(&wrapped).expect("parse must succeed");
        assert_eq!(report.status, ServiceHealth::Degraded);
        let inference = report.metrics["inference"].as_str().unwrap_or("");
        assert_ne!(
            inference, "ok",
            "auth_error state must not report inference=ok"
        );
    }

    // ── ServiceHealth::Error mapping test ────────────────────────────────────

    /// Why: The `else if health_str == "error"` branch (issue #1164 sweep-up)
    /// must map to `ServiceHealth::Error`, not `Degraded`. Without the fix the
    /// branch fell to the final `else` and silently produced `Degraded`.
    /// What: Construct a report directly with `ServiceHealth::Error`, round-trip
    /// it through the MCP envelope, and assert the decoded status is `Error`.
    /// The `make_report` helper constructs the report independently of
    /// `handle_console_metrics`; we only need to verify the serde round-trip
    /// and that `ServiceHealth::Error` serialises to `"error"` as expected by
    /// the console's `parse_report`.
    /// Test: This test.
    #[test]
    fn service_health_error_serialises_correctly() {
        use trusty_common::console_metrics::make_report;

        let report = make_report(
            "trusty-review",
            "Trusty Review",
            "0.1.0",
            ServiceHealth::Error,
            serde_json::json!({"reason": "catastrophic failure"}),
            1,
        );
        let raw_value = serde_json::to_value(&report).expect("to_value must succeed");
        // Verify the serialised status field is "error" (not "degraded").
        assert_eq!(
            raw_value["status"].as_str(),
            Some("error"),
            "ServiceHealth::Error must serialise to \"error\""
        );
        let wrapped = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&raw_value)
                    .expect("to_string_pretty must succeed"),
            }],
            "isError": false,
        });
        let decoded = parse_report(&wrapped).expect("parse must succeed");
        assert_eq!(
            decoded.status,
            ServiceHealth::Error,
            "parse_report must decode \"error\" back to ServiceHealth::Error"
        );
    }
}
