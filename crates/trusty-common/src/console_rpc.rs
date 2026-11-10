//! The wire vocabulary of trusty-console's own Unix socket (#10902, ADR-0065).
//!
//! Why: the owner's D8b ruling gives the console a UDS listener so the TUI can
//! read the console's fleet view over the same transport every other service
//! uses (ADR-0032), and aggregate health over UDS (ADR-0035). The TUI lives in
//! trusty-mpm, which has no Cargo edge on trusty-console, so the method names
//! and the payload types it must decode live here, beside the other per-service
//! RPC vocabularies (`memory_rpc`, `mpm_rpc`).
//! What: the socket's service name, the method names, the
//! [`ConsoleHealth`](crate::console_rpc::ConsoleHealth) and
//! [`ConsoleFleetStatus`](crate::console_rpc::ConsoleFleetStatus) response
//! types, and the per-service row types
//! ([`ServiceInfo`](crate::console_rpc::ServiceInfo),
//! [`ServiceStatus`](crate::console_rpc::ServiceStatus),
//! [`ServiceLifecycle`](crate::console_rpc::ServiceLifecycle)) that moved here from `trusty_console::connector`,
//! which re-exports them. Ungated: the types need only `serde`, and a reader
//! should not compile the socket machinery to decode a payload. The
//! `console.machine_status` and `console.history` payloads embed host metrics,
//! so they live in the `metrics` submodule, gated on `console-metrics` +
//! `host-metrics` (#10942).
//!
//! **Forward compatibility.** No type here uses `deny_unknown_fields`, and every
//! field a later console adds must carry `#[serde(default)]`, so an older TUI
//! decodes a newer console's answer and the reverse.
//!
//! Test: `console_rpc_tests` below, and the console's
//! `console_uds::tests` over a real socket.

// See #10902

use serde::{Deserialize, Serialize};

// #10942: the machine-status and history payloads; see the module docs.
#[cfg(all(feature = "console-metrics", feature = "host-metrics"))]
pub mod metrics;

/// The service name the console's socket is derived from.
///
/// `crate::daemon_socket_path(CONSOLE_SERVICE)` is `{data_dir}/trusty-console.sock`
/// (ADR-0065 reserves that name for the console's own API).
pub const CONSOLE_SERVICE: &str = "trusty-console";

/// Liveness probe: answers [`ConsoleHealth`] and never touches the poller.
pub const METHOD_HEALTH: &str = "console.health";

/// The console's cached all-services snapshot, as [`ConsoleFleetStatus`].
pub const METHOD_FLEET_STATUS: &str = "console.fleet_status";

/// Whole-machine status, as `metrics::ConsoleMachineStatus` (#10942).
pub const METHOD_MACHINE_STATUS: &str = "console.machine_status";

/// The 10-minute host, transition and per-service window, as
/// `metrics::HistorySnapshot` (#10942, #10943).
pub const METHOD_HISTORY: &str = "console.history";

/// Every method the console's socket serves.
///
/// Why: a contract test compares the router's names against this list, so a
/// rename shows up as a failing assertion rather than as a consumer reporting
/// `method_not_found`. trusty-console's router test compares against it.
/// Test: `console_fleet_status_decodes_a_payload_missing_new_fields`.
pub const METHODS: &[&str] = &[
    METHOD_HEALTH,
    METHOD_FLEET_STATUS,
    METHOD_MACHINE_STATUS,
    METHOD_HISTORY,
];

/// The `console.health` answer.
///
/// Why: the TUI's Console tile needs "is the console up, which build, for how
/// long" in one cheap call. The field names match the other `<service>.health`
/// answers (`status`, `service`, `version`, `uptime_secs`) so one decoder reads
/// them all.
/// What: `status` is `"ok"` whenever the console answers; `service` is
/// [`CONSOLE_SERVICE`]; `version` is the console crate's version; `uptime_secs`
/// is whole seconds since the console built its state.
/// Test: `console_health_round_trips_and_tolerates_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleHealth {
    /// `"ok"` whenever the console answers.
    pub status: String,
    /// Always [`CONSOLE_SERVICE`].
    pub service: String,
    /// The console's crate version.
    pub version: String,
    /// Whole seconds since the console started.
    #[serde(default)]
    pub uptime_secs: u64,
}

/// The `console.fleet_status` answer: the console's cached services snapshot.
///
/// Why: the TUI home takes service status from the console when it is up
/// (D8b (b)), which only saves work if the answer comes from the cache the
/// console's own poller keeps warm — never a fresh probe per call.
/// What: `services` is exactly the body `GET /api/console/services` returns for
/// the same cache state: overlaid with MCP-handle state and per-service
/// metrics, ordered for display. `ready` is `false` until the first poll lands;
/// `services` is then empty and the reader should probe the services itself.
/// `overlaid` is `false` when the handle overlay could not finish inside its
/// budget and the rows are the poller's own, unmodified.
/// Test: `console_fleet_status_decodes_a_payload_missing_new_fields`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsoleFleetStatus {
    /// Whether the console's poller has produced a snapshot yet.
    #[serde(default)]
    pub ready: bool,
    /// Whether the MCP-handle and metrics overlays were applied.
    #[serde(default)]
    pub overlaid: bool,
    /// One row per service, ordered for display.
    #[serde(default)]
    pub services: Vec<ServiceInfo>,
}

