//! `PushClient`: the producer-side buffered UDS transport to console's event
//! bus (DOC-73 §4.2, issue #6847).
//!
//! Why: Owner ruling 2026-09-05 makes trusty-console the one event bus in the
//!      workspace; every other harness is a producer that pushes to it. The
//!      non-blocking invariant (§4.1) — "[the bus] shouldn't block
//!      functionality, just messages and observability" — means a producer's
//!      `send` can never wait on console being reachable, so the buffering and
//!      the actual socket I/O have to be two different operations: enqueue is
//!      synchronous and infallible, and only an explicit [`PushClient::flush`]
//!      touches the network.
//! What: A bounded, per-instance ring buffer guarded by a plain
//!       [`std::sync::Mutex`] — no channel, no process-global state, so this
//!       stays within the same types-and-a-client boundary
//!       [`super::tests::control_bus_declares_no_transport`] enforces on the
//!       rest of this module. The buffer is bounded two ways at once (fix
//!       round, issue #6847 review): frame COUNT (default
//!       [`DEFAULT_PUSH_BUFFER_CAPACITY`], 4096) and cumulative serialized
//!       BYTE size (default [`DEFAULT_PUSH_BUFFER_BYTES`], 8 MiB) — either
//!       limit evicts the oldest buffered frame and counts it (§4.2
//!       "Overflow: the `dropped` count"). [`PushClient::flush`] dials
//!       console's ingest socket ONCE via [`crate::uds::connect_hardened`]
//!       and writes every buffered frame down that one connection with
//!       [`crate::uds::write_frame`] — the same newline-terminated JSON
//!       framing [`crate::uds::send_framed_notification`] uses for a single
//!       frame — before half-closing. Earlier revisions of this file dialled
//!       once PER frame; a drain of N buffered frames now costs one connect,
//!       not N. Stops and re-queues the first frame that fails to send.
//! Test: `tests::push_client_buffers_when_the_socket_is_absent`,
//!       `tests::push_client_flushes_once_the_socket_appears`,
//!       `tests::push_client_drops_the_oldest_frame_beyond_capacity`,
//!       `tests::flush_dials_exactly_once_for_the_whole_drain`,
//!       `tests::ten_thousand_sends_with_no_socket_complete_under_one_second`,
//!       `tests::thousand_sends_emit_no_tracing_lines`,
//!       `tests::oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap`.

// #6847: gated on `uds` (and `unix`, matching every other consumer of
// `crate::uds`) because it is the first thing in `control_bus` that actually
// moves an event over a socket. `control_bus`'s existing types (envelope,
// taxonomy, filter) stay unconditional; only this file, and its `mod
// push_client;` declaration in `mod.rs`, are feature-gated.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;

use super::envelope::HarnessEvent;
use crate::uds::UdsRpcError;

/// Default number of buffered frames a [`PushClient`] holds before it starts
/// dropping the oldest (DOC-73 §4.2).
pub const DEFAULT_PUSH_BUFFER_CAPACITY: usize = 4096;

/// Default byte budget for a [`PushClient`]'s buffer, tracked as the sum of
/// every currently-buffered frame's serialized size (security hardening,
/// issue #6847 fix round).
///
/// Why: [`DEFAULT_PUSH_BUFFER_CAPACITY`] alone bounds frame COUNT, not SIZE —
///      a handful of frames carrying large `ObjectRef.label` or
///      `PathRef.diff_ref` strings could balloon a producer's own memory well
///      past what 4096 small frames would cost, with nothing to stop it
///      short of the count cap. 8 MiB matches the
///      [`crate::uds::MAX_FRAME_BYTES`] class: far above any legitimate
///      control-plane frame, far below a real memory problem.
/// What: Enforced in [`PushClient::send`] against the buffer's cumulative
///       serialized size, evicting the oldest frame (and counting it as
///       dropped) until the total is back under budget — the same
///       oldest-first policy the count cap already uses.
/// Test: `tests::oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap`.
pub const DEFAULT_PUSH_BUFFER_BYTES: u64 = 8 * 1024 * 1024;

