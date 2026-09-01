//! Aggregated machine-status model for the Foundry dashboard (#6517).
//!
//! Why: the dashboard's home view needs ONE payload describing the whole
//!      machine: the host's own resources ([`HostMetrics`]) plus a rollup of
//!      the per-service health each local trusty-* daemon already reports via
//!      [`ConsoleMetricsReport`]. Building that shape here — beside the
//!      `console_metrics` contract it rolls up — keeps the console a thin
//!      assembler and lets any future consumer reuse the type.
//! What: [`MachineStatus`] combines a `HostMetrics` snapshot with a
//!      [`ServiceRollup`] derived from the per-service reports. [`ServiceSummary`]
//!      is the slim per-service view (identity + health) — it deliberately drops
//!      each report's opaque `metrics` blob, which the per-service endpoints
//!      already serve. Gated behind BOTH `console-metrics` (this parent module)
//!      and `host-metrics` (the `host_metrics` module it embeds).
//! Test: the inline `tests` module — `assemble_counts_by_health`,
//!      `rollup_drops_opaque_metrics`, `machine_status_serde_round_trip`.

use serde::{Deserialize, Serialize};

use super::{ConsoleMetricsReport, ServiceHealth};
use crate::host_metrics::HostMetrics;

/// The machine-status JSON schema version the phase-2 UI negotiates against.
///
/// Why: the UI can detect when the assembled shape changed and needs an update,
///      mirroring `ConsoleMetricsReport::metrics_schema_version`.
/// What: a monotonically increasing integer bumped on any breaking shape change.
/// Test: `machine_status_serde_round_trip` asserts it serialises.
pub const MACHINE_STATUS_SCHEMA_VERSION: u32 = 1;

/// A slim per-service health line rolled up from a [`ConsoleMetricsReport`]
/// (#6517).
///
/// Why: the whole-machine view lists each service's health without the opaque,
///      per-service `metrics` payload — that stays behind the existing
///      `/api/console/metrics/<service>` endpoints. Carrying only identity +
///      health keeps the machine-status payload small and stable.
/// What: the report's identity, coarse status, schema version, and collection
///      time. `metrics` is intentionally omitted.
/// Test: `rollup_drops_opaque_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSummary {
    /// Machine-readable service id (e.g. `"trusty-search"`).
    pub service_id: String,
    /// Human-readable display name for the dashboard.
    pub display_name: String,
    /// The service's crate semver string.
    pub version: String,
    /// Coarse health classification.
    pub status: ServiceHealth,
    /// The service's own `metrics` schema version (for the drill-down UI).
    pub metrics_schema_version: u32,
    /// Unix seconds when the service collected its report, if provided.
    pub collected_at_unix: Option<u64>,
}

impl From<&ConsoleMetricsReport> for ServiceSummary {
    /// Project a full report down to its rollup summary, dropping `metrics`.
    fn from(r: &ConsoleMetricsReport) -> Self {
        Self {
            service_id: r.service_id.clone(),
            display_name: r.display_name.clone(),
            version: r.version.clone(),
            status: r.status.clone(),
            metrics_schema_version: r.metrics_schema_version,
            collected_at_unix: r.collected_at_unix,
        }
    }
}

/// Health counts + per-service summaries across every reporting service (#6517).
///
/// Why: the dashboard's "services" tile shows how many are ok/degraded/error at
///      a glance, plus the per-service list. Counting once, server-side, keeps
///      every client consistent.
/// What: `total` is the number of services that produced a report;
///      `ok`/`degraded`/`error` partition them by [`ServiceHealth`]. `services`
///      holds the slim per-service summaries.
/// Test: `assemble_counts_by_health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRollup {
    /// Number of services that produced a report.
    pub total: usize,
    /// Count reporting [`ServiceHealth::Ok`].
    pub ok: usize,
    /// Count reporting [`ServiceHealth::Degraded`].
    pub degraded: usize,
    /// Count reporting [`ServiceHealth::Error`].
    pub error: usize,
    /// The slim per-service summaries, in the order supplied.
    pub services: Vec<ServiceSummary>,
}

impl ServiceRollup {
    /// Roll up a slice of reports into health counts + summaries.
    ///
    /// Why: the ONE place a report set becomes a rollup, so the counts and the
    ///      list can never disagree.
    /// What: maps each report to a [`ServiceSummary`] and tallies by status.
    ///      Services absent from `reports` (never polled, or binary missing)
    ///      simply do not appear — the console decides which services to include
    ///      by which caches were warm.
    /// Test: `assemble_counts_by_health`.
    #[must_use]
    pub fn from_reports(reports: &[ConsoleMetricsReport]) -> Self {
        let (mut ok, mut degraded, mut error) = (0usize, 0usize, 0usize);
        let mut services = Vec::with_capacity(reports.len());
        for r in reports {
            match r.status {
                ServiceHealth::Ok => ok += 1,
                ServiceHealth::Degraded => degraded += 1,
                ServiceHealth::Error => error += 1,
            }
            services.push(ServiceSummary::from(r));
        }
        Self {
            total: reports.len(),
            ok,
            degraded,
            error,
            services,
        }
    }
}

