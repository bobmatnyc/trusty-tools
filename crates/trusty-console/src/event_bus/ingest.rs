//! The UDS ingest listener producers dial (issue #6848, DOC-73 §4.2).
//!
//! Why: DOC-73 §4.2 makes console "the only ingester" — every harness pushes
//! `HarnessEvent` frames over one socket rather than console polling each of
//! them. This module is that socket's server half: bind it hardened (the same
//! `0700` directory / `0600` socket / peer-uid convention every other socket
//! in the workspace uses), accept connections, and read each one as
//! newline-delimited `HarnessEvent` JSON for as long as the producer keeps the
//! connection open — a producer's `control_bus::PushClient` (§4.2, a parallel
//! slice) is expected to hold one long-lived connection and stream frames on
//! it rather than reconnect per event. The wire format is exactly the
//! newline-terminated JSON [`trusty_common::uds::send_framed_notification`]
//! writes — `PushClient::flush` (issue #6847) dials, sends one frame, and
//! half-closes per call today, but nothing here assumes a connection carries
//! only one frame, so a later `PushClient` revision that holds the connection
//! open and streams needs no ingest-side change.
//! What: [`ingest_socket_path`] resolves the socket
//! (`daemon_socket_path("trusty-console")`, the same cross-crate convention
//! [`trusty_common::daemon_socket_path`] documents); [`bind_ingest`] binds it
//! through [`trusty_common::uds::bind_singleton_hardened`], which reclaims a
//! stale socket file left by an unclean shutdown rather than refusing to bind
//! forever; [`serve_ingest`] accepts until shutdown, bounding concurrent
//! connections at [`MAX_CONCURRENT_CONNECTIONS`] and spawning one task per
//! connection. A connection that sends a line that is not valid `HarnessEvent`
//! JSON logs a warning and keeps reading — one malformed frame must not cost
//! every frame after it on the same connection, still less every other
//! producer's connection. A line over [`MAX_LINE_BYTES`] ends that connection
//! only, and the allocation for that line is capped at read time (a fresh
//! [`tokio::io::AsyncReadExt::take`] budget per line) rather than checked only
//! after an unbounded `read_until` returns — an unterminated line otherwise
//! grows the buffer without limit before the size check ever runs. A
//! connection that goes [`READ_IDLE_TIMEOUT`] without producing a full line is
//! dropped the same way.
//! Test: `super::tests` — `ingest_over_uds_socket_reaches_the_bus`,
//! `malformed_line_does_not_kill_the_listener`,
//! `oversized_line_ends_only_its_own_connection`,
//! `unterminated_line_never_grows_past_the_line_cap`,
//! `stale_socket_file_is_reclaimed_on_bind`,
//! `idle_connection_is_dropped_after_the_read_timeout`,
//! `connections_beyond_the_limit_wait_for_a_free_slot`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use trusty_common::control_bus::HarnessEvent;
use trusty_common::uds::{
    UdsSecurityError, accept_sized, bind_singleton_hardened, ensure_peer_is_self,
};

use super::bus::EventBus;

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
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest number of producer connections this listener serves at once.
///
/// Why a cap at all: an unbounded accept loop hands every accepted socket its
/// own task and `BufReader`, so a burst of connections (malicious or merely a
/// thundering herd of restarted harnesses) costs memory proportional to
/// connection count with no ceiling. 256 is comfortably above the number of
/// harnesses this workspace runs per host today (`trusty-mpm`, `trusty-code`,
/// `trusty-agents`, `trusty-analyze` — DOC-73 §4.1) with headroom for several
/// concurrent reconnect storms, while still bounding worst-case memory to a
/// fixed multiple of [`MAX_LINE_BYTES`] rather than the connection count the
/// kernel's accept queue happens to deliver.
/// Test: `super::tests::connections_beyond_the_limit_wait_for_a_free_slot`.
const MAX_CONCURRENT_CONNECTIONS: usize = 256;

/// Resolve the console event-bus ingest socket path (DOC-73 §4.2).
///
/// # Errors
///
/// Whatever [`trusty_common::daemon_socket_path`] returns — the console data
/// directory could not be resolved or created.
pub(crate) fn ingest_socket_path() -> anyhow::Result<PathBuf> {
    trusty_common::daemon_socket_path("trusty-console")
}

/// Why the ingest socket could not be bound.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IngestError {
    /// The socket could not be bound, or someone else already serves it.
    #[error("bind the event-bus ingest socket at {path}: {source}")]
    Bind {
        /// Socket that could not be bound.
        path: PathBuf,
        /// Why the bind failed.
        #[source]
        source: UdsSecurityError,
    },
}

/// Bind the ingest socket, reclaiming a stale socket file per
/// [`trusty_common::uds::bind_singleton_hardened`].
///
/// Why `bind_singleton_hardened` rather than `bind_hardened`: console is
/// supervised and restarted like every other daemon in this workspace, and
/// `bind_hardened` refuses an occupied path outright — a predecessor that
/// exits uncleanly (a SIGKILL, a crash) never unlinks its socket, so every
/// bind after that would fail forever with no operator-visible cause. This
/// mirrors `trusty-memory`, `trusty-analyze` and `trusty-review`'s own
/// listener binds, all of which made the same switch for the same reason.
///
/// # Errors
///
/// [`IngestError::Bind`] when the path cannot be bound — including when the
/// probe proves another process is still serving it, in which case this
/// process correctly does not take over.
///
/// Test: `super::tests::ingest_over_uds_socket_reaches_the_bus`,
/// `super::tests::stale_socket_file_is_reclaimed_on_bind`.
pub(crate) async fn bind_ingest(socket: &Path) -> Result<UnixListener, IngestError> {
    bind_singleton_hardened(socket)
        .await
        .map_err(|source| IngestError::Bind {
            path: socket.to_path_buf(),
            source,
        })
}

