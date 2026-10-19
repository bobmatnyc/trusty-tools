//! The ingest half of the bus socket: the frames producers push (issue #6848,
//! DOC-73 §4.2, #9075).
//!
//! Why: DOC-73 §4.2 makes the bus "the only ingester" — every harness pushes
//! `HarnessEvent` frames over one socket rather than the bus polling each of
//! them. A producer's `control_bus::PushClient` dials, writes its buffered
//! frames as newline-terminated JSON, and half-closes; nothing here assumes a
//! connection carries only one frame, so a later `PushClient` that holds the
//! connection open and streams needs no change here.
//! What: [`ingest_lines`] reads one connection's frames, starting with the
//! first line [`crate::serve`] already read to decide this connection is a
//! producer (#9075: one socket carries ingest and `events.subscribe`). A line
//! that is not valid `HarnessEvent` JSON logs a warning and reading goes on —
//! one malformed frame must not cost every frame after it on the same
//! connection, still less every other producer's connection. A line over
//! [`MAX_LINE_BYTES`] ends that connection only, and the allocation for that
//! line is capped at read time (a fresh [`tokio::io::AsyncReadExt::take`]
//! budget per line) rather than checked only after an unbounded `read_until`
//! returns. A connection that goes [`READ_IDLE_TIMEOUT`] without producing a
//! full line is dropped the same way. A bus that has closed for shutdown
//! refuses the frame, which ends the connection with a warning.
//! Test: `super::tests` — `ingest_over_uds_socket_reaches_the_bus`,
//! `malformed_line_does_not_kill_the_listener`,
//! `oversized_line_ends_only_its_own_connection`,
//! `unterminated_line_never_grows_past_the_line_cap`,
//! `idle_connection_is_dropped_after_the_read_timeout`.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::UnixStream;
use trusty_common::control_bus::HarnessEvent;

use super::bus::{EventBus, IngestOutcome};

/// Largest single newline-delimited frame this listener accepts.
///
/// Why not [`trusty_common::uds::MAX_FRAME_BYTES`] (8 MiB, sized for a request
/// on the request/response RPC transport that module also defines): a
/// `HarnessEvent` is a small, flat envelope around one domain-tagged payload —
/// the largest arm is `HarnessPayload::Hook`'s open-ended `serde_json::Value`,
/// still nowhere near an RPC response carrying whole search results. 1 MiB is
/// generous headroom over any hook payload observed in the workspace today
/// while still bounding a misbehaving or malicious producer's memory cost per
/// connection to a fixed budget rather than the connection's lifetime.
/// Test: `super::tests::oversized_line_ends_only_its_own_connection`,
/// `super::tests::unterminated_line_never_grows_past_the_line_cap`.
pub(crate) const MAX_LINE_BYTES: usize = 1024 * 1024;

/// How long one connection may go without completing a line before this
/// listener gives up on it.
///
/// Why 60 s: a `PushClient` that holds a connection open and streams (the
/// intended shape once §4.2's revision lands) sends far more often than this
/// under any real workload; a producer that goes a full minute mid-frame is
/// indistinguishable from a hung peer holding a slot another producer could
/// use. Mirrors the idle-timeout shape `webhook_relay::serve::serve_until`
/// applies per connection, scaled up because that listener serves one
/// request/response pair per connection while this one serves a long-lived
/// stream.
/// Test: `super::tests::idle_connection_is_dropped_after_the_read_timeout`.
pub(crate) const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What one bounded line read produced.
#[derive(Debug)]
pub(crate) enum LineRead {
    /// A line (with its newline, unless it is the last bytes before EOF).
    Line,
    /// The peer closed with nothing pending.
    Eof,
    /// The line reached [`MAX_LINE_BYTES`] without a newline.
    TooLong,
    /// No complete line within the idle timeout.
    Idle,
    /// The read itself failed.
    Failed(std::io::Error),
}

impl LineRead {
    /// Log why a connection ends on this read. `Eof` is a clean end — a
    /// finished flush or a connect-and-close liveness probe — and logs nothing.
    pub(crate) fn log_end(&self, line_len: usize, idle_timeout: Duration) {
        match self {
            Self::Line | Self::Eof => {}
            Self::TooLong => tracing::warn!(
                bytes = line_len,
                max = MAX_LINE_BYTES,
                "event-bus: line exceeded the max frame size; dropping connection"
            ),
            Self::Idle => tracing::warn!(
                timeout = ?idle_timeout,
                "event-bus: connection idle too long; dropping"
            ),
            Self::Failed(e) => tracing::warn!(error = %e, "event-bus: connection read error"),
        }
    }
}