/// Default per-frame dial-and-write budget for [`PushClient::flush`].
///
/// Why: A [`flush`](PushClient::flush) call must not hang the caller when
///      console is unreachable — the same reasoning [`crate::uds::rpc`]'s own
///      timeouts document. 500ms is far above a local UDS round-trip and far
///      below anything a caller would notice as a stall.
pub const DEFAULT_PUSH_TIMEOUT: Duration = Duration::from_millis(500);

/// The result of one [`PushClient::flush`] call.
///
/// Why: A caller (a background flush loop, or a test) needs both how many
///      frames actually left the buffer and, when draining stopped early,
///      why — without `flush` needing to be infallible in a way that hides
///      partial progress.
/// What: `sent` is always accurate even when `error` is `Some`: it counts only
///       frames console's socket actually accepted. `error` is `None` when the
///       buffer was fully drained (including the empty-buffer case).
/// Test: `tests::push_client_flushes_once_the_socket_appears`,
///       `tests::push_client_buffers_when_the_socket_is_absent`.
#[derive(Debug)]
pub struct PushFlushOutcome {
    /// Frames console's ingest socket accepted this call.
    pub sent: usize,
    /// Set when draining stopped before the buffer was empty; the frame that
    /// failed is left at the front of the buffer for the next flush attempt.
    pub error: Option<UdsRpcError>,
}

/// One buffered frame plus its precomputed serialized byte length.
///
/// Why: [`PushClient::send`] must be able to evict against the byte budget
///      without re-serializing every buffered frame on every call — computing
///      the length once, at enqueue time, keeps eviction O(1) per frame.
/// What: `bytes` is `crate::uds::encode_frame(&event)`'s length (the exact
///       frame [`PushClient::flush`] later writes), or `0` if serialization
///       fails at enqueue time — extremely unlikely for [`HarnessEvent`], and
///       never a reason to drop the caller's `send`.
struct QueuedFrame {
    event: HarnessEvent,
    bytes: u64,
}

/// The byte-and-count-bounded ring buffer backing [`PushClient`].
///
/// Why: split out of [`PushClient`] so the two eviction policies (count,
///      bytes) and their bookkeeping (`total_bytes`) live in one place rather
///      than being recomputed at every call site that touches the buffer.
/// What: a plain [`VecDeque`] of [`QueuedFrame`] plus a running
///       `total_bytes`, kept in sync by `push_back`/`push_front`/`pop_front`
///       — the only three mutating operations this type exposes.
struct PushBuffer {
    frames: VecDeque<QueuedFrame>,
    total_bytes: u64,
}

impl PushBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.min(64)),
            total_bytes: 0,
        }
    }

    fn len(&self) -> usize {
        self.frames.len()
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn push_back(&mut self, frame: QueuedFrame) {
        self.total_bytes = self.total_bytes.saturating_add(frame.bytes);
        self.frames.push_back(frame);
    }

    fn push_front(&mut self, frame: QueuedFrame) {
        self.total_bytes = self.total_bytes.saturating_add(frame.bytes);
        self.frames.push_front(frame);
    }

    fn pop_front(&mut self) -> Option<QueuedFrame> {
        let frame = self.frames.pop_front()?;
        self.total_bytes = self.total_bytes.saturating_sub(frame.bytes);
        Some(frame)
    }
}

/// Serialized length of `event`'s wire frame, or `0` on a (practically
/// unreachable) serialization failure — see [`QueuedFrame`].
fn frame_byte_len(event: &HarnessEvent) -> u64 {
    crate::uds::encode_frame(event)
        .map(|frame| frame.len() as u64)
        .unwrap_or(0)
}