/// Accept and serve ingest connections until `shutdown` resolves.
///
/// Each connection is handed to its own `tokio::spawn` rather than served
/// inline, matching the accept-loop shape every other in-process UDS listener
/// in this workspace uses (`webhook_relay::serve_until`): one slow or stalled
/// producer must never delay another producer's connection from being
/// accepted. Concurrency is bounded by [`MAX_CONCURRENT_CONNECTIONS`]: a
/// [`tokio::sync::Semaphore`] permit is acquired before a connection is
/// spawned, so once the limit is reached, further accepted connections wait
/// for a slot rather than piling up an unbounded number of tasks.
///
/// Test: `super::tests::ingest_over_uds_socket_reaches_the_bus`,
/// `super::tests::malformed_line_does_not_kill_the_listener`,
/// `super::tests::oversized_line_ends_only_its_own_connection`,
/// `super::tests::connections_beyond_the_limit_wait_for_a_free_slot`.
pub(crate) async fn serve_ingest(
    listener: UnixListener,
    bus: Arc<EventBus>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    serve_ingest_with_limit(listener, bus, MAX_CONCURRENT_CONNECTIONS, shutdown).await;
}

/// [`serve_ingest`] with the concurrency cap taken explicitly rather than
/// read from [`MAX_CONCURRENT_CONNECTIONS`], so a test can prove the gating
/// behavior against a limit of 1 instead of opening 257 real connections.
///
/// Test: `super::tests::connections_beyond_the_limit_wait_for_a_free_slot`.
pub(crate) async fn serve_ingest_with_limit(
    listener: UnixListener,
    bus: Arc<EventBus>,
    max_connections: usize,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let semaphore = Arc::new(Semaphore::new(max_connections));
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            biased;
            () = &mut shutdown => return,
            accepted = accept_sized(&listener) => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "event-bus ingest accept failed");
                continue;
            }
        };
        // #6848 fix round: acquiring the permit here, before `spawn`, is what
        // makes the bound real — a permit acquired inside the spawned task
        // would let an unbounded number of tasks queue up waiting for one,
        // which defeats the point of capping concurrency. The accept loop
        // itself stalls once the limit is reached, so a burst of connections
        // beyond the cap waits for a slot rather than each getting its own
        // task and `BufReader` immediately. `expect` is safe: the semaphore
        // is never `close`d, so `acquire_owned` only errors on a closed one.
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .expect("event-bus ingest semaphore is never closed");
        let bus = Arc::clone(&bus);
        tokio::spawn(async move {
            handle_connection(stream, bus).await;
            drop(permit);
        });
    }
}

/// Serve one accepted connection: verify the peer, then read newline-delimited
/// `HarnessEvent` JSON until EOF, a read error, or [`READ_IDLE_TIMEOUT`]
/// elapses with no line completed.
///
/// Why a peer check at all: the same ADR-0034 §3 trust boundary every other
/// hardened socket in this crate enforces — the `0600` mode is a documented
/// intention until something actually reads the peer's uid off the accepted
/// connection.
async fn handle_connection(stream: UnixStream, bus: Arc<EventBus>) {
    handle_connection_with_timeout(stream, bus, READ_IDLE_TIMEOUT).await;
}

/// [`handle_connection`] with the idle timeout taken explicitly rather than
/// read from [`READ_IDLE_TIMEOUT`], so a test can prove the drop behavior in
/// milliseconds instead of the real 60 s.
///
/// Test: `super::tests::idle_connection_is_dropped_after_the_read_timeout`.
pub(crate) async fn handle_connection_with_timeout(
    stream: UnixStream,
    bus: Arc<EventBus>,
    idle_timeout: Duration,
) {
    if let Err(e) = ensure_peer_is_self(&stream) {
        tracing::warn!(
            error = %e,
            "event-bus ingest connection refused: peer is not this process's own uid"
        );
        return;
    }

    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = match tokio::time::timeout(
            idle_timeout,
            read_capped_line(&mut reader, &mut line),
        )
        .await
        {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "event-bus ingest connection read error");
                return;
            }
            Err(_) => {
                tracing::warn!(
                    timeout = ?idle_timeout,
                    "event-bus ingest: connection idle too long; dropping"
                );
                return;
            }
        };
        if read == 0 && line.is_empty() {
            // EOF: the producer closed the connection. Not a failure — a
            // liveness probe or a clean disconnect looks identical here.
            return;
        }
        if !line.ends_with(b"\n") && line.len() >= MAX_LINE_BYTES {
            tracing::warn!(
                bytes = line.len(),
                max = MAX_LINE_BYTES,
                "event-bus ingest: line exceeded the max frame size; dropping connection"
            );
            return;
        }

        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<HarnessEvent>(trimmed) {
            Ok(event) => {
                bus.ingest(event);
            }
            Err(e) => {
                // Deliberately does not `return`: one malformed frame from a
                // producer must not cost every frame that follows it on the
                // same long-lived connection.
                tracing::warn!(
                    error = %e,
                    "event-bus ingest: malformed HarnessEvent line, dropped"
                );
            }
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
