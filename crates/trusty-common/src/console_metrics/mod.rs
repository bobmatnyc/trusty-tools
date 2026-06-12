//! Service-agnostic "console metrics" contract for trusty-console (epic #1104).
//!
//! Why: The trusty-console daemon polls each local service (trusty-search,
//! trusty-memory, trusty-analyze, …) over its MCP stdio connection to gather
//! health and operational data for the web dashboard. To keep the console
//! service-agnostic — so new local services can plug in without console
//! changes — every service exposes a single standardised MCP tool named by
//! [`CONSOLE_METRICS_METHOD`] that returns a [`ConsoleMetricsReport`].
//!
//! This module defines:
//! - The method/tool name constant [`CONSOLE_METRICS_METHOD`].
//! - The response type [`ConsoleMetricsReport`] with a flexible
//!   `metrics: serde_json::Value` payload for per-service data.
//! - The [`ServiceHealth`] enum for coarse health classification.
//! - Helper functions [`make_report`] (service side, build a report) and
//!   [`parse_report`] (console side, decode a raw MCP tool result).
//!
//! What: Gated behind the `console-metrics` feature flag in `trusty-common`.
//! Depends on `stdio-mcp-client` (same feature group) for the client types
//! used by console pollers.
//!
//! Test: `cargo test -p trusty-common --features console-metrics` runs the
//! unit tests in this module (round-trip serde, schema version, status
//! variants, helper functions).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP tool name a local service exposes (no arguments) so a console can
/// poll it uniformly.
///
/// Why: Using a constant prevents typos and ensures the console and all
/// service implementations agree on the same name without coordination.
/// What: The string `"console_metrics"` — used as the `name` parameter in a
/// `tools/call` request and as the key in a service's `tools/list` response.
/// Test: `method_constant_value` asserts the expected string value.
pub const CONSOLE_METRICS_METHOD: &str = "console_metrics";

/// Coarse health classification reported by a local service.
///
/// Why: The console dashboard needs a simple traffic-light status to render
/// per-service health badges without parsing per-service metric payloads.
/// What: Three variants — `Ok` (operating normally), `Degraded` (partial
/// impairment, e.g. warm-boot degraded), `Error` (not functioning). Serialised
/// as lowercase strings ("ok", "degraded", "error") for JSON compatibility.
/// Test: `service_health_serde_round_trip` verifies all variants round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceHealth {
    /// The service is operating normally.
    Ok,
    /// The service is running but with partial impairment.
    Degraded,
    /// The service is not functioning.
    Error,
}

/// The standardised metrics report a local service returns from the
/// `console_metrics` MCP tool.
///
/// Why: A shared response schema lets the console decode health data from any
/// service without per-service deserialization logic. The flexible
/// `metrics: Value` field accommodates rich per-service data (index counts,
/// palace stats, analyzer hotspots) without forcing a lowest-common-denominator
/// type.
///
/// Design notes (see epic #1104 for owner sign-off):
/// - `service_id`: machine-readable identifier (e.g. `"trusty-search"`).
/// - `display_name`: human-readable label for the console tab (e.g. `"Search"`).
/// - `version`: the service's crate semver string (e.g. `"0.24.1"`).
/// - `status`: coarse health classification; see [`ServiceHealth`].
/// - `metrics`: opaque per-service payload (object or null). The console
///   passes this through to a service-specific Svelte component.
/// - `metrics_schema_version`: monotonically increasing integer the service
///   bumps when the shape of `metrics` changes. Lets the console detect when
///   it needs a UI update for a given service.
/// - `collected_at_unix`: optional Unix timestamp (seconds) when the service
///   collected the data. `None` means "unknown / not provided".
///
/// What: A serde-serialisable struct; `make_report` constructs it on the
/// service side; `parse_report` decodes it on the console side.
/// Test: `report_round_trips_all_fields`, `report_minimal_round_trip`,
/// `parse_report_decodes_valid_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleMetricsReport {
    /// Machine-readable service identifier (e.g. `"trusty-search"`).
    pub service_id: String,
    /// Human-readable display name for the console tab (e.g. `"Search"`).
    pub display_name: String,
    /// Crate semver version string (e.g. `"0.24.1"`).
    pub version: String,
    /// Coarse health classification.
    pub status: ServiceHealth,
    /// Opaque per-service metrics payload. Services document their own schema
    /// in their own MCP server crate; the console passes this to a
    /// service-specific Svelte component without interpreting it.
    pub metrics: Value,
    /// Monotonically increasing integer the service bumps when the shape of
    /// `metrics` changes. Lets the console detect stale UI components.
    pub metrics_schema_version: u32,
    /// Unix timestamp (seconds) when metrics were collected. `None` means the
    /// service did not record collection time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collected_at_unix: Option<u64>,
}

