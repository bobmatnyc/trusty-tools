//! The receive half of the console→target webhook relay (#5182, ADR-0034).
//!
//! Why: before this module `trusty-console` sent [`super::RELAY_METHOD`] frames
//! to two sockets nothing bound. Every delivery landed in
//! `RelayOutcome::Unreachable`, stayed pending forever, and no review or
//! analysis ever ran. The receive side lives here rather than twice — once in
//! `trusty-analyze` and once in `trusty-review` — because the property that
//! matters is a single ordering rule, and two copies of an ordering rule is one
//! copy plus a future regression.
//!
//! 🔴 **The ordering rule, which is the whole point of the module.** A
//! [`super::RelayResponse::ack`] is written only after
//! [`DeliverySink::take_ownership`] has returned `Ok`, and that call is durable
//! before it returns (see [`super::inbox`]). An ack is the SOLE state that lets
//! the sender delete its spool entry, and that entry is the only remaining copy
//! — GitHub does not re-send a delivery it has seen acknowledged. So acking
//! first and working after does not make the failure less likely, it makes it
//! unrecoverable.
//!
//! What: [`dispatch_frame`] is the decision, as a synchronous function over
//! bytes, so every arm — including the two that must never ack — is assertable
//! without a socket. [`serve_until`] is the accept loop that binds it to a
//! listener, and [`handle_connection`] the per-connection half. Every non-ack
//! arm answers a JSON-RPC error rather than hanging up, so the sender records
//! `Refused` with the target's own words instead of an opaque transport
//! failure.
//!
//! Test: `tests.rs` — `dispatch_*` for the decision, `serve_*` for the socket.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::{JSONRPC_VERSION, RELAY_METHOD, RelayDelivery, RelayRequest, RelayResponse};
use crate::uds::{MAX_FRAME_BYTES, ensure_peer_is_self};

/// JSON-RPC code for a frame that is not valid JSON for [`RelayRequest`].
pub const CODE_PARSE_ERROR: i64 = -32700;

/// JSON-RPC code for a frame whose envelope is wrong (unsupported `jsonrpc`).
pub const CODE_INVALID_REQUEST: i64 = -32600;

/// JSON-RPC code for a method that is not [`RELAY_METHOD`].
pub const CODE_METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC code for a well-formed frame whose params are unusable.
pub const CODE_INVALID_PARAMS: i64 = -32602;

/// JSON-RPC code for "understood, but I could not take responsibility".
pub const CODE_NOT_DURABLE: i64 = -32000;

/// How long a listener may take to settle an in-flight delivery on shutdown.
///
/// Why: `trusty-console` must declare this as its supervised child's
/// `shutdown_flush` when it builds a `ServiceTimeouts`, and the sourcing rule on
/// that type requires the supervised binary's OWN constant rather than a literal
/// that happens to match. Console cannot depend on `trusty-review` or
/// `trusty-analyze`, so the constant lives in the contract module both halves
/// already share, and both targets honour it as their actual shutdown budget.
/// What: a listener holds no unflushed work by construction — it acks only
/// after an fsync — so this covers finishing the one connection it may be
/// mid-way through, bounded by [`ServeOptions::read_timeout`].
/// Test: `serve_options_read_timeout_fits_the_declared_flush_budget`.
pub const LISTENER_SHUTDOWN_FLUSH: Duration = Duration::from_secs(2);

/// Why a receiver declined to take responsibility for a delivery.
///
/// Why: maps one-to-one onto [`RelayResponse::refuse`] so a sink's failure
/// reaches the sender's durable `last_error` in the target's own words, rather
/// than as console's paraphrase of a transport error.
/// What: a JSON-RPC code and a human-readable message.
/// Test: `dispatch_refuses_when_the_sink_cannot_take_ownership`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkRejection {
    /// JSON-RPC error code reported to the sender.
    pub code: i64,
    /// Human-readable reason, stored verbatim in the sender's spool entry.
    pub message: String,
}

impl SinkRejection {
    /// A rejection meaning "the work is not durably held".
    pub fn not_durable(message: impl Into<String>) -> Self {
        Self {
            code: CODE_NOT_DURABLE,
            message: message.into(),
        }
    }
}

