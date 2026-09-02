//! The byte stream behind `GET /api/console/machine-status/stream` (#6641).
//!
//! Why this is a module and not an inline closure in the route: the ordering
//! contract — the whole current window first, then every later event, with no
//! gap and no duplicate — is the part that can silently break, so it is built
//! where a test can drive it without an HTTP client or a running server.
//! What: [`event_stream`] subscribes to a [`MachineHistory`], emits its snapshot
//! as one `history` event, then one frame per broadcast event, with a `lagged`
//! frame carrying the dropped count whenever a slow subscriber falls behind the
//! channel, and a `: heartbeat` comment on an idle timer so an intermediary does
//! not close a quiet connection.
//! Test: `a_late_subscriber_gets_the_window_then_the_next_sample`,
//! `a_lagging_subscriber_is_told_what_it_missed`.

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use super::MachineHistory;
use super::events::{lagged_frame, sse_frame};

/// How often a silent stream emits an SSE comment.
///
/// Why: samples arrive every 5 s by default, so a healthy stream is never
/// silent. An operator who slows the cadence, or a proxy with a short idle
/// timeout, still needs traffic — the same reason the #6524 search relay
/// heartbeats.
/// What: `20` seconds, matching that relay's interval.
/// Test: covered structurally by `a_late_subscriber_gets_the_window_then_the_next_sample`,
/// which must not see a heartbeat before its live event.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// The SSE comment sent to keep an idle connection open.
const HEARTBEAT: &[u8] = b": heartbeat\n\n";

/// Build the SSE byte stream for one subscriber.
///
/// Why the snapshot and the subscription are taken together (see
/// [`MachineHistory::subscribe`]): a client that connects mid-window must get
/// every sample exactly once — no gap where a sample landed between the
/// snapshot and the subscription, and no duplicate where it landed in both.
/// What: yields one `history` frame built from the snapshot, then unfolds the
/// receiver: an event becomes its own frame, a `RecvError::Lagged(n)` becomes a
/// `lagged` frame carrying `n`, a closed channel ends the stream, and an idle
/// [`HEARTBEAT_INTERVAL`] emits a comment. The stream ends when the browser
/// disconnects and axum drops the body.
/// Test: `a_late_subscriber_gets_the_window_then_the_next_sample`,
/// `a_lagging_subscriber_is_told_what_it_missed`.
pub async fn event_stream(
    history: &MachineHistory,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
    let (snapshot, rx) = history.subscribe().await;
    let head = futures_util::stream::iter(std::iter::once(Ok(sse_frame("history", &snapshot))));

    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + HEARTBEAT_INTERVAL,
        HEARTBEAT_INTERVAL,
    );

    let tail = futures_util::stream::unfold(Some((rx, heartbeat)), |state| async move {
        let (mut rx, mut heartbeat) = state?;
        tokio::select! {
            biased;
            received = rx.recv() => match received {
                Ok(event) => Some((Ok(event.frame()), Some((rx, heartbeat)))),
                // #6641: never a silent gap — the viewer is told how many
                // samples its own slowness cost it.
                Err(RecvError::Lagged(dropped)) => {
                    debug!(dropped, "machine_history: subscriber lagged the event buffer");
                    Some((Ok(lagged_frame(dropped)), Some((rx, heartbeat))))
                }
                // Only reachable once the console's own `AppState` is gone,
                // i.e. at shutdown. Ending the stream is the honest answer.
                Err(RecvError::Closed) => None,
            },
            _ = heartbeat.tick() => Some((
                Ok(Bytes::from_static(HEARTBEAT)),
                Some((rx, heartbeat)),
            )),
        }
    });

    futures_util::StreamExt::chain(head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use trusty_common::host_metrics::HostSampler;

    /// Split one SSE frame into its event name and its JSON data.
    fn parse(frame: &Bytes) -> (String, serde_json::Value) {
        let text = String::from_utf8(frame.to_vec()).expect("utf8 frame");
        let (name, rest) = text
            .strip_prefix("event: ")
            .and_then(|r| r.split_once('\n'))
            .unwrap_or_else(|| panic!("frame has no event line: {text:?}"));
        let data = rest
            .strip_prefix("data: ")
            .and_then(|d| d.strip_suffix("\n\n"))
            .unwrap_or_else(|| panic!("frame has no data line: {text:?}"));
        (
            name.to_string(),
            serde_json::from_str(data).expect("frame data is json"),
        )
    }

    /// Why: this is the contract a browser reconnecting mid-window depends on —
    /// the samples it missed arrive in the first event, and the next live sample
    /// follows without being duplicated into the history.
    /// What: records three samples, opens a stream, records a fourth, and
    /// asserts frame 1 is a `history` event holding exactly the first three and
    /// frame 2 is a `sample` event for the fourth.
    /// Test: this test.
    #[tokio::test]
    async fn a_late_subscriber_gets_the_window_then_the_next_sample() {
        let history = MachineHistory::new();
        let mut sampler = HostSampler::new();
        for _ in 0..3 {
            history.record_sample(sampler.sample()).await;
        }

        let mut stream = Box::pin(event_stream(&history).await);

        // The fourth sample is recorded AFTER the subscription, so it must
        // arrive live rather than inside the history event.
        let fourth = sampler.sample();
        let fourth_cores = fourth.cpu.logical_cores;
        history.record_sample(fourth).await;

        let first = stream
            .next()
            .await
            .expect("history frame")
            .expect("infallible");
        let (name, data) = parse(&first);
        assert_eq!(name, "history");
        assert_eq!(
            data["samples"].as_array().expect("samples array").len(),
            3,
            "the connecting subscriber gets the three samples it missed"
        );
        assert_eq!(data["sample_capacity"], 120);
        assert_eq!(data["sample_interval_secs"], 5);

        let second = stream
            .next()
            .await
            .expect("live frame")
            .expect("infallible");
        let (name, data) = parse(&second);
        assert_eq!(name, "sample", "the fourth sample arrives live");
        assert_eq!(data["cpu"]["logical_cores"], fourth_cores);
    }

    /// Why: a subscriber that falls behind the broadcast buffer loses events. A
    /// graph with a silent hole in it is worse than one that says it has a hole.
    /// What: opens a stream against a two-event buffer, records five samples
    /// without reading, then reads: the history frame first, then a `lagged`
    /// frame naming a non-zero dropped count.
    /// Test: this test.
    #[tokio::test]
    async fn a_lagging_subscriber_is_told_what_it_missed() {
        let history = MachineHistory::with_limits(120, 16, 2, Duration::from_secs(60));
        let mut stream = Box::pin(event_stream(&history).await);

        let mut sampler = HostSampler::new();
        for _ in 0..5 {
            history.record_sample(sampler.sample()).await;
        }

        let first = stream
            .next()
            .await
            .expect("history frame")
            .expect("infallible");
        assert_eq!(parse(&first).0, "history");

        let second = stream
            .next()
            .await
            .expect("lagged frame")
            .expect("infallible");
        let (name, data) = parse(&second);
        assert_eq!(name, "lagged", "the gap is reported, not hidden");
        assert!(
            data["dropped"].as_u64().expect("dropped count") >= 1,
            "the dropped count is reported: {data}"
        );
    }
}
