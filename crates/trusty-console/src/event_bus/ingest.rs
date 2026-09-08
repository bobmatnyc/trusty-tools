//! The UDS ingest listener producers dial (issue #6848, DOC-73 §4.2).
//!
//! Why: DOC-73 §4.2 makes console "the only ingester" — every harness pushes
//! `HarnessEvent` frames over one socket rather than console polling each of
//! them. This module is that socket's server half: bind it hardened (the same
//! `0700` directory / `0600` socket / peer-uid convention every other socket
//! in the workspace uses), accept connections, and read each one as
//! newline-delimited `HarnessEvent` JSON for as long as the producer keeps the
//! connection open — a producer's `PushClient` (§4.2, a parallel slice) is
//! expected to hold one long-lived connection and stream frames on it rather
//! than reconnect per event.
//! What: [`ingest_socket_path`] resolves the socket
//! (`daemon_socket_path("trusty-console")`, the same cross-crate convention
//! [`trusty_common::daemon_socket_path`] documents); [`bind_ingest`] binds it;
//! [`serve_ingest`] accepts until shutdown, spawning one task per connection.
//! A connection that sends a line that is not valid `HarnessEvent` JSON logs a
//! warning and keeps reading — one malformed frame must not cost every frame
//! after it on the same connection, still less every other producer's
//! connection. A line over [`MAX_LINE_BYTES`] ends that connection only.
//! Test: `super::tests` — `ingest_over_uds_socket_reaches_the_bus`,
//! `malformed_line_does_not_kill_the_listener`,
//! `oversized_line_ends_only_its_own_connection`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use trusty_common::control_bus::HarnessEvent;
use trusty_common::uds::{UdsSecurityError, accept_sized, bind_hardened, ensure_peer_is_self};

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
/// Test: `super::tests::oversized_line_ends_only_its_own_connection`.
pub(crate) const MAX_LINE_BYTES: usize = 1024 * 1024;

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

/// Bind the ingest socket, hardened per [`trusty_common::uds::bind_hardened`].
///
/// # Errors
///
/// [`IngestError::Bind`] when the path cannot be bound.
///
/// Test: `super::tests::ingest_over_uds_socket_reaches_the_bus`.
pub(crate) async fn bind_ingest(socket: &Path) -> Result<UnixListener, IngestError> {
    bind_hardened(socket).map_err(|source| IngestError::Bind {
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
/// accepted.
///
/// Test: `super::tests::ingest_over_uds_socket_reaches_the_bus`,
/// `super::tests::malformed_line_does_not_kill_the_listener`,
/// `super::tests::oversized_line_ends_only_its_own_connection`.
pub(crate) async fn serve_ingest(
    listener: UnixListener,
    bus: Arc<EventBus>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
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
        let bus = Arc::clone(&bus);
        tokio::spawn(handle_connection(stream, bus));
    }
}

/// Serve one accepted connection: verify the peer, then read newline-delimited
/// `HarnessEvent` JSON until EOF or a read error.
///
/// Why a peer check at all: the same ADR-0034 §3 trust boundary every other
/// hardened socket in this crate enforces — the `0600` mode is a documented
/// intention until something actually reads the peer's uid off the accepted
/// connection.
async fn handle_connection(stream: UnixStream, bus: Arc<EventBus>) {
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
        let read = match reader.read_until(b'\n', &mut line).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "event-bus ingest connection read error");
                return;
            }
        };
        if read == 0 {
            // EOF: the producer closed the connection. Not a failure — a
            // liveness probe or a clean disconnect looks identical here.
            return;
        }
        if line.len() > MAX_LINE_BYTES {
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