/// Whatever takes durable responsibility for a delivery on the receiver's side.
///
/// Why: the ack is conditioned on this returning `Ok`, so it is the seam a test
/// drives to prove that a receiver which CANNOT own the work does not ack — the
/// failure that would otherwise let the sender delete the only copy.
/// [`super::Inbox`] is the production implementation.
///
/// What: synchronous, because durability means filesystem work; [`serve_until`]
/// runs it on the blocking pool so it never stalls a runtime worker. Returning
/// `Ok` MUST mean the delivery survives a crash of this process.
///
/// Test: `dispatch_acks_only_after_the_sink_has_taken_ownership`,
/// `dispatch_refuses_when_the_sink_cannot_take_ownership`.
pub trait DeliverySink: Send + Sync + 'static {
    /// Take durable ownership, or say why it could not be taken.
    fn take_ownership(&self, delivery: &RelayDelivery) -> Result<(), SinkRejection>;
}

impl DeliverySink for super::Inbox {
    fn take_ownership(&self, delivery: &RelayDelivery) -> Result<(), SinkRejection> {
        match super::Inbox::take_ownership(self, delivery) {
            Ok(ownership) => {
                tracing::info!(
                    delivery_id = %delivery.delivery_id,
                    source = %delivery.source,
                    event = %delivery.event,
                    already_owned = ownership.already_owned,
                    path = %ownership.path.display(),
                    "webhook delivery is durably held; acknowledging"
                );
                Ok(())
            }
            // Not a `warn!`-and-continue: the refusal is what keeps the
            // sender's copy alive, so it has to reach the sender.
            Err(e) => {
                tracing::error!(
                    delivery_id = %delivery.delivery_id,
                    error = %e,
                    "could not take durable ownership; refusing so the sender keeps its copy"
                );
                Err(SinkRejection::not_durable(format!("{e}")))
            }
        }
    }
}

/// Per-connection budgets for [`serve_until`].
///
/// Test: `serve_options_read_timeout_fits_the_declared_flush_budget`.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// Longest one connection may take from accept to answered.
    pub read_timeout: Duration,
    /// Largest request frame accepted without a newline.
    pub max_frame_bytes: u64,
}

impl Default for ServeOptions {
    /// One second, and [`MAX_FRAME_BYTES`].
    ///
    /// The sender's own round-trip budget is 5 s and it writes the whole frame
    /// before half-closing, so a second is ample for a local socket while
    /// staying inside [`LISTENER_SHUTDOWN_FLUSH`].
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(1),
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }
}

/// Decide what to answer for one request frame.
///
/// 🔴 The ack arm is reachable only after `sink.take_ownership` returns `Ok`.
/// Do not hoist the ack above that call, and do not add an arm that acks on a
/// path where ownership was not taken — that reintroduces the exact silent loss
/// ADR-0034 §2 exists to eliminate, with the sender deleting its only copy on
/// the strength of it.
///
/// What, in order: parse; reject a non-`2.0` envelope; reject any method other
/// than [`RELAY_METHOD`]; reject a frame whose provenance says the sender did
/// not verify it (the sender refuses an unverified delivery before it reaches
/// its spool, so such a frame is a contract violation and never something to
/// take responsibility for); then, and only then, take ownership and ack.
///
/// Test: `dispatch_acks_only_after_the_sink_has_taken_ownership`,
/// `dispatch_refuses_when_the_sink_cannot_take_ownership`,
/// `dispatch_rejects_a_method_that_is_not_webhook_deliver`,
/// `dispatch_rejects_an_unparseable_frame`,
/// `dispatch_rejects_an_unverified_provenance`.
pub fn dispatch_frame(frame: &[u8], sink: &dyn DeliverySink) -> RelayResponse {
    let request: RelayRequest = match serde_json::from_slice(frame) {
        Ok(r) => r,
        Err(e) => {
            return RelayResponse::refuse(CODE_PARSE_ERROR, format!("unparseable relay frame: {e}"));
        }
    };

    if request.jsonrpc != JSONRPC_VERSION {
        return RelayResponse::refuse(
            CODE_INVALID_REQUEST,
            format!(
                "unsupported jsonrpc version {:?}; this listener speaks {JSONRPC_VERSION}",
                request.jsonrpc
            ),
        );
    }

    // #5182: the listener serves exactly one method. Anything else is refused
    // by name rather than ignored, so a sender that drifts sees an error it can
    // record instead of a delivery that silently goes nowhere.
    if request.method != RELAY_METHOD {
        return RelayResponse::refuse(
            CODE_METHOD_NOT_FOUND,
            format!(
                "unknown method {:?}; this listener serves only {RELAY_METHOD}",
                request.method
            ),
        );
    }

    if !request.params.provenance.verified {
        return RelayResponse::refuse(
            CODE_INVALID_PARAMS,
            "frame carries provenance.verified = false; the sender must verify \
             before relaying (ADR-0034 §3)"
                .to_string(),
        );
    }

    match sink.take_ownership(&request.params) {
        Ok(()) => RelayResponse::ack(),
        Err(rejection) => RelayResponse::refuse(rejection.code, rejection.message),
    }
}