/// Construct a [`ConsoleMetricsReport`] on the service side.
///
/// Why: Providing a constructor helper prevents service implementers from
/// hand-rolling JSON or accidentally omitting required fields. The console
/// cannot function correctly if a required field is missing.
/// What: Builds and returns a `ConsoleMetricsReport`. The `collected_at_unix`
/// field is set to the current Unix time via `std::time::SystemTime`; on
/// platforms where this fails it is set to `None`.
/// Test: `make_report_sets_collection_time` verifies `collected_at_unix` is
/// populated and close to `now`.
pub fn make_report(
    service_id: impl Into<String>,
    display_name: impl Into<String>,
    version: impl Into<String>,
    status: ServiceHealth,
    metrics: Value,
    metrics_schema_version: u32,
) -> ConsoleMetricsReport {
    let collected_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs());

    ConsoleMetricsReport {
        service_id: service_id.into(),
        display_name: display_name.into(),
        version: version.into(),
        status,
        metrics,
        metrics_schema_version,
        collected_at_unix,
    }
}

/// Serialise a [`ConsoleMetricsReport`] to a `serde_json::Value` for
/// inclusion in an MCP `tools/call` response.
///
/// Why: Service-side tool handlers return `serde_json::Value`; this helper
/// wraps the report so the call site is one line rather than a nested
/// `json!({"content": [...]})` construction.
/// What: Calls `serde_json::to_value` and wraps the result in the MCP
/// `content[0].text` envelope expected by MCP tool result consumers.
/// Returns `Err` if serialisation fails (should not happen for well-formed
/// reports).
/// Test: `serialise_report_produces_mcp_envelope` verifies the envelope shape.
pub fn serialise_report(report: &ConsoleMetricsReport) -> Result<Value> {
    let text = serde_json::to_string(report).context("serialising ConsoleMetricsReport")?;
    Ok(serde_json::json!({
        "content": [
            { "type": "text", "text": text }
        ]
    }))
}

