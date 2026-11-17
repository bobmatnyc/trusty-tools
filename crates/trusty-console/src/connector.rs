//! ServiceConnector trait — the extensibility seam for all per-service adapters.
//!
//! Why: P0 only needs read-only detection (`detect()`), but P1+ must add
//! `spawn()` and typed tool-call methods. Defining the trait now with those
//! method stubs keeps the architecture clean and avoids large breaking
//! refactors when those phases land.
//! What: Defines the `ServiceConnector` trait and re-exports the
//! `ServiceStatus` / `ServiceInfo` row types (owned by
//! `trusty_common::console_rpc` since #10902) that the API layer serialises.
//! Test: Each concrete connector implements `#[cfg(test)]` unit tests that
//! exercise `detect()` against fake data-dir fixtures; see `detect.rs`.

// #10902: the row types moved to trusty-common so the TUI (trusty-mpm, no edge
// on this crate) can decode `console.fleet_status`. Re-exported here so every
// `crate::connector::ServiceInfo` path, in and out of this crate, is unchanged.
pub use trusty_common::console_rpc::{ServiceInfo, ServiceLifecycle, ServiceStatus};

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

    /// How this member runs when it is healthy (#6416).
    ///
    /// Why: a roster-level test can read this without probing sockets or
    /// spawning binaries, so "which members are on-demand" is assertable as the
    /// constant it is rather than inferred from a live detection pass.
    /// What: defaults to `Daemon`; the de-daemonized members override it.
    /// Test: `on_demand_members_are_exactly_review_and_analyze`.
    fn lifecycle(&self) -> ServiceLifecycle {
        ServiceLifecycle::Daemon
    }

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

    /// Why (#6416): the Svelte card branches on this string. A rename that only
    /// changed the Rust identifier would leave every on-demand row rendering the
    /// daemon sentence again, with nothing red anywhere to say so.
    /// What: asserts both variants' wire spellings and that an older payload
    /// with no `lifecycle` key still reads as a daemon.
    /// Test: this test.
    #[test]
    fn service_lifecycle_serialises_to_snake_case() {
        assert_eq!(
            serde_json::to_value(ServiceLifecycle::Daemon).unwrap(),
            serde_json::json!("daemon")
        );
        assert_eq!(
            serde_json::to_value(ServiceLifecycle::OnDemand).unwrap(),
            serde_json::json!("on_demand")
        );

        let legacy: ServiceInfo = serde_json::from_value(serde_json::json!({
            "id": "trusty-search",
            "display_name": "Trusty Search",
            "status": "available",
        }))
        .expect("a payload predating #6416 must still deserialise");
        assert_eq!(legacy.lifecycle, ServiceLifecycle::Daemon);
    }
}