/// Serve one accepted connection: verify the peer, read one frame, answer one.
///
/// Why: split out so a test can drive the wire behaviour against a plain
/// `UnixStream` pair without an accept loop.
/// What: refuses a peer whose uid is not our own (ADR-0034 §3 — this is what
/// makes the socket's permission bits an enforced boundary), reads bytes up to
/// the first newline or EOF under [`ServeOptions::max_frame_bytes`], runs
/// [`dispatch_frame`] on the blocking pool because a sink does filesystem work,
/// and writes one newline-terminated response frame.
///
/// # Errors
///
/// Any I/O failure, a foreign peer, or the read exceeding the budget. An error
/// here means no response was written, which the sender classifies as
/// `Unreachable` — a durable pending state, never an ack.
///
/// Test: `serve_round_trips_a_delivery_over_a_real_socket`,
/// `serve_rejects_an_oversized_frame`.
pub async fn handle_connection(
    mut stream: UnixStream,
    sink: Arc<dyn DeliverySink>,
    options: ServeOptions,
) -> std::io::Result<()> {
    ensure_peer_is_self(&stream).map_err(std::io::Error::other)?;

    let mut frame: Vec<u8> = Vec::new();
    {
        let mut reader = BufReader::new((&mut stream).take(options.max_frame_bytes));
        let read = reader.read_until(b'\n', &mut frame).await?;
        if read == 0 && frame.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "peer closed without sending a frame",
            ));
        }
        if !frame.ends_with(b"\n") && frame.len() as u64 >= options.max_frame_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "request frame exceeded {} bytes without a newline",
                    options.max_frame_bytes
                ),
            ));
        }
    }

    let response = tokio::task::spawn_blocking(move || dispatch_frame(&frame, sink.as_ref()))
        .await
        .map_err(|join| std::io::Error::other(format!("dispatch task did not complete: {join}")))?;

    let mut bytes = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Accept and serve connections until `shutdown` resolves.
///
/// Why: the socket must exist without the service running resident, so this is
/// the whole body of a short-lived, console-supervised child — bind, serve,
/// exit on SIGTERM.
/// What: each connection is handled inline rather than spawned. Webhook volume
/// is measured at zero deliveries in 14 days (#5028) and the handler is bounded
/// by [`ServeOptions::read_timeout`], so serialising costs nothing and removes
/// any question of how many tasks a shutdown has to wait for. A connection that
/// errors is logged and dropped without answering; the sender records
/// `Unreachable` and keeps its copy.
///
/// Test: `serve_round_trips_a_delivery_over_a_real_socket`,
/// `serve_stops_on_shutdown`.
pub async fn serve_until(
    listener: UnixListener,
    sink: Arc<dyn DeliverySink>,
    options: ServeOptions,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            biased;
            () = &mut shutdown => return,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "webhook listener accept failed");
                continue;
            }
        };
        let sink = Arc::clone(&sink);
        match tokio::time::timeout(
            options.read_timeout,
            handle_connection(stream, sink, options),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "webhook delivery connection failed; not acknowledged");
            }
            Err(_) => {
                tracing::warn!(
                    timeout = ?options.read_timeout,
                    "webhook delivery connection timed out; not acknowledged"
                );
            }
        }
    }
}
