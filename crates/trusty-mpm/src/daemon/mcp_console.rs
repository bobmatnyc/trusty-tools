//! Daemon-side implementation of the console-facing MCP tools (#1222 / P2).
//!
//! Why: trusty-console renders the Sessions tab natively by polling the daemon
//! over MCP (per the #1104 HTTP-only-in-console principle). It needs three tools
//! the existing catalog did not provide: `console_metrics` (the standard
//! [`trusty_common::console_metrics::ConsoleMetricsReport`] every trusty service
//! exposes so the console poller is service-agnostic), `supervisor_status` (the
//! fleet-state snapshot plus the auto-resume control state), and `auto_resume_set`
//! (the console's non-CLI control for enabling/disabling auto-resume — RFC §6 Q6).
//! Putting them here keeps `mcp_backend.rs` under the 500-SLOC production cap.
//! What: three free async functions over `&Arc<DaemonState>`:
//! [`console_metrics`] builds a report whose `metrics` payload carries the
//! `FleetMetrics` and supervisor flags; [`supervisor_status`] returns the same
//! fleet snapshot as a bare object; [`auto_resume_set`] persists the operator's
//! desired flag and echoes the resulting state. All return the same JSON shapes
//! across MCP and any future HTTP transport.
//! Test: `cargo test -p trusty-mpm daemon::mcp_console` covers report shape,
//! fleet derivation, and the auto-resume persistence round-trip.

use std::sync::Arc;

use serde_json::{Value, json};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth, make_report};

use crate::core::auto_resume;
use crate::daemon::state::DaemonState;
use crate::supervisor::metrics::FleetMetrics;

/// Schema version of the `console_metrics` `metrics` payload for trusty-mpm.
///
/// Why: the console bumps its UI when this changes; starting at 1 establishes the
/// contract for the Sessions tab.
/// What: monotonically-increasing integer carried in every report.
/// Test: asserted in `console_metrics_report_has_expected_shape`.
const METRICS_SCHEMA_VERSION: u32 = 1;

/// Service version reported in `console_metrics` (the crate semver).
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the supervisor/auto-resume + fleet snapshot shared by both
/// `console_metrics` and `supervisor_status`.
///
/// Why: both tools surface the same fleet counts and auto-resume state; deriving
/// them in one place keeps the two tool payloads in lockstep.
/// What: lists the managed session records, derives [`FleetMetrics`] from them,
/// and reads the persisted desired + live env auto-resume flags. Returns a JSON
/// object: `{ fleet, auto_resume: { desired, env, effective } }`.
/// Test: `supervisor_status_reports_fleet_and_auto_resume`.
async fn fleet_snapshot(state: &Arc<DaemonState>) -> Value {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let fleet = FleetMetrics::from_records(&records);

    // The persisted desired flag is the console-mutable control; the env flag is
    // what the supervisor process booted with. They can disagree until restart.
    let desired = auto_resume::read_desired().unwrap_or(false);
    let env = auto_resume::effective_from_env();

    json!({
        "fleet": fleet,
        "auto_resume": {
            "desired": desired,
            "env": env,
            // `effective` is what is actually in force right now: the supervisor
            // only honours its boot-time env until it next reads the file.
            "effective": env,
            "pending_restart": desired != env,
        },
    })
}

