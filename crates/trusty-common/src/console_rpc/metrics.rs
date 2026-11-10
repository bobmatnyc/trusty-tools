//! Host metrics and the 10-minute history over the console socket (#10942, #10943).
//!
//! Why: `tm tui` must draw the console's machine cards and their sparklines
//! over UDS alone (#10899), and it has no Cargo edge on trusty-console. The
//! payloads `GET /api/console/machine-status` and
//! `GET /api/console/machine-status/history` serve were defined inside
//! trusty-console; they move here so both ends decode one definition.
//! What: the params and answers of `console.machine_status`
//! ([`MachineStatusRequest`] → [`ConsoleMachineStatus`]) and `console.history`
//! ([`HistoryRequest`] → [`HistorySnapshot`]), plus the history's row types
//! ([`ServiceSample`], [`ServiceTransition`], [`ServiceState`]) that
//! trusty-console re-exports from `machine_history`. The per-service CPU/RSS
//! rings (#10943) ride on [`HistorySnapshot::service_samples`].
//! Gated on `console-metrics` + `host-metrics`, the two modules the payloads
//! embed ([`MachineStatus`], [`HostMetrics`]).
//! Test: `metrics_tests.rs` beside this file, and trusty-console's
//! `console_uds::metrics_tests` over a real socket.

// #10942: moved from trusty_console::machine_history so the TUI can decode it.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::ServiceStatus;
use crate::console_metrics::ServiceHealth;
use crate::console_metrics::machine_status::MachineStatus;
use crate::host_metrics::HostMetrics;

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;

/// The schema version [`HistorySnapshot`] advertises.
///
/// Why: lets a client detect a shape change instead of mis-rendering one,
/// as `MachineStatus::schema_version` does.
/// What: bumped on any change a client must notice. `2` (#6642) added
/// `service_samples`; `3` (#6773) added `ServiceSample::rss_bytes`; `4`
/// (#6908) added `sse_client_count`.
/// Test: `history_snapshot_golden_json`.
pub const MACHINE_HISTORY_SCHEMA_VERSION: u32 = 4;

/// Params of `console.machine_status`: none today.
///
/// Why: a typed request lets a later console add a param without a new method.
/// What: encodes as `{}`; decodes from an absent or `null` params, an object
/// (keys ignored) or an empty array. Any other value is refused, so the router
/// answers `invalid_params` rather than guessing.
/// Test: `request_types_accept_absent_null_or_an_object`,
/// `request_types_refuse_a_scalar_or_a_non_empty_array`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MachineStatusRequest {}

impl<'de> Deserialize<'de> for MachineStatusRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EmptyParams).map(|()| Self {})
    }
}

/// Params of `console.history`: none today. Same decode rules as
/// [`MachineStatusRequest`].
///
/// Test: `request_types_accept_absent_null_or_an_object`,
/// `request_types_refuse_a_scalar_or_a_non_empty_array`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HistoryRequest {}

impl<'de> Deserialize<'de> for HistoryRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EmptyParams).map(|()| Self {})
    }
}

/// Accepts the spellings of "no params" and refuses everything else.
struct EmptyParams;

impl<'de> Visitor<'de> for EmptyParams {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no params: absent, null, an object or an empty array")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::invalid_length(1, &self));
        }
        Ok(())
    }
}

/// The `console.machine_status` answer.
///
/// Why: the HTTP route answers `503` until the first host sample lands; a UDS
/// call has no status code, so readiness travels in the body, as
/// `ConsoleFleetStatus::ready` does.
/// What: `ready` is `false` and `machine` is `null` before the first host
/// sample. Otherwise `machine` is exactly the body
/// `GET /api/console/machine-status` serves for the same caches.
/// Test: `machine_status_not_ready_golden_json`,
/// `machine_status_ready_round_trips`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsoleMachineStatus {
    /// Whether the console's host sampler has produced a sample yet.
    #[serde(default)]
    pub ready: bool,
    /// The whole-machine status; `null` until `ready`.
    #[serde(default)]
    pub machine: Option<MachineStatus>,
}

