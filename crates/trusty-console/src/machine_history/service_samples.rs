//! The per-second, per-service sample the home-page cards graph (#6642).
//!
//! Why: #6641 gave the console a 10-minute window of WHOLE-MACHINE samples plus
//! a log of the moments a service changed state. The owner's ruling for the home
//! page needs a third thing: a bar per second per service, showing that
//! service's own CPU. A transition log cannot draw it (it records changes, not
//! levels) and the host sample cannot either (it is one number for the whole
//! machine).
//!
//! Why `cpu_pct` and `rss_bytes` are `Option` and never `0` for an absent
//! service: a service that is not installed, is on-demand and idle, or whose
//! process the console could not identify has NO measurement. Rendering that as
//! zero draws a flat idle bar, which is visually identical to a healthy service
//! doing nothing — the one reading the operator must be able to tell apart.
//! `None` is the honest answer and the UI renders it as a gap.
//!
//! Why memory rides on the SAME sample as CPU (#6773): the owner's ruling puts
//! a CPU graph and a memory graph side by side in every service row. A second
//! sampler, a second ring or a second event would each let the two graphs show
//! different seconds, which is the one way a side-by-side pair can lie. One
//! sample, one tick, one ring, one event.
//!
//! What: [`ServiceSample`] is one service at one instant; [`ServiceSampleBatch`]
//! is the whole roster at one instant, which is what the sampler produces per
//! tick, what the `services` SSE event carries, and what
//! [`MachineHistory::record_service_samples`] folds into the per-service rings.
//! [`SERVICE_HISTORY_CAPACITY`] sizes those rings to the same 10-minute window
//! the host ring covers.
//! Test: `a_sample_serialises_the_shape_the_ui_reads`,
//! `an_absent_cpu_serialises_as_null`, `an_absent_memory_serialises_as_null`,
//! `a_batch_names_its_services`.
//!
//! [`MachineHistory::record_service_samples`]:
//!     crate::machine_history::MachineHistory::record_service_samples

use serde::{Deserialize, Serialize};
use trusty_common::host_metrics::history::HOST_HISTORY_CAPACITY;

use crate::connector::ServiceStatus;

/// Samples retained per service — the same 10-minute window the host ring holds.
///
/// Why: the home-page card draws the host graph and the service graph on one
/// x-axis. Two different capacities would put two different spans side by side
/// with nothing saying so, so this is defined as the host constant rather than
/// as a second number that could drift from it.
/// What: [`HOST_HISTORY_CAPACITY`] — 600 points at the 1 s cadence.
/// Test: `the_service_window_matches_the_host_window`.
pub const SERVICE_HISTORY_CAPACITY: usize = HOST_HISTORY_CAPACITY;

/// One service's state, CPU and memory at one instant (#6642, #6773).
///
/// Why: the row needs all three together. CPU alone cannot say whether a
/// missing bar means "idle" or "stopped", status alone cannot draw a graph, and
/// a memory figure sampled on a different tick than the CPU figure would put two
/// different seconds beside each other in one row.
/// What: the service id (the same `ServiceInfo::id` the services route serves,
/// so the UI joins the two by string), the status the poller last detected, the
/// CPU percentage, and the resident-memory figure in bytes — each `None`
/// whenever no measurement was taken. The percentage follows `sysinfo`'s
/// convention: `100.0` is one fully-saturated core, so a multi-threaded daemon
/// can exceed 100.
/// Test: `a_sample_serialises_the_shape_the_ui_reads`,
/// `an_absent_cpu_serialises_as_null`, `an_absent_memory_serialises_as_null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSample {
    /// Stable service identifier, e.g. `"trusty-search"`.
    pub id: String,
    /// The status the health poller last detected for this service.
    pub status: ServiceStatus,
    /// Process CPU percentage, or `None` when no measurement was taken.
    ///
    /// Serialised UNCONDITIONALLY, as `null` when absent — unlike the optional
    /// fields on `ServiceInfo`, which are skipped. The UI must distinguish "no
    /// measurement" from a value, and a key that is sometimes missing and
    /// sometimes `null` is two spellings of the same fact for the client to
    /// handle.
    pub cpu_pct: Option<f32>,
    /// Process resident memory in bytes, or `None` when nothing was measured.
    ///
    /// Why it rides on this struct rather than on a second event (#6773): the
    /// owner's ruling puts a CPU graph and a memory graph side by side in one
    /// row, and two events would let the two graphs show different seconds. One
    /// sample carries both, so they cannot disagree.
    ///
    /// Why bytes: the UI formats to `MiB`/`GiB`, and a megabyte-granular figure
    /// quantises a small daemon's 1 s curve into a staircase.
    ///
    /// Same `null`-not-zero contract as `cpu_pct`, for the same reason and with
    /// the same unconditional serialisation. `#[serde(default)]` so a payload
    /// written before #6773 still deserialises.
    #[serde(default)]
    pub rss_bytes: Option<u64>,
}

