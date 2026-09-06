//! `ServiceConnector` implementation for the console itself (#6908).
//!
//! Why: every trusty-* daemon the console can see appears in the Services
//! roster except the one serving the page. An operator looking at the roster to
//! answer "what is running on this machine, and what is it costing me" got six
//! answers and a blind spot, and the console is the process holding the SSE
//! fan-out and the sampling loop — the one whose cost the operator cannot check
//! anywhere else.
//!
//! Why this connector does no I/O at all, unlike every sibling in this module:
//! the sibling connectors answer "is that other process alive", which needs a
//! binary lookup, a discovery file and a socket probe. This one answers "am I
//! alive", and the fact that `detect()` was called is the whole proof. There is
//! no code path on which the console can report itself `Absent` or `Available`:
//! both would mean this process is not running, and a process that is not
//! running does not serve the roster.
//! What: [`ConsoleConnector`], registered in
//! [`all_connectors`](super::all_connectors), reporting `Running`, the
//! `Daemon` lifecycle, and the compiled-in crate version. CPU and RSS are left
//! `None` here and overlaid by the route from the sampler's rings, exactly as
//! they are for every other member — see
//! [`crate::service_metrics::apply_metrics_overlay`].
//! Test: `console_connector_reports_itself_running`,
//! `console_connector_reports_the_crate_version`,
//! `super::tests::test_all_connectors_returns_seven`,
//! `crate::server::tests::services_route_lists_the_console_itself`.

use crate::connector::{ServiceConnector, ServiceInfo, ServiceLifecycle, ServiceStatus};

/// The console's own row in the Services roster (#6908).
///
/// Why a connector rather than a special case in the route: the roster's shape,
/// its ordering, and the CPU/RSS overlay all run off `all_connectors()`. A row
/// synthesised anywhere else would need the route, the poller cache and the
/// sampler each taught about one exception, and the UI would need a branch to
/// render it. As a connector it is the same row as the other six.
/// What: a zero-sized struct — it holds nothing because `detect()` reads
/// nothing.
/// Test: `console_connector_reports_itself_running`.
pub struct ConsoleConnector;

impl ConsoleConnector {
    /// Create a new `ConsoleConnector`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConsoleConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceConnector for ConsoleConnector {
    fn id(&self) -> &'static str {
        "trusty-console"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Console"
    }

    /// Report the console as running, at the version it was built from.
    ///
    /// Why the status is a constant: see the module docs — reaching this line
    /// IS the liveness evidence a probe would go looking for.
    /// Why the version is `CARGO_PKG_VERSION` rather than a `/health` fetch:
    /// the sibling connectors read a version off the wire because it belongs to
    /// a process they did not build. This one is compiled into the binary
    /// answering the request, so a fetch would ask the network a question the
    /// linker already answered.
    /// What: a `Running` [`ServiceInfo`] with the `Daemon` lifecycle and no URL
    /// — the connector is a synchronous probe with no access to the bind
    /// address the server resolved, and the proxy keyed `console` off its
    /// allowlist deliberately, so there is nothing a URL here would serve.
    /// `cpu_pct` and `rss_bytes` are `None` for every connector; the route
    /// overlays them.
    /// Test: `console_connector_reports_itself_running`,
    /// `console_connector_reports_the_crate_version`.
    fn detect(&self) -> ServiceInfo {
        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            // #6908: the console can only answer while it is running.
            status: ServiceStatus::Running,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            url: None,
            hint: None,
            lifecycle: ServiceLifecycle::Daemon,
            cpu_pct: None,
            rss_bytes: None,
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the roster row must be `Running` on every call, with no path to
    /// `Absent` or `Available` — those describe a process that is not serving
    /// the roster. This also pins the id the route, the sampler and the UI all
    /// key off.
    /// What: calls `detect()` twice and asserts the identity fields and the
    /// status both times.
    /// Test: this test itself.
    #[test]
    fn console_connector_reports_itself_running() {
        let connector = ConsoleConnector::new();
        for _ in 0..2 {
            let info = connector.detect();
            assert_eq!(info.id, "trusty-console");
            assert_eq!(info.display_name, "Trusty Console");
            assert_eq!(info.status, ServiceStatus::Running);
            assert_eq!(info.lifecycle, ServiceLifecycle::Daemon);
            assert_eq!(info.url, None);
            assert_eq!(info.hint, None);
        }
    }

    /// Why: the row renders a version, and reading it off the binary is the
    /// point — a hard-coded string would drift from the crate the operator is
    /// actually running the moment the version is bumped.
    /// What: asserts the reported version IS `CARGO_PKG_VERSION`.
    /// Test: this test itself.
    #[test]
    fn console_connector_reports_the_crate_version() {
        let info = ConsoleConnector::new().detect();
        assert_eq!(info.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }
}
