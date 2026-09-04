//! The events `/api/console/machine-status/stream` carries, and their SSE
//! framing (#6641).
//!
//! Why: the browser needs to tell a fresh sample from a service transition from
//! a gap in its own delivery, and an SSE `data:` line alone cannot say which.
//! Naming the kind on an `event:` line lets `EventSource.addEventListener`
//! dispatch each kind to its own handler, which is what the Phase 3 PR2 UI
//! (#6642) wires the sparklines and the timeline to.
//! What: [`HistoryEvent`] is what the console broadcasts to every connected
//! subscriber; [`sse_frame`] renders any payload as
//! `event: <kind>\ndata: {json}\n\n`, the same one-line-per-event framing the
//! #6524 search relay uses. [`lagged_frame`] is the fourth kind: a subscriber
//! that fell far enough behind for the broadcast channel to drop messages is
//! told how many it lost rather than being left with a silent gap in its graph.
//! Test: `a_frame_names_its_kind_on_one_line`,
//! `a_sample_frame_carries_the_host_snapshot`, `a_lagged_frame_carries_the_count`.

use axum::body::Bytes;
use serde::Serialize;
use trusty_common::host_metrics::HostMetrics;

use super::service_samples::ServiceSampleBatch;
use super::transitions::ServiceTransition;

/// One event pushed to every open machine-status stream (#6641).
///
/// Why: the sampler and the transition tracker both produce updates on the same
/// cadence, and a single broadcast channel keeps them ordered relative to each
/// other for every subscriber.
/// What: `Sample` carries a whole new [`HostMetrics`] snapshot; `Transition`
/// carries one [`ServiceTransition`]. Both are `Clone` because
/// `tokio::sync::broadcast` hands each receiver its own copy. The `history`
/// event is deliberately NOT a variant — it is sent once, per connection, from
/// the subscriber's own snapshot and never broadcast.
/// Test: `a_sample_frame_carries_the_host_snapshot`.
#[derive(Debug, Clone)]
pub enum HistoryEvent {
    /// A new host-metrics sample entered the ring.
    Sample(Box<HostMetrics>),
    /// A service changed state.
    Transition(Box<ServiceTransition>),
    /// One tick's per-service status + CPU samples, whole roster (#6642).
    ///
    /// Why its own event rather than a field on `Sample`: the host sample is one
    /// object and this is a list, and the #6284 transport inversion will have
    /// services push their own samples — at which point the two halves arrive
    /// from different producers on different schedules. Keeping them separate
    /// events now means that cutover changes who sends, not what the browser
    /// parses.
    Services(Box<ServiceSampleBatch>),
}

impl HistoryEvent {
    /// The `event:` name this variant is delivered under.
    ///
    /// Test: `a_frame_names_its_kind_on_one_line`,
    /// `a_services_frame_carries_the_whole_roster`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            HistoryEvent::Sample(_) => "sample",
            HistoryEvent::Transition(_) => "transition",
            HistoryEvent::Services(_) => "services",
        }
    }

    /// Render this event as one SSE frame.
    ///
    /// Why: keeping the kind and its payload together here means a new variant
    /// cannot be added without also giving it a name on the wire.
    /// What: delegates to [`sse_frame`] with [`HistoryEvent::kind`].
    /// Test: `a_sample_frame_carries_the_host_snapshot`.
    #[must_use]
    pub fn frame(&self) -> Bytes {
        match self {
            HistoryEvent::Sample(m) => sse_frame("sample", m.as_ref()),
            HistoryEvent::Transition(t) => sse_frame("transition", t.as_ref()),
            HistoryEvent::Services(b) => sse_frame("services", b.as_ref()),
        }
    }
}

