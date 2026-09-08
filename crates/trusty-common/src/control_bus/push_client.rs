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
//! What: A bounded, per-instance ring buffer (default capacity 4096, §4.2)
//!       guarded by a plain [`std::sync::Mutex`] — no channel, no
//!       process-global state, so this stays within the same types-and-a-
//!       client boundary [`super::tests::control_bus_declares_no_transport`]
//!       enforces on the rest of this module. [`PushClient::send`] enqueues
//!       and, on overflow, drops the oldest buffered frame and counts it
//!       (§4.2 "Overflow: the `dropped` count"). [`PushClient::flush`] dials
//!       console's ingest socket via [`crate::uds::send_framed_notification`]
//!       — the same one-way, no-reply framing every other fire-and-forget UDS
//!       write in this crate already uses — and drains the buffer oldest
//!       first, stopping and re-queuing the first frame that fails to send.
//! Test: `tests::push_client_buffers_when_the_socket_is_absent`,
//!       `tests::push_client_flushes_once_the_socket_appears`,
//!       `tests::push_client_drops_the_oldest_frame_beyond_capacity`.

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

use super::envelope::HarnessEvent;
use crate::uds::UdsRpcError;

/// Default number of buffered frames a [`PushClient`] holds before it starts
/// dropping the oldest (DOC-73 §4.2).
pub const DEFAULT_PUSH_BUFFER_CAPACITY: usize = 4096;

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
/// Test: the three `tests::push_client_*` cases in this file.
pub struct PushClient {
    socket_path: PathBuf,
    capacity: usize,
    timeout: Duration,
    buffer: Mutex<VecDeque<HarnessEvent>>,
    dropped: AtomicU64,
}

impl PushClient {
    /// A client dialing `socket_path`, with the default buffer capacity and
    /// per-frame timeout.
    ///
    /// Test: `tests::push_client_buffers_when_the_socket_is_absent`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self::with_capacity(socket_path, DEFAULT_PUSH_BUFFER_CAPACITY)
    }

    /// A client with an explicit buffer capacity, e.g. for a test that wants
    /// to force an overflow without sending thousands of frames.
    ///
    /// Test: `tests::push_client_drops_the_oldest_frame_beyond_capacity`.
    pub fn with_capacity(socket_path: impl Into<PathBuf>, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            socket_path: socket_path.into(),
            capacity,
            timeout: DEFAULT_PUSH_TIMEOUT,
            buffer: Mutex::new(VecDeque::with_capacity(capacity.min(64))),
            dropped: AtomicU64::new(0),
        }
    }

    /// Enqueue `event` for delivery. Never blocks on the network and never
    /// fails — the non-blocking invariant a producer relies on (DOC-73 §4.1).
    ///
    /// Why: A producer's tool call or workflow step must never stall because
    ///      console is slow, down, or absent. Keeping `send` pure buffer
    ///      manipulation — no dial, no write — is what makes that guarantee
    ///      hold unconditionally rather than "usually, unless the socket
    ///      hangs".
    /// What: Pushes `event` onto the back of the buffer. When the buffer is
    ///       already at `capacity`, pops the oldest frame first and counts it
    ///       in [`PushClient::dropped`] (§4.2 "Overflow: the `dropped`
    ///       count").
    /// Test: `tests::push_client_buffers_when_the_socket_is_absent`,
    ///       `tests::push_client_drops_the_oldest_frame_beyond_capacity`.
    pub fn send(&self, event: HarnessEvent) {
        let mut buffer = self.lock_buffer();
        if buffer.len() >= self.capacity {
            buffer.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        buffer.push_back(event);
    }

    /// Attempt to deliver every currently-buffered frame to console's ingest
    /// socket, oldest first.
    ///
    /// Why: The only method in this type that performs I/O, so a caller (a
    ///      background retry loop in the harness that embeds this client, or
    ///      a test) controls exactly when a dial happens rather than it being
    ///      an implicit side effect of `send`.
    /// What: Pops one frame at a time and sends it via
    ///       [`crate::uds::send_framed_notification`] — dial, write one
    ///       newline-terminated JSON frame, half-close, no reply expected,
    ///       matching §4.2's frame shape exactly. Stops and re-queues the
    ///       frame at the front of the buffer on the first failure, so a
    ///       transient dial failure loses nothing and the next `flush` call
    ///       resumes where this one stopped.
    ///
    /// # Errors
    ///
    /// Returns `Ok` (with `error: None`) even on a fully-successful drain of
    /// zero frames. `PushFlushOutcome.error` carries the [`UdsRpcError`] from
    /// the first frame that failed to send; `sent` still reports every frame
    /// that succeeded before it.
    /// Test: `tests::push_client_flushes_once_the_socket_appears`,
    ///       `tests::push_client_buffers_when_the_socket_is_absent`.
    pub async fn flush(&self) -> PushFlushOutcome {
        let mut sent = 0usize;
        loop {
            let Some(event) = self.lock_buffer().pop_front() else {
                return PushFlushOutcome { sent, error: None };
            };
            match crate::uds::send_framed_notification(&self.socket_path, &event, self.timeout)
                .await
            {
                Ok(()) => sent += 1,
                Err(error) => {
                    self.lock_buffer().push_front(event);
                    return PushFlushOutcome {
                        sent,
                        error: Some(error),
                    };
                }
            }
        }
    }

    /// Total frames dropped by [`PushClient::send`] overflow, across the
    /// lifetime of this client.
    ///
    /// Why: §4.2 — "a gap is always visible to the viewer, never silent". This
    ///      is the counter a caller surfaces alongside its own metrics.
    /// Test: `tests::push_client_drops_the_oldest_frame_beyond_capacity`.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Frames currently held in the buffer, awaiting delivery.
    ///
    /// Test: every `tests::push_client_*` case asserts this at some point.
    pub fn buffered_len(&self) -> usize {
        self.lock_buffer().len()
    }

    fn lock_buffer(&self) -> std::sync::MutexGuard<'_, VecDeque<HarnessEvent>> {
        self.buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;

    use super::*;
    use crate::control_bus::{ActionMeta, Actor, EventId, HarnessPayload, HarnessSource};

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
            let mut frames = Vec::new();
            for _ in 0..2 {
                let (mut conn, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                conn.read_to_end(&mut buf).await.expect("drain");
                frames.push(buf);
            }
            frames
        });

        let outcome = client.flush().await;
        assert_eq!(outcome.sent, 2, "both buffered frames were delivered");
        assert!(outcome.error.is_none());
        assert_eq!(client.buffered_len(), 0, "the buffer is drained");

        let frames = served.await.expect("join");
        assert_eq!(frames.len(), 2);
        for frame in frames {
            let text = String::from_utf8(frame).expect("utf8");
            assert!(
                text.ends_with('\n') && text.matches('\n').count() == 1,
                "one newline-terminated JSON frame per event: {text}"
            );
            assert!(
                text.contains("\"domain\":\"action\""),
                "the pushed frame carries the Action payload: {text}"
            );
        }
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
}