/// Runtime status of one detected service.
///
/// Why: Four states capture everything the console needs — whether the binary
/// exists, whether a daemon is running, whether the MCP handshake succeeded but
/// the expected tool is missing (Degraded), and whether the binary is absent.
/// What: Serialises to a lowercase string for the JSON API.
/// Test: `console_fleet_status_decodes_a_payload_missing_new_fields`.
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

/// How a service is meant to run when it is healthy.
///
/// Why (#6416): `ServiceStatus::Available` means "the binary is installed and
/// nothing is serving". For an on-demand member that IS the healthy resting
/// state, and `status` alone cannot tell the two readings apart; this does.
/// What: a per-member constant, not an observation. A connector picks it once
/// and every `ServiceInfo` it builds carries it, `Absent` rows included.
/// Test: `console_fleet_status_decodes_a_payload_missing_new_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycle {
    /// A resident daemon. `Running` is the healthy state and `Available` means
    /// it is installed but stopped.
    #[default]
    Daemon,
    /// A per-invocation binary or an on-demand server. Installed IS healthy:
    /// `Available` is the resting state and `Running` merely means a server
    /// happens to be up right now.
    OnDemand,
}

/// All facts gathered about a service in one detection pass.
///
/// Why: `GET /api/console/services` and `console.fleet_status` both serialise
/// this directly, so every reader gets one stable row shape.
/// What: `id` is the stable machine identifier; `display_name` is human-
/// readable; `status` is the current runtime state; `version` is the daemon's
/// reported version when running; `url` is the daemon base URL when reachable;
/// `hint` is a remediation message surfaced when `status` is `Degraded`;
/// `lifecycle` says whether a stopped daemon or an installed on-demand binary
/// is the healthy reading of that status (#6416); `cpu_pct` and `rss_bytes`
/// are the newest per-service samples (#6642, #6773).
/// Test: `console_fleet_status_decodes_a_payload_missing_new_fields`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Stable machine identifier (e.g. `"trusty-search"`).
    pub id: String,
    /// Human-readable display name (e.g. `"Trusty Search"`).
    pub display_name: String,
    /// Current runtime state.
    pub status: ServiceStatus,
    /// Version string reported by the running daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Base URL of the running daemon (e.g. `"http://127.0.0.1:7879"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Actionable remediation hint — present when `status` is `Degraded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Whether this member runs as a resident daemon or on demand (#6416).
    ///
    /// Serialised unconditionally — the UI branches on it — and `#[serde(default)]`
    /// so a payload written before #6416 still deserialises, as a daemon.
    #[serde(default)]
    pub lifecycle: ServiceLifecycle,
    /// The newest per-second CPU sample for this service, if any (#6642).
    ///
    /// Serialised UNCONDITIONALLY, as `null` when absent: `None` means no
    /// measurement was taken, and rendering that as `0.0` would draw an idle
    /// service where there is none. `#[serde(default)]` so a payload written
    /// before #6642 still deserialises.
    #[serde(default)]
    pub cpu_pct: Option<f32>,
    /// The newest per-second resident-memory sample, in bytes, if any (#6773).
    ///
    /// Same contract as `cpu_pct`: `null` when absent, never zero bytes, and
    /// `#[serde(default)]` so a payload written before #6773 still deserialises.
    #[serde(default)]
    pub rss_bytes: Option<u64>,
}

#[cfg(test)]
mod console_rpc_tests {
    use super::*;

    /// Why: an older TUI must decode a newer console's health answer.
    /// What: a payload with an extra field decodes; a typed value round-trips.
    /// Test: this test itself.
    #[test]
    fn console_health_round_trips_and_tolerates_unknown_fields() {
        let health = ConsoleHealth {
            status: "ok".into(),
            service: CONSOLE_SERVICE.into(),
            version: "1.2.3".into(),
            uptime_secs: 7,
        };
        let value = serde_json::to_value(&health).expect("encode");
        assert_eq!(
            value,
            serde_json::json!({"status":"ok","service":"trusty-console","version":"1.2.3","uptime_secs":7})
        );
        let mut extended = value;
        extended["added_later"] = serde_json::json!(true);
        let back: ConsoleHealth = serde_json::from_value(extended).expect("decode");
        assert_eq!(back, health);
    }

    /// Why: forward compatibility in both directions — a newer TUI reading an
    /// older console, and an older TUI reading a newer one.
    /// What: decodes a row missing `lifecycle`/`cpu_pct`/`rss_bytes` and an
    /// envelope missing `overlaid` but carrying an unknown key.
    /// Test: this test itself.
    #[test]
    fn console_fleet_status_decodes_a_payload_missing_new_fields() {
        let payload = serde_json::json!({
            "ready": true,
            "future_field": 1,
            "services": [{
                "id": "trusty-search",
                "display_name": "Trusty Search",
                "status": "running",
                "future_row_field": "x"
            }]
        });
        let fleet: ConsoleFleetStatus = serde_json::from_value(payload).expect("decode");
        assert!(fleet.ready);
        assert!(!fleet.overlaid);
        let row = &fleet.services[0];
        assert_eq!(row.status, ServiceStatus::Running);
        assert_eq!(row.lifecycle, ServiceLifecycle::Daemon);
        assert_eq!(row.cpu_pct, None);
        assert_eq!(
            METHODS,
            [
                METHOD_HEALTH,
                METHOD_FLEET_STATUS,
                METHOD_MACHINE_STATUS,
                METHOD_HISTORY
            ]
        );
    }
}