/// Buffered, non-blocking producer-side client for console's event-bus
/// ingest socket (DOC-73 §4.2).
///
/// Why: See module docs. One instance is meant to be shared (behind an `Arc`,
///      by whichever harness embeds it) across every call site that emits a
///      [`HarnessEvent`], the same way `trusty-agents-common`'s in-process bus
///      was shared before #6854 retires it.
/// What: `send` is synchronous and always succeeds from the caller's point of
///       view — the non-blocking invariant §4.1 states. `flush` is the only
///       method that performs I/O.
/// Test: the `tests::push_client_*` and `tests::flush_*` cases in this file.
pub struct PushClient {
    socket_path: PathBuf,
    capacity: usize,
    max_bytes: u64,
    timeout: Duration,
    buffer: Mutex<PushBuffer>,
    dropped: AtomicU64,
}

impl PushClient {
    /// A client dialing `socket_path`, with the default buffer capacity,
    /// byte budget, and per-frame timeout.
    ///
    /// Test: `tests::push_client_buffers_when_the_socket_is_absent`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self::with_limits(
            socket_path,
            DEFAULT_PUSH_BUFFER_CAPACITY,
            DEFAULT_PUSH_BUFFER_BYTES,
        )
    }

    /// A client with an explicit buffer capacity (default byte budget), e.g.
    /// for a test that wants to force a count-cap overflow without sending
    /// thousands of frames.
    ///
    /// Test: `tests::push_client_drops_the_oldest_frame_beyond_capacity`.
    pub fn with_capacity(socket_path: impl Into<PathBuf>, capacity: usize) -> Self {
        Self::with_limits(socket_path, capacity, DEFAULT_PUSH_BUFFER_BYTES)
    }

    /// A client with an explicit frame-count capacity AND byte budget, e.g.
    /// for a test that wants to force a byte-budget eviction independently of
    /// the count cap.
    ///
    /// What: both `capacity` and `max_bytes` are floored at `1` — a
    /// zero-sized buffer cannot hold even one frame, and the eviction loop in
    /// [`PushClient::send`] assumes at least one frame may remain.
    /// Test: `tests::oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap`.
    pub fn with_limits(socket_path: impl Into<PathBuf>, capacity: usize, max_bytes: u64) -> Self {
        let capacity = capacity.max(1);
        Self {
            socket_path: socket_path.into(),
            capacity,
            max_bytes: max_bytes.max(1),
            timeout: DEFAULT_PUSH_TIMEOUT,
            buffer: Mutex::new(PushBuffer::new(capacity)),
            dropped: AtomicU64::new(0),
        }
    }

    /// Enqueue `event` for delivery. Never blocks on the network and never
    /// fails — the non-blocking invariant a producer relies on (DOC-73 §4.1).
    ///
    /// Why: A producer's tool call or workflow step must never stall because
    ///      console is slow, down, or absent. Keeping `send` pure buffer
    ///      manipulation — no dial, no write, no `tracing` call on the hot
    ///      path — is what makes that guarantee hold unconditionally rather
    ///      than "usually, unless the socket hangs or the buffer is full".
    /// What: Pushes `event` onto the back of the buffer, then evicts the
    ///       oldest frame — counting it in [`PushClient::dropped`] (§4.2
    ///       "Overflow: the `dropped` count") — while the buffer is over
    ///       EITHER bound: more than `capacity` frames, or more than
    ///       `max_bytes` of cumulative serialized size with more than one
    ///       frame still buffered. The `> 1` guard on the byte check means a
    ///       single frame larger than `max_bytes` is still buffered (evicting
    ///       it would just replace one oversized frame with an empty buffer)
    ///       rather than silently discarded.
    /// Test: `tests::push_client_buffers_when_the_socket_is_absent`,
    ///       `tests::push_client_drops_the_oldest_frame_beyond_capacity`,
    ///       `tests::oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap`,
    ///       `tests::ten_thousand_sends_with_no_socket_complete_under_one_second`,
    ///       `tests::thousand_sends_emit_no_tracing_lines`.
    pub fn send(&self, event: HarnessEvent) {
        let bytes = frame_byte_len(&event);
        let mut buffer = self.lock_buffer();
        buffer.push_back(QueuedFrame { event, bytes });
        while buffer.len() > self.capacity
            || (buffer.total_bytes > self.max_bytes && buffer.len() > 1)
        {
            if buffer.pop_front().is_none() {
                break;
            }
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Attempt to deliver every currently-buffered frame to console's ingest
    /// socket, oldest first, over ONE connection.
    ///
    /// Why: The only method in this type that performs I/O, so a caller (a
    ///      background retry loop in the harness that embeds this client, or
    ///      a test) controls exactly when a dial happens rather than it being
    ///      an implicit side effect of `send`. Dialling once for the whole
    ///      drain — rather than once per frame, as an earlier revision did —
    ///      means flushing N buffered frames costs one connection setup, not
    ///      N (issue #6847 fix round).
    /// What: Returns immediately with an empty, `error: None` outcome when
    ///       the buffer is empty, without dialling. Otherwise dials
    ///       [`crate::uds::connect_hardened`] once, then pops and writes one
    ///       frame at a time via [`crate::uds::write_frame`] — a
    ///       newline-terminated JSON frame per event, matching §4.2's frame
    ///       shape exactly and the wire format `send_framed_notification`
    ///       already used per-frame. Half-closes the connection once every
    ///       buffered frame has been written. Stops and re-queues the frame
    ///       at the front of the buffer on the first failure (dial, write, or
    ///       per-operation timeout), so a transient failure loses nothing and
    ///       the next `flush` call resumes where this one stopped.
    ///
    /// # Errors
    ///
    /// Returns `Ok` (with `error: None`) even on a fully-successful drain of
    /// zero frames. `PushFlushOutcome.error` carries the [`UdsRpcError`] from
    /// the dial or from the first frame that failed to send; `sent` still
    /// reports every frame that succeeded before it.
    /// Test: `tests::push_client_flushes_once_the_socket_appears`,
    ///       `tests::push_client_buffers_when_the_socket_is_absent`,
    ///       `tests::flush_dials_exactly_once_for_the_whole_drain`.
    pub async fn flush(&self) -> PushFlushOutcome {
        if self.lock_buffer().is_empty() {
            return PushFlushOutcome {
                sent: 0,
                error: None,
            };
        }

        let mut stream = match tokio::time::timeout(
            self.timeout,
            crate::uds::connect_hardened(&self.socket_path),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(source)) => {
                return PushFlushOutcome {
                    sent: 0,
                    error: Some(UdsRpcError::Dial {
                        path: self.socket_path.clone(),
                        source,
                    }),
                };
            }
            Err(_) => {
                return PushFlushOutcome {
                    sent: 0,
                    error: Some(UdsRpcError::Timeout {
                        path: self.socket_path.clone(),
                        timeout: self.timeout,
                    }),
                };
            }
        };

        let mut sent = 0usize;
        loop {
            let Some(frame) = self.lock_buffer().pop_front() else {
                break;
            };
            match tokio::time::timeout(
                self.timeout,
                crate::uds::write_frame(&mut stream, &frame.event),
            )
            .await
            {
                Ok(Ok(())) => sent += 1,
                Ok(Err(source)) => {
                    self.lock_buffer().push_front(frame);
                    return PushFlushOutcome {
                        sent,
                        error: Some(UdsRpcError::Write {
                            path: self.socket_path.clone(),
                            source,
                        }),
                    };
                }
                Err(_) => {
                    self.lock_buffer().push_front(frame);
                    return PushFlushOutcome {
                        sent,
                        error: Some(UdsRpcError::Timeout {
                            path: self.socket_path.clone(),
                            timeout: self.timeout,
                        }),
                    };
                }
            }
        }

        // Half-close: lets a peer reading to EOF know the drain is complete.
        // Best-effort — the peer may already have gone away, which is not a
        // reason to report an otherwise-successful flush as failed.
        let _ = stream.shutdown().await;

        PushFlushOutcome { sent, error: None }
    }

    /// Total frames dropped by [`PushClient::send`] overflow (count OR byte
    /// budget), across the lifetime of this client.
    ///
    /// Why: §4.2 — "a gap is always visible to the viewer, never silent". This
    ///      is the counter a caller surfaces alongside its own metrics.
    /// Test: `tests::push_client_drops_the_oldest_frame_beyond_capacity`,
    ///       `tests::oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap`.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Frames currently held in the buffer, awaiting delivery.
    ///
    /// Test: every `tests::push_client_*` case asserts this at some point.
    pub fn buffered_len(&self) -> usize {
        self.lock_buffer().len()
    }

    fn lock_buffer(&self) -> std::sync::MutexGuard<'_, PushBuffer> {
        self.buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::control_bus::{ActionMeta, Actor, EventId, HarnessPayload, HarnessSource};
    use crate::log_buffer::{LogBuffer, LogBufferLayer};

    fn sample_event() -> HarnessEvent {
        HarnessEvent {
            source: HarnessSource::Mpm,
            session: Some("s1".into()),
            seq: 0,
            at: chrono::Utc::now(),
            payload: HarnessPayload::Action(crate::control_bus::ActionEvent::Session {
                meta: ActionMeta {
                    id: EventId::new(),
                    at: chrono::Utc::now(),
                    source: HarnessSource::Mpm,
                    session: Some("s1".into()),
                    parent_id: None,
                    actor: Actor::System,
                    objects: Vec::new(),
                    schema_version: 1,
                },
                phase: crate::control_bus::SessionPhase::Started,
            }),
            id: EventId::new(),
            parent_id: None,
        }
    }

    /// A `send` while nothing is listening buffers the frame rather than
    /// erroring — the non-blocking invariant (DOC-73 §4.1).
    #[tokio::test]
    async fn push_client_buffers_when_the_socket_is_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("absent.sock");
        let client = PushClient::new(&sock);

        client.send(sample_event());
        assert_eq!(client.buffered_len(), 1, "the frame stays buffered");

        let outcome = client.flush().await;
        assert_eq!(outcome.sent, 0, "nothing could be delivered");
        assert!(outcome.error.is_some(), "flush reports why it stopped");
        assert_eq!(
            client.buffered_len(),
            1,
            "a failed flush must not lose the frame"
        );
        assert_eq!(
            client.dropped(),
            0,
            "buffering under capacity drops nothing"
        );
    }

    /// Frames buffered while console is absent are delivered once a listener
    /// appears and `flush` is called again (DOC-73 §4.2 "Reconnect and
    /// replay").
    #[tokio::test]
    async fn push_client_flushes_once_the_socket_appears() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("console.sock");
        let client = PushClient::new(&sock);

        client.send(sample_event());
        client.send(sample_event());
        assert_eq!(client.buffered_len(), 2);

        let listener: UnixListener = crate::uds::bind_hardened(&sock).expect("bind");
        let served = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            conn.read_to_end(&mut buf).await.expect("drain");
            buf
        });

        let outcome = client.flush().await;
        assert_eq!(outcome.sent, 2, "both buffered frames were delivered");
        assert!(outcome.error.is_none());
        assert_eq!(client.buffered_len(), 0, "the buffer is drained");

        let buf = served.await.expect("join");
        let text = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "one newline-terminated JSON frame per event, over the same connection: {text}"
        );
        for line in lines {
            assert!(
                line.contains("\"domain\":\"action\""),
                "the pushed frame carries the Action payload: {line}"
            );
        }
    }

    /// `flush` dials console's ingest socket exactly once for the whole
    /// drain, no matter how many frames are buffered — the HIGH-severity fix
    /// from the #6847 review round: an earlier revision dialled once PER
    /// frame.
    #[tokio::test]
    async fn flush_dials_exactly_once_for_the_whole_drain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("console.sock");
        let client = PushClient::new(&sock);

        const FRAME_COUNT: usize = 100;
        for _ in 0..FRAME_COUNT {
            client.send(sample_event());
        }
        assert_eq!(client.buffered_len(), FRAME_COUNT);

        let listener: UnixListener = crate::uds::bind_hardened(&sock).expect("bind");
        let connections = Arc::new(AtomicUsize::new(0));
        let accept_connections = connections.clone();
        let served = tokio::spawn(async move {
            let mut lines = Vec::new();
            // A fake acceptor: keep accepting until nothing new shows up for
            // a while, counting every accepted connection along the way.
            while let Ok(Ok((mut conn, _))) =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
            {
                accept_connections.fetch_add(1, Ordering::SeqCst);
                let mut buf = Vec::new();
                conn.read_to_end(&mut buf).await.expect("drain");
                let text = String::from_utf8(buf).expect("utf8");
                lines.extend(text.lines().map(str::to_string));
            }
            lines
        });

        let outcome = client.flush().await;
        assert_eq!(
            outcome.sent, FRAME_COUNT,
            "every buffered frame was delivered"
        );
        assert!(outcome.error.is_none());

        let lines = served.await.expect("join");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            1,
            "flush must dial exactly one connection for the whole drain"
        );
        assert_eq!(
            lines.len(),
            FRAME_COUNT,
            "one line per buffered frame, all over that one connection"
        );
    }

    /// Sending past capacity drops the oldest buffered frame and counts it,
    /// never silently (DOC-73 §4.2 "Overflow: the `dropped` count").
    #[tokio::test]
    async fn push_client_drops_the_oldest_frame_beyond_capacity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("absent.sock");
        let client = PushClient::with_capacity(&sock, 2);

        for _ in 0..5 {
            client.send(sample_event());
        }

        assert_eq!(client.buffered_len(), 2, "never grows past capacity");
        assert_eq!(client.dropped(), 3, "the three oldest overflow frames");
    }

    /// A byte budget evicts oldest-first even when the frame COUNT cap alone
    /// would never trigger — the security hardening added in the #6847 fix
    /// round.
    #[tokio::test]
    async fn oversized_events_evict_earlier_frames_by_bytes_before_the_count_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("absent.sock");

        let one_frame_bytes = frame_byte_len(&sample_event());
        // Budget room for a little over two frames; a frame-count cap of
        // 1000 would never evict anything on its own for the five frames
        // this test sends.
        let byte_budget = one_frame_bytes.saturating_mul(2) + one_frame_bytes / 2;
        let client = PushClient::with_limits(&sock, 1000, byte_budget);

        for _ in 0..5 {
            client.send(sample_event());
        }

        assert!(
            client.buffered_len() < 5,
            "the byte budget must evict frames the 1000-frame count cap alone would not: \
             buffered_len={}",
            client.buffered_len()
        );
        assert!(
            client.dropped() > 0,
            "byte-budget evictions must be counted as drops, same as count-cap evictions"
        );
    }

    /// Acceptance criterion (issue #6847): `send` never touches the network,
    /// so 10,000 calls with no socket present complete in well under a
    /// second.
    #[test]
    fn ten_thousand_sends_with_no_socket_complete_under_one_second() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("never-there.sock");
        let client = PushClient::new(&sock);

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            client.send(sample_event());
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "10,000 no-socket sends took {elapsed:?}, expected under 1s"
        );
    }

    /// Acceptance criterion (issue #6847): `send` in no-socket mode is pure
    /// buffer manipulation — it must not emit a single `tracing` line, which
    /// would turn a hot enqueue path into log-volume pressure.
    #[test]
    fn thousand_sends_emit_no_tracing_lines() {
        let buffer = LogBuffer::new(16);
        let subscriber = tracing_subscriber::registry().with(LogBufferLayer::new(buffer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let tmp = tempfile::tempdir().expect("tempdir");
            let sock = tmp.path().join("silent.sock");
            let client = PushClient::new(&sock);
            for _ in 0..1_000 {
                client.send(sample_event());
            }
        });

        let lines = buffer.tail(16);
        assert!(
            lines.is_empty(),
            "send() must not emit any tracing lines, got: {lines:?}"
        );
    }
}