/// One service's state, CPU and memory at one instant (#6642, #6773).
///
/// Why: a service row draws a CPU and a memory graph side by side; one sample
/// carrying both keeps the two graphs on the same second.
/// What: the service id (joins `ServiceInfo::id`), the status the poller last
/// detected, the CPU percentage (`100.0` is one saturated core) and the
/// resident memory in bytes. Each figure is `null` when nothing was measured —
/// never `0`, which would draw an idle service where there is none. Both keys
/// are serialised unconditionally, so a client parses one shape.
/// Test: `service_sample_serialises_absent_figures_as_null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSample {
    /// Stable service identifier, e.g. `"trusty-search"`.
    pub id: String,
    /// The status the health poller last detected for this service.
    pub status: ServiceStatus,
    /// Process CPU percentage, or `None` when no measurement was taken.
    pub cpu_pct: Option<f32>,
    /// Process resident memory in bytes, or `None` when nothing was measured.
    /// `#[serde(default)]` so a payload written before #6773 still decodes.
    #[serde(default)]
    pub rss_bytes: Option<u64>,
}

/// The state a service is in, as the transition log records it (#6641).
///
/// Why: `ServiceHealth` is what a service says about itself and cannot say
/// "not heard from"; the timeline needs that fourth state.
/// What: `Up` / `Degraded` map from `Ok` / `Degraded`; `Down` covers a
/// self-reported `Error` and a report gone stale; `Unknown` is a service never
/// observed. Serialised lowercase.
/// Test: `service_state_maps_from_self_reported_health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    /// The service reported `ok` within the grace window.
    Up,
    /// The service reported `degraded` within the grace window.
    Degraded,
    /// The service reported `error`, or has not reported within the grace window.
    Down,
    /// The tracker has never observed this service.
    Unknown,
}

impl From<&ServiceHealth> for ServiceState {
    /// Map a service's self-reported health onto the log's state.
    fn from(h: &ServiceHealth) -> Self {
        match h {
            ServiceHealth::Ok => ServiceState::Up,
            ServiceHealth::Degraded => ServiceState::Degraded,
            ServiceHealth::Error => ServiceState::Down,
        }
    }
}

/// One recorded change of a service's state (#6641).
///
/// Why: the timeline draws a band per state, so it needs both ends of the
/// change and when it happened.
/// What: the service's id and display name, the state it left, the state it
/// entered, and the Unix second the console observed the change.
/// Test: `history_snapshot_golden_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTransition {
    /// Machine-readable service id (e.g. `"trusty-search"`).
    pub service_id: String,
    /// Human-readable display name for the timeline row.
    pub display_name: String,
    /// The state the service was in before this observation.
    pub from: ServiceState,
    /// The state the service is in now.
    pub to: ServiceState,
    /// Unix seconds when the console observed the change.
    pub at_unix: u64,
}

/// The `console.history` answer, and the body of
/// `GET /api/console/machine-status/history` and the SSE `history` event.
///
/// Why: one shape for every reader. The capacities and the sample interval
/// travel with the data because a graph's x-axis is derived from them.
/// What: host samples and transitions oldest-first; per-service sample rings
/// keyed by service id, oldest-first within each id (#6642, #10943); the
/// capacities; the sampler's interval; the count of attached browser SSE
/// streams (#6908 — not a UDS or bus client count); and the schema version.
/// Before the first sample every collection is empty, which is a complete
/// answer ("no samples yet"), not an error. Every field defaults on decode, so
/// a newer TUI reads an older console.
/// Test: `history_snapshot_golden_json`,
/// `history_snapshot_decodes_a_payload_missing_fields`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySnapshot {
    /// Host samples, oldest first. Empty before the first sample.
    pub samples: Vec<HostMetrics>,
    /// Service state changes, oldest first. Empty until a service changes state.
    pub transitions: Vec<ServiceTransition>,
    /// Per-service samples keyed by service id, oldest first within each id.
    /// A `BTreeMap` so the JSON key order is stable across responses.
    pub service_samples: BTreeMap<String, Vec<ServiceSample>>,
    /// Maximum host samples retained (the ring's capacity).
    pub sample_capacity: usize,
    /// Maximum samples retained per service.
    pub service_sample_capacity: usize,
    /// Maximum transitions retained.
    pub transition_capacity: usize,
    /// Seconds between samples, as the running sampler is configured.
    pub sample_interval_secs: u64,
    /// Browser streams attached to the machine-status SSE endpoint at the
    /// instant this snapshot was built (#6908).
    pub sse_client_count: usize,
    /// See [`MACHINE_HISTORY_SCHEMA_VERSION`].
    pub schema_version: u32,
}