/// Render `payload` as `event: <kind>\ndata: {json}\n\n`.
///
/// Why serialising rather than passing raw text through: SSE terminates an
/// event at a blank line, so an embedded newline would split one event into two
/// malformed ones. `serde_json::to_string` emits no newlines, which is what
/// keeps every event exactly one `data:` line — the same reason the #6524 relay
/// re-serialises its parsed frames.
/// What: a `Bytes` frame. A payload that fails to serialise (which no type here
/// can) degrades to a JSON object naming the error rather than dropping the
/// event silently.
/// Test: `a_frame_names_its_kind_on_one_line`.
#[must_use]
pub fn sse_frame(kind: &str, payload: &impl Serialize) -> Bytes {
    let json = serde_json::to_string(payload)
        .unwrap_or_else(|e| format!(r#"{{"error":"serialise {kind}: {e}"}}"#));
    Bytes::from(format!("event: {kind}\ndata: {json}\n\n"))
}

/// The frame a subscriber gets when the broadcast channel dropped messages.
///
/// Why: a browser that stalls (a backgrounded tab, a slow machine) can fall
/// further behind than the channel's buffer. `tokio::sync::broadcast` then drops
/// the oldest messages and reports how many. Saying so is the difference between
/// a graph the viewer knows has a hole and one that silently lies.
/// What: `event: lagged` carrying `{"dropped": <n>}`.
/// Test: `a_lagged_frame_carries_the_count`.
#[must_use]
pub fn lagged_frame(dropped: u64) -> Bytes {
    sse_frame("lagged", &serde_json::json!({ "dropped": dropped }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an `EventSource` listener keyed on the event name never fires if the
    /// `event:` line is missing or on the wrong line.
    /// What: asserts the exact three-part shape — name line, data line, blank
    /// terminator — and that the data is a single line.
    /// Test: this test.
    #[test]
    fn a_frame_names_its_kind_on_one_line() {
        let frame = sse_frame("transition", &serde_json::json!({ "a": 1 }));
        let text = String::from_utf8(frame.to_vec()).expect("utf8 frame");
        assert_eq!(text, "event: transition\ndata: {\"a\":1}\n\n");
        let data_lines = text.lines().filter(|l| l.starts_with("data: ")).count();
        assert_eq!(data_lines, 1, "one data line per event");
    }

    /// Why: the sparkline reads the sample straight off the frame, so the frame
    /// must carry the whole snapshot under the `sample` name.
    /// What: frames a real sample and asserts the kind line plus a field the UI
    /// reads back out of the JSON.
    /// Test: this test.
    #[test]
    fn a_sample_frame_carries_the_host_snapshot() {
        let metrics = trusty_common::host_metrics::HostSampler::new().sample();
        let cores = metrics.cpu.logical_cores;
        let event = HistoryEvent::Sample(Box::new(metrics));
        assert_eq!(event.kind(), "sample");
        let text = String::from_utf8(event.frame().to_vec()).expect("utf8 frame");
        let data = text
            .strip_prefix("event: sample\ndata: ")
            .and_then(|r| r.strip_suffix("\n\n"))
            .expect("sample frame shape");
        let parsed: serde_json::Value = serde_json::from_str(data).expect("sample json");
        assert_eq!(parsed["cpu"]["logical_cores"], cores);
    }

    /// Why (#6642): PR-B adds one `EventSource` listener per event name, so the
    /// per-service batch must arrive under `services` with the roster intact —
    /// a rename or a dropped field leaves the card graphs empty with nothing red
    /// anywhere.
    /// What: frames a two-service batch and asserts the kind line, the
    /// timestamp, and both rows — including that ONE event carries the CPU and
    /// memory figures the row's two graphs need (#6773), and that an absent
    /// measurement of either is null.
    /// Test: this test.
    #[test]
    fn a_services_frame_carries_the_whole_roster() {
        use crate::connector::ServiceStatus;
        use crate::machine_history::service_samples::{ServiceSample, ServiceSampleBatch};

        let event = HistoryEvent::Services(Box::new(ServiceSampleBatch {
            sampled_at_unix: 1_700_000_000,
            services: vec![
                ServiceSample {
                    id: "trusty-search".to_string(),
                    status: ServiceStatus::Running,
                    cpu_pct: Some(3.25),
                    rss_bytes: Some(148_897_792),
                },
                ServiceSample {
                    id: "trusty-review".to_string(),
                    status: ServiceStatus::Available,
                    cpu_pct: None,
                    rss_bytes: None,
                },
            ],
        }));
        assert_eq!(event.kind(), "services");

        let text = String::from_utf8(event.frame().to_vec()).expect("utf8 frame");
        let data = text
            .strip_prefix("event: services\ndata: ")
            .and_then(|r| r.strip_suffix("\n\n"))
            .expect("services frame shape");
        let parsed: serde_json::Value = serde_json::from_str(data).expect("services json");
        assert_eq!(parsed["sampled_at_unix"], 1_700_000_000_u64);
        assert_eq!(parsed["services"][0]["id"], "trusty-search");
        assert_eq!(parsed["services"][0]["status"], "running");
        assert_eq!(parsed["services"][0]["cpu_pct"], 3.25);
        // REGRESSION (#6773): one `services` event carries BOTH figures, so a
        // client never has to join two streams to draw the row's two graphs.
        assert_eq!(parsed["services"][0]["rss_bytes"], 148_897_792_u64);
        assert_eq!(parsed["services"][1]["id"], "trusty-review");
        assert!(
            parsed["services"][1]["cpu_pct"].is_null(),
            "an unmeasurable service is null, never 0.0"
        );
        assert!(
            parsed["services"][1]["rss_bytes"].is_null(),
            "an unmeasurable service is null, never 0"
        );
    }

    /// Why: a lag the viewer cannot see is a graph that lies about continuity.
    /// What: asserts the frame names `lagged` and carries the dropped count.
    /// Test: this test.
    #[test]
    fn a_lagged_frame_carries_the_count() {
        let text = String::from_utf8(lagged_frame(7).to_vec()).expect("utf8 frame");
        assert_eq!(text, "event: lagged\ndata: {\"dropped\":7}\n\n");
    }
}