/// The whole service roster at one instant (#6642).
///
/// Why one batch rather than N independent samples: the graphs are read
/// together, and a client that received six separate events would have to
/// re-group them by timestamp to draw one column. Batching also means one
/// broadcast send per tick instead of six.
/// What: the wall-clock second the batch was taken plus one [`ServiceSample`]
/// per registered service, in the order the poller reported them (the UI sorts;
/// see #6642 PR-B).
/// Test: `a_batch_names_its_services`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSampleBatch {
    /// Unix seconds when the batch was taken. `0` when the clock is unreadable.
    pub sampled_at_unix: u64,
    /// One entry per registered service.
    pub services: Vec<ServiceSample>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, cpu: Option<f32>) -> ServiceSample {
        ServiceSample {
            id: id.to_string(),
            status: ServiceStatus::Running,
            cpu_pct: cpu,
            // #6773: a measured service carries both figures; the memory-absent
            // case has its own test below.
            rss_bytes: cpu.map(|_| 142 * 1024 * 1024),
        }
    }

    /// Why: PR-B renders straight off these three keys; a rename that only
    /// changed the Rust identifier would leave the card blank with nothing red.
    /// What: asserts the exact JSON keys and value spellings.
    /// Test: this test itself.
    #[test]
    fn a_sample_serialises_the_shape_the_ui_reads() {
        let json = serde_json::to_value(sample("trusty-search", Some(12.5))).expect("serialise");
        assert_eq!(
            json,
            serde_json::json!({
                "id": "trusty-search",
                "status": "running",
                "cpu_pct": 12.5,
                "rss_bytes": 142 * 1024 * 1024,
            })
        );
    }

    /// REGRESSION (#6773): an unmeasurable service must serialise `rss_bytes` as
    /// an explicit `null`, not omit the key and not report `0`.
    ///
    /// Why: this is the memory half of `an_absent_cpu_serialises_as_null`. A
    /// zero draws a bar at the bottom of the memory graph, which reads as a
    /// daemon holding almost nothing rather than as a daemon that is not there.
    /// What: serialises a sample with no measurement and asserts the key is
    /// present and null.
    /// Test: this test itself.
    #[test]
    fn an_absent_memory_serialises_as_null() {
        let json = serde_json::to_value(sample("trusty-review", None)).expect("serialise");
        assert!(
            json.get("rss_bytes").is_some(),
            "the key must be present so the client has one shape to parse"
        );
        assert!(json["rss_bytes"].is_null(), "absent must be null, not 0");
    }

    /// REGRESSION (#6642): an unmeasurable service must serialise `cpu_pct` as
    /// an explicit `null`, not omit the key and not report `0.0`.
    ///
    /// Why: a zero draws a flat idle bar, which reads as a healthy quiet daemon.
    /// An omitted key makes the client handle two spellings of "absent".
    /// What: serialises a `None` sample and asserts the key is present and null.
    /// Test: this test itself.
    #[test]
    fn an_absent_cpu_serialises_as_null() {
        let json = serde_json::to_value(sample("trusty-review", None)).expect("serialise");
        assert!(
            json.get("cpu_pct").is_some(),
            "the key must be present so the client has one shape to parse"
        );
        assert!(json["cpu_pct"].is_null(), "absent must be null, not 0.0");
    }

    /// Why: the SSE consumer keys the column on `sampled_at_unix` and the rows
    /// on `services`.
    /// What: round-trips a batch and asserts both fields survive.
    /// Test: this test itself.
    #[test]
    fn a_batch_names_its_services() {
        let batch = ServiceSampleBatch {
            sampled_at_unix: 1_700_000_000,
            services: vec![
                sample("trusty-search", Some(1.0)),
                sample("trusty-mpm", None),
            ],
        };
        let text = serde_json::to_string(&batch).expect("serialise");
        let back: ServiceSampleBatch = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, batch);
        assert_eq!(back.services.len(), 2);
    }

    /// Why: the host graph and the service graph share one x-axis, so two
    /// capacities would silently put two different spans side by side.
    /// What: asserts the service capacity IS the host capacity.
    /// Test: this test itself.
    #[test]
    fn the_service_window_matches_the_host_window() {
        assert_eq!(SERVICE_HISTORY_CAPACITY, HOST_HISTORY_CAPACITY);
        assert_eq!(SERVICE_HISTORY_CAPACITY, 600);
    }
}
