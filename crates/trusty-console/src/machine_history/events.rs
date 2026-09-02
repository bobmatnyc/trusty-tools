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
}

impl HistoryEvent {
    /// The `event:` name this variant is delivered under.
    ///
    /// Test: `a_frame_names_its_kind_on_one_line`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            HistoryEvent::Sample(_) => "sample",
            HistoryEvent::Transition(_) => "transition",
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

    /// Why: a lag the viewer cannot see is a graph that lies about continuity.
    /// What: asserts the frame names `lagged` and carries the dropped count.
    /// Test: this test.
    #[test]
    fn a_lagged_frame_carries_the_count() {
        let text = String::from_utf8(lagged_frame(7).to_vec()).expect("utf8 frame");
        assert_eq!(text, "event: lagged\ndata: {\"dropped\":7}\n\n");
    }
}