/// Read the next line into `line` (cleared first), capped and timed out.
///
/// Why: the first-line sniff in [`crate::serve`] and the ingest loop below
/// must apply the same cap and the same timeout, or a peer could pick the
/// laxer of the two.
/// Test: `super::tests::oversized_line_ends_only_its_own_connection`,
/// `super::tests::idle_connection_is_dropped_after_the_read_timeout`.
pub(crate) async fn read_line_bounded(
    reader: &mut BufReader<UnixStream>,
    line: &mut Vec<u8>,
    idle_timeout: Duration,
) -> LineRead {
    line.clear();
    match tokio::time::timeout(idle_timeout, read_capped_line(reader, line)).await {
        Err(_) => LineRead::Idle,
        Ok(Err(e)) => LineRead::Failed(e),
        Ok(Ok(0)) if line.is_empty() => LineRead::Eof,
        Ok(Ok(_)) if !line.ends_with(b"\n") && line.len() >= MAX_LINE_BYTES => LineRead::TooLong,
        Ok(Ok(_)) => LineRead::Line,
    }
}

/// Ingest `first` and every later line on `reader` until EOF, a read error,
/// an oversized line, the idle timeout, or a bus closed for shutdown.
///
/// Why/What: see the module docs.
/// Test: `super::tests::ingest_over_uds_socket_reaches_the_bus`,
/// `super::tests::malformed_line_does_not_kill_the_listener`.
pub(crate) async fn ingest_lines(
    reader: &mut BufReader<UnixStream>,
    first: Vec<u8>,
    bus: &EventBus,
    idle_timeout: Duration,
) {
    let mut line = first;
    loop {
        if !ingest_line(&line, bus) {
            return;
        }
        match read_line_bounded(reader, &mut line, idle_timeout).await {
            LineRead::Line => {}
            end => {
                end.log_end(line.len(), idle_timeout);
                return;
            }
        }
    }
}

/// Ingest one line. `false` when the connection should end.
fn ingest_line(line: &[u8], bus: &EventBus) -> bool {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    match serde_json::from_str::<HarnessEvent>(trimmed) {
        Ok(event) => match bus.ingest(event) {
            IngestOutcome::Ingested | IngestOutcome::Deduped => true,
            // #9075: shutdown closed the bus; a frame accepted now would get a
            // seq the log never records and the next start would reuse.
            IngestOutcome::Closed => {
                tracing::warn!("event-bus ingest: bus is shutting down; frame refused");
                false
            }
        },
        Err(e) => {
            // Deliberately does not end the connection: one malformed frame
            // from a producer must not cost every frame that follows it on the
            // same long-lived connection.
            tracing::warn!(
                error = %e,
                "event-bus ingest: malformed HarnessEvent line, dropped"
            );
            true
        }
    }
}

/// Read up to and including the next newline, with the allocation for THIS
/// line capped at [`MAX_LINE_BYTES`] before any byte of it is read.
///
/// Why: the prior shape read with a plain `BufReader::read_until` and checked
/// `line.len() > MAX_LINE_BYTES` only after it returned — an unterminated line
/// from a peer that never sends `\n` grows `line` without limit in the
/// meantime, because `read_until` has no bound of its own. Re-taking a fresh
/// `.take(MAX_LINE_BYTES)` budget on `&mut reader` for every line — the same
/// pattern `trusty_common::uds::stream_client::UdsStreamClient::read_line` and
/// `uds::rpc::read_one_frame` use — caps the allocation up front instead.
/// `take` borrows the reader rather than consuming it, so bytes already
/// buffered past the cap for an oversized line stay in `reader` and are
/// visible to the caller's own oversized-line check on `line.len()`.
///
/// Test: `super::tests::unterminated_line_never_grows_past_the_line_cap`.
pub(crate) async fn read_capped_line(
    reader: &mut BufReader<UnixStream>,
    line: &mut Vec<u8>,
) -> std::io::Result<usize> {
    let mut bounded = (&mut *reader).take(MAX_LINE_BYTES as u64);
    bounded.read_until(b'\n', line).await
}