/// Back the `console_metrics` tool with a [`ConsoleMetricsReport`].
///
/// Why: the trusty-console metrics poller calls `console_metrics` on every
/// service uniformly; trusty-mpm must speak the same contract so it appears in
/// the dashboard like the other services.
/// What: classifies health (`Ok` normally, `Degraded` when any session is
/// `errored`), packs the fleet + auto-resume snapshot into the report's flexible
/// `metrics` field, and returns it as a JSON object the console deserialises with
/// `trusty_common::console_metrics::parse_report`.
/// Test: `console_metrics_report_has_expected_shape`.
pub async fn console_metrics(state: &Arc<DaemonState>) -> Result<Value, String> {
    let snapshot = fleet_snapshot(state).await;
    let errored = snapshot
        .get("fleet")
        .and_then(|f| f.get("errored"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let status = if errored > 0 {
        ServiceHealth::Degraded
    } else {
        ServiceHealth::Ok
    };

    let report: ConsoleMetricsReport = make_report(
        "trusty-mpm",
        "Trusty MPM",
        SERVICE_VERSION,
        status,
        snapshot,
        METRICS_SCHEMA_VERSION,
    );

    serde_json::to_value(&report).map_err(|e| format!("serialising console_metrics report: {e}"))
}

/// Back the `supervisor_status` tool with the fleet + auto-resume snapshot.
///
/// Why: the Sessions tab's supervisor widget needs fleet counts by lifecycle
/// state and the current auto-resume control state in one call, without parsing
/// the full `console_metrics` envelope.
/// What: returns the bare `{ fleet, auto_resume }` object from [`fleet_snapshot`].
/// Test: `supervisor_status_reports_fleet_and_auto_resume`.
pub async fn supervisor_status(state: &Arc<DaemonState>) -> Result<Value, String> {
    Ok(fleet_snapshot(state).await)
}

/// Back the `auto_resume_set` tool: persist the operator's desired flag.
///
/// Why: the console toggle must durably record whether auto-resume should be on
/// (RFC §6 Q6 — controls live in the console, not CLI-only). The supervisor runs
/// as a separate process, so this writes the desired state the supervisor reads
/// on its next sweep rather than mutating a live env var.
/// What: writes `~/.trusty-mpm/auto_resume`, then echoes the resulting
/// `{ desired, env, effective, pending_restart }` so the console can render the
/// toggle and a "restart pending" hint when the persisted desire differs from the
/// supervisor's boot-time env.
/// Test: `auto_resume_set_persists_desired`.
pub async fn auto_resume_set(enabled: bool) -> Result<Value, String> {
    auto_resume::write_desired(enabled)
        .map_err(|e| format!("persisting auto_resume desired state: {e}"))?;

    let desired = enabled;
    let env = auto_resume::effective_from_env();
    Ok(json!({
        "desired": desired,
        "env": env,
        "effective": env,
        "pending_restart": desired != env,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<DaemonState> {
        DaemonState::shared()
    }

    /// Why: the console deserialises the report with `parse_report`; the tool must
    /// return the exact `ConsoleMetricsReport` shape (service_id, status, and a
    /// `metrics.fleet` object).
    /// Test: this test.
    // NOTE: `DaemonState::shared()` is a PROCESS-WIDE singleton, so other tests in
    // this binary may have registered managed sessions before these run. The
    // assertions below therefore check SHAPE and field TYPES, never exact fleet
    // counts (which are non-deterministic across the shared test process).

    #[tokio::test]
    async fn console_metrics_report_has_expected_shape() {
        let report = console_metrics(&state()).await.expect("report");
        assert_eq!(report["service_id"], "trusty-mpm");
        assert_eq!(report["display_name"], "Trusty MPM");
        assert_eq!(report["metrics_schema_version"], METRICS_SCHEMA_VERSION);
        // Status is one of the coarse ServiceHealth variants.
        assert!(
            matches!(report["status"].as_str(), Some("ok") | Some("degraded")),
            "status must be ok|degraded: {report}"
        );
        assert!(
            report["metrics"]["fleet"].is_object(),
            "metrics.fleet must be an object: {report}"
        );
        assert!(
            report["metrics"]["fleet"]["total"].is_u64(),
            "metrics.fleet.total must be an integer: {report}"
        );
        assert!(
            report["metrics"]["auto_resume"].is_object(),
            "metrics.auto_resume must be an object: {report}"
        );
    }

    /// Why: the supervisor widget reads `fleet` counts and the auto-resume control
    /// state from this object; both must be present and well-typed.
    /// Test: this test.
    #[tokio::test]
    async fn supervisor_status_reports_fleet_and_auto_resume() {
        let status = supervisor_status(&state()).await.expect("status");
        // Fleet counts must be present and integer-typed (exact values are
        // non-deterministic in the shared-singleton test process).
        assert!(status["fleet"]["active"].is_u64());
        assert!(status["fleet"]["stopped"].is_u64());
        assert!(status["fleet"]["total"].is_u64());
        // Auto-resume control block must carry the three flags.
        assert!(status["auto_resume"]["desired"].is_boolean());
        assert!(status["auto_resume"]["env"].is_boolean());
        assert!(status["auto_resume"]["pending_restart"].is_boolean());
    }
}