/// The aggregated whole-machine status the Foundry dashboard renders (#6517).
///
/// Why: one payload combining host resources and per-service health is what the
///      dashboard home view needs; assembling it server-side keeps the phase-2
///      UI a pure renderer.
/// What: the [`HostMetrics`] snapshot, the [`ServiceRollup`], the schema
///      version, and the assembly timestamp.
/// Test: `machine_status_serde_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineStatus {
    /// Whole-machine host resource snapshot.
    pub host: HostMetrics,
    /// Per-service health rollup.
    pub services: ServiceRollup,
    /// The schema version of this payload; see [`MACHINE_STATUS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Unix seconds when the console assembled this status, or `None` on a clock
    /// read failure.
    pub assembled_at_unix: Option<u64>,
}

impl MachineStatus {
    /// Assemble a [`MachineStatus`] from a host snapshot and the per-service
    /// reports.
    ///
    /// Why: the console's machine-status route calls this after reading its
    ///      host-metrics cache and each service's metrics cache; centralising the
    ///      assembly keeps the route a one-liner and the shape testable.
    /// What: takes ownership of `host`, rolls up `reports`, stamps the schema
    ///      version and the current Unix time.
    /// Test: `assemble_counts_by_health`, `machine_status_serde_round_trip`.
    #[must_use]
    pub fn assemble(host: HostMetrics, reports: &[ConsoleMetricsReport]) -> Self {
        Self {
            host,
            services: ServiceRollup::from_reports(reports),
            schema_version: MACHINE_STATUS_SCHEMA_VERSION,
            assembled_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console_metrics::make_report;
    use serde_json::json;

    fn sample_host() -> HostMetrics {
        crate::host_metrics::HostSampler::new().sample()
    }

    /// Why: the rollup counts drive the dashboard's health tile; a miscount
    ///      mis-reports the fleet.
    /// What: three reports (ok, degraded, error) roll up to the matching counts
    ///      and `total == 3`.
    /// Test: this test.
    #[test]
    fn assemble_counts_by_health() {
        let reports = vec![
            make_report("a", "A", "1.0.0", ServiceHealth::Ok, json!({}), 1),
            make_report("b", "B", "1.0.0", ServiceHealth::Degraded, json!({}), 1),
            make_report("c", "C", "1.0.0", ServiceHealth::Error, json!({}), 1),
        ];
        let status = MachineStatus::assemble(sample_host(), &reports);
        assert_eq!(status.services.total, 3);
        assert_eq!(status.services.ok, 1);
        assert_eq!(status.services.degraded, 1);
        assert_eq!(status.services.error, 1);
        assert_eq!(status.schema_version, MACHINE_STATUS_SCHEMA_VERSION);
    }

    /// Why: the machine-status payload must stay small; the opaque per-service
    ///      `metrics` blob belongs to the drill-down endpoints, not this rollup.
    /// What: a report carrying a fat `metrics` object rolls up to a summary whose
    ///      serialised form contains no `metrics` field but keeps identity/health.
    /// Test: this test.
    #[test]
    fn rollup_drops_opaque_metrics() {
        let reports = vec![make_report(
            "trusty-search",
            "Search",
            "0.24.1",
            ServiceHealth::Ok,
            json!({ "index_count": 42, "big": [1, 2, 3, 4, 5] }),
            3,
        )];
        let rollup = ServiceRollup::from_reports(&reports);
        assert_eq!(rollup.services.len(), 1);
        let summary = &rollup.services[0];
        assert_eq!(summary.service_id, "trusty-search");
        assert_eq!(summary.metrics_schema_version, 3);
        let value = serde_json::to_value(summary).unwrap();
        assert!(
            value.get("metrics").is_none(),
            "ServiceSummary must not carry the opaque metrics payload"
        );
    }

    /// Why: the JSON contract the phase-2 UI renders must round-trip intact.
    /// What: assembles a status, serialises it, deserialises it back, and asserts
    ///      the host core fields and rollup counts survive.
    /// Test: this test.
    #[test]
    fn machine_status_serde_round_trip() {
        let reports = vec![make_report(
            "trusty-memory",
            "Memory",
            "0.46.5",
            ServiceHealth::Ok,
            json!(null),
            1,
        )];
        let status = MachineStatus::assemble(sample_host(), &reports);
        let s = serde_json::to_string(&status).expect("serialise MachineStatus");
        let back: MachineStatus = serde_json::from_str(&s).expect("deserialise MachineStatus");
        assert_eq!(back.services.total, 1);
        assert_eq!(back.services.ok, 1);
        assert_eq!(back.schema_version, status.schema_version);
        assert_eq!(back.host.cpu.logical_cores, status.host.cpu.logical_cores);
    }
}
