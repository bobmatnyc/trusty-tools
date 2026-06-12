//! ServiceConnector trait — the extensibility seam for all per-service adapters.
//!
//! Why: P0 only needs read-only detection (`detect()`), but P1+ must add
//! `spawn()` and typed tool-call methods. Defining the trait now with those
//! method stubs keeps the architecture clean and avoids large breaking
//! refactors when those phases land.
//! What: Defines `ServiceStatus`, the `ServiceConnector` trait, and a
//! `ServiceInfo` result struct that the API layer serialises.
//! Test: Each concrete connector implements `#[cfg(test)]` unit tests that
//! exercise `detect()` against fake data-dir fixtures; see `detect.rs`.

use serde::{Deserialize, Serialize};

/// Runtime status of one detected service.
///
/// Why: Four states capture everything the console needs — whether the binary
/// exists, whether a daemon is running, whether the MCP handshake succeeded but
/// the expected tool is missing (Degraded), and whether the binary is absent.
/// What: Serialises to a lowercase string for the JSON API.
/// Test: Asserted by unit tests in `detect.rs` and `server.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    /// Daemon is reachable and health-checked OK.
    Running,
    /// Binary found on PATH but no daemon discovery file / TCP probe.
    Available,
    /// Binary not found on PATH.
    Absent,
    /// Process is reachable (MCP handshake succeeded) but the expected
    /// `console_metrics` tool was not listed in `tools/list` — check that
    /// the daemon is started in `serve --stdio` mode with the correct wiring.
    Degraded,
}

/// All facts gathered about a service in one detection pass.
///
/// Why: The API handler turns this struct directly into JSON for
/// `GET /api/console/services`, so callers get a stable shape to render cards.
/// What: `id` is the stable machine identifier; `display_name` is human-
/// readable; `status` is the current runtime state; `version` is the version
/// string from `/health` when the daemon is running (absent otherwise);
/// `url` is the daemon base URL when reachable; `hint` is an optional
/// actionable remediation message surfaced when `status` is `Degraded`.
/// Test: Tested via the server integration test in `server.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Stable machine identifier (e.g. `"trusty-search"`).
    pub id: String,
    /// Human-readable display name (e.g. `"Trusty Search"`).
    pub display_name: String,
    /// Current runtime state.
    pub status: ServiceStatus,
    /// Version string reported by the running daemon's `/health` endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Base URL of the running daemon (e.g. `"http://127.0.0.1:7879"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Actionable remediation hint — present when `status` is `Degraded`.
    /// Example: "reachable but `console_metrics` tool not registered — check
    /// `serve --stdio` wiring / restart the daemon".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Per-service adapter contract.
///
/// Why: Decouples the orchestration loop from service-specific knowledge.
/// P0 only calls `detect()`; P1+ will add `spawn()` and typed MCP/tool-call
/// methods without modifying existing code paths.
/// What: A synchronous detect() that returns a `ServiceInfo`. The trait is
/// object-safe so connectors can be boxed (`Box<dyn ServiceConnector>`).
/// Test: Each impl has unit tests in `detect.rs` exercising the three status
/// outcomes.
pub trait ServiceConnector: Send + Sync {
    /// Stable machine identifier for this service (must be unique per instance).
    ///
    /// Why: Used as the `id` field in `ServiceInfo` and as a log tag.
    /// What: Returns a `'static str` reference so no allocation is needed.
    /// Test: Compared against expected strings in tests.
    fn id(&self) -> &'static str;

    /// Human-readable name shown in the console UI.
    ///
    /// Why: Separates the stable ID from the display label so either can
    /// change independently.
    /// What: Returns a `'static str`.
    /// Test: Asserted by the connector construction tests.
    fn display_name(&self) -> &'static str;

    /// Detect the current runtime status of this service.
    ///
    /// Why: P0 detection is purely read-only — reads files, probes TCP, looks
    /// for binaries — so it never mutates any service state.
    /// What: Returns a `ServiceInfo` with `status`, optional `version`, and
    /// optional `url`. Never returns an error; degrades gracefully to `Absent`.
    /// Test: Unit tests in `detect.rs` inject a temp home dir and fake files.
    fn detect(&self) -> ServiceInfo;
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The JSON API contract requires `ServiceStatus` variants to serialise
    /// to exact lowercase strings. A missing or mismatched `#[serde(rename_all)]`
    /// attribute would silently change the wire format and break the Svelte UI.
    /// What: Serialises each variant via `serde_json::to_value` and asserts the
    /// exact expected string so any future rename is caught at test time.
    /// Test: This test.
    #[test]
    fn service_status_serialises_to_lowercase_strings() {
        assert_eq!(
            serde_json::to_value(ServiceStatus::Running).unwrap(),
            serde_json::json!("running"),
            "Running must serialise to \"running\""
        );
        assert_eq!(
            serde_json::to_value(ServiceStatus::Available).unwrap(),
            serde_json::json!("available"),
            "Available must serialise to \"available\""
        );
        assert_eq!(
            serde_json::to_value(ServiceStatus::Absent).unwrap(),
            serde_json::json!("absent"),
            "Absent must serialise to \"absent\""
        );
        assert_eq!(
            serde_json::to_value(ServiceStatus::Degraded).unwrap(),
            serde_json::json!("degraded"),
            "Degraded must serialise to \"degraded\""
        );
    }
}