/// Decode a [`ConsoleMetricsReport`] from a raw MCP `tools/call` result on
/// the console side.
///
/// Why: The console receives the raw result from `StdioMcpClient::call_tool`
/// as a `serde_json::Value`. This helper extracts and deserialises the report
/// so console poll loops are a single `parse_report(raw)?` call.
/// What: Looks for `result.content[0].text` (the MCP text-content envelope),
/// parses the JSON string therein as a `ConsoleMetricsReport`, and returns it.
/// Returns `Err` if the envelope shape is unexpected or the JSON is malformed.
/// Test: `parse_report_decodes_valid_json` round-trips through `serialise_report`.
pub fn parse_report(raw: &Value) -> Result<ConsoleMetricsReport> {
    // The MCP result shape from StdioMcpClient::call_tool is the `result` object
    // returned by the server, which contains `content: [{type:"text", text:"..."}]`.
    let text = raw
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .context("MCP tool result missing content[0].text")?;

    serde_json::from_str::<ConsoleMetricsReport>(text)
        .context("deserialising ConsoleMetricsReport from tool result text")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why: The method constant is the integration point between services and
    /// the console; a typo silently breaks all polling.
    /// What: Assert the exact expected string value.
    /// Test: This test.
    #[test]
    fn method_constant_value() {
        assert_eq!(CONSOLE_METRICS_METHOD, "console_metrics");
    }

    /// Why: `ServiceHealth` values are sent over the wire; serde behaviour must
    /// be deterministic and lowercase for JSON convention.
    /// What: Serialise all three variants and assert the expected strings;
    /// deserialise them back and assert equality.
    /// Test: This test.
    #[test]
    fn service_health_serde_round_trip() {
        let cases = [
            (ServiceHealth::Ok, "\"ok\""),
            (ServiceHealth::Degraded, "\"degraded\""),
            (ServiceHealth::Error, "\"error\""),
        ];
        for (variant, expected_json) in &cases {
            let serialised = serde_json::to_string(variant).unwrap();
            assert_eq!(&serialised, expected_json, "wrong JSON for {variant:?}");
            let roundtripped: ServiceHealth = serde_json::from_str(&serialised).unwrap();
            assert_eq!(&roundtripped, variant, "round-trip failed for {variant:?}");
        }
    }

    /// Why: All required fields must survive a serde round-trip intact so the
    /// console can rely on them.
    /// What: Construct a report with all fields set, serialise to JSON, and
    /// deserialise back; assert every field equals the original.
    /// Test: This test.
    #[test]
    fn report_round_trips_all_fields() {
        let metrics = json!({ "index_count": 42, "warm_boot_degraded": false });
        let report = ConsoleMetricsReport {
            service_id: "trusty-search".to_string(),
            display_name: "Search".to_string(),
            version: "0.24.1".to_string(),
            status: ServiceHealth::Ok,
            metrics: metrics.clone(),
            metrics_schema_version: 3,
            collected_at_unix: Some(1_700_000_000),
        };

        let json_str = serde_json::to_string(&report).unwrap();
        let decoded: ConsoleMetricsReport = serde_json::from_str(&json_str).unwrap();

        assert_eq!(decoded.service_id, "trusty-search");
        assert_eq!(decoded.display_name, "Search");
        assert_eq!(decoded.version, "0.24.1");
        assert_eq!(decoded.status, ServiceHealth::Ok);
        assert_eq!(decoded.metrics, metrics);
        assert_eq!(decoded.metrics_schema_version, 3);
        assert_eq!(decoded.collected_at_unix, Some(1_700_000_000));
    }

    /// Why: A minimal report (no `collected_at_unix`) must still deserialise
    /// correctly — `skip_serializing_if = "Option::is_none"` means the field
    /// is absent in the JSON when None.
    /// What: Construct a report without `collected_at_unix`, serialise and
    /// deserialise; assert the field is None.
    /// Test: This test.
    #[test]
    fn report_minimal_round_trip() {
        let report = ConsoleMetricsReport {
            service_id: "trusty-memory".to_string(),
            display_name: "Memory".to_string(),
            version: "0.15.0".to_string(),
            status: ServiceHealth::Degraded,
            metrics: json!(null),
            metrics_schema_version: 1,
            collected_at_unix: None,
        };

        let json_str = serde_json::to_string(&report).unwrap();
        // Confirm `collected_at_unix` is absent in the JSON.
        let raw: Value = serde_json::from_str(&json_str).unwrap();
        assert!(
            raw.get("collected_at_unix").is_none(),
            "collected_at_unix should be absent when None"
        );

        let decoded: ConsoleMetricsReport = serde_json::from_str(&json_str).unwrap();
        assert_eq!(decoded.collected_at_unix, None);
        assert_eq!(decoded.status, ServiceHealth::Degraded);
    }

    /// Why: `metrics_schema_version` starts at 1 and can increase; verify
    /// that zero and u32::MAX survive round-trip without truncation.
    /// What: Two round-trips with version = 0 and version = u32::MAX.
    /// Test: This test.
    #[test]
    fn schema_version_boundary_values() {
        for version in [0u32, u32::MAX] {
            let report = ConsoleMetricsReport {
                service_id: "svc".to_string(),
                display_name: "Svc".to_string(),
                version: "1.0.0".to_string(),
                status: ServiceHealth::Ok,
                metrics: json!({}),
                metrics_schema_version: version,
                collected_at_unix: None,
            };
            let json_str = serde_json::to_string(&report).unwrap();
            let decoded: ConsoleMetricsReport = serde_json::from_str(&json_str).unwrap();
            assert_eq!(
                decoded.metrics_schema_version, version,
                "schema_version {version} did not survive round-trip"
            );
        }
    }

    /// Why: `make_report` is the canonical service-side constructor; it must
    /// populate all fields correctly and set `collected_at_unix` to a plausible
    /// current time.
    /// What: Build a report via `make_report`, assert field values, and assert
    /// `collected_at_unix` is within 5 seconds of now.
    /// Test: This test.
    #[test]
    fn make_report_sets_collection_time() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let report = make_report(
            "trusty-analyze",
            "Analyze",
            "0.5.0",
            ServiceHealth::Ok,
            json!({ "files_analyzed": 100 }),
            2,
        );

        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        assert_eq!(report.service_id, "trusty-analyze");
        assert_eq!(report.display_name, "Analyze");
        assert_eq!(report.version, "0.5.0");
        assert_eq!(report.status, ServiceHealth::Ok);
        assert_eq!(report.metrics_schema_version, 2);

        let ts = report
            .collected_at_unix
            .expect("make_report should set collected_at_unix");
        assert!(
            ts >= before && ts <= after + 1,
            "collected_at_unix {ts} is outside [{before}, {after}]"
        );
    }

    /// Why: `serialise_report` + `parse_report` must be a lossless round-trip;
    /// together they form the integration boundary between service and console.
    /// What: Build a report, serialise it, parse it back, and assert equality.
    /// Test: This test.
    #[test]
    fn parse_report_decodes_valid_json() {
        let original = ConsoleMetricsReport {
            service_id: "trusty-search".to_string(),
            display_name: "Search".to_string(),
            version: "0.24.1".to_string(),
            status: ServiceHealth::Ok,
            metrics: json!({ "index_count": 7 }),
            metrics_schema_version: 1,
            collected_at_unix: Some(1_700_000_000),
        };

        let envelope = serialise_report(&original).unwrap();
        let decoded = parse_report(&envelope).unwrap();

        assert_eq!(decoded.service_id, original.service_id);
        assert_eq!(decoded.display_name, original.display_name);
        assert_eq!(decoded.version, original.version);
        assert_eq!(decoded.status, original.status);
        assert_eq!(decoded.metrics, original.metrics);
        assert_eq!(
            decoded.metrics_schema_version,
            original.metrics_schema_version
        );
        assert_eq!(decoded.collected_at_unix, original.collected_at_unix);
    }

    /// Why: `serialise_report` must produce the MCP content envelope shape
    /// (`{content: [{type:"text", text:"..."}]}`); the console parses this
    /// exact shape.
    /// What: Serialise a report and assert the top-level `content` array
    /// exists, has one element, and its `type` is `"text"`.
    /// Test: This test.
    #[test]
    fn serialise_report_produces_mcp_envelope() {
        let report = make_report("svc", "Svc", "1.0.0", ServiceHealth::Ok, json!({}), 1);
        let envelope = serialise_report(&report).unwrap();
        let content = envelope
            .get("content")
            .and_then(|c| c.as_array())
            .expect("envelope should have `content` array");
        assert_eq!(content.len(), 1, "content should have exactly one item");
        assert_eq!(
            content[0].get("type").and_then(|t| t.as_str()),
            Some("text"),
            "content[0].type should be \"text\""
        );
        assert!(
            content[0].get("text").and_then(|t| t.as_str()).is_some(),
            "content[0].text should be a string"
        );
    }

    /// Why: `parse_report` must return a clear error on malformed input so
    /// the console can log and skip bad responses.
    /// What: Pass a JSON object without the `content` envelope and assert Err.
    /// Test: This test.
    #[test]
    fn parse_report_errors_on_bad_envelope() {
        let bad = json!({ "result": "some random value" });
        assert!(
            parse_report(&bad).is_err(),
            "parse_report should error on missing content envelope"
        );
    }
}
