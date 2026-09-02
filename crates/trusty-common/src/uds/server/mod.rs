//! The serving half of [`crate::uds::rpc`]: a generic framed JSON-RPC server
//! over a hardened Unix socket (#6277, ADR-0032).
//!
//! Why: `uds::rpc` is the client — it dials, writes one frame, reads one back —
//! and nothing in this workspace owned the other end generically. The only
//! server-side accept loop was `webhook_relay::serve`, which is a single-purpose
//! durable-delivery contract: one method, an ack conditioned on an fsync, an
//! inbox. ADR-0032 puts every trusty-* service's own transport on UDS, so the
//! next service to migrate needed either a fourth hand-rolled accept loop or
//! this. `webhook_relay` is deliberately left untouched; its ordering rule is
//! the reason it exists and is not a thing to generalise.
//!
//! What, in the order a daemon uses them.
//!   - [`RpcRouter`] is the caller's half: method names mapped to handlers over
//!     the caller's own request and response types (see [`RpcRouter::typed`]).
//!     A service that already has a generic `(method, params)` dispatcher
//!     mounts it whole through [`RpcRouter::fallback`] instead (#6286). A method
//!     that answers in many frames rather than one is registered with
//!     [`RpcRouter::typed_stream`] — see the wire contract below.
//!   - [`RpcServer::run`] is the whole body of a daemon — bind, serve, unlink.
//!   - [`serve_until`] and [`handle_connection`] are that body's two halves,
//!     public so a caller with its own bind (say
//!     [`crate::uds::bind_singleton_hardened`], which takes over a stale socket
//!     file where [`crate::uds::bind_hardened`] refuses) drives the loop itself.
//!
//! **The trust boundary is the socket, not the payload.** [`bind_hardened`]
//! puts the socket at `0600` inside a `0700` directory, and every accepted
//! connection runs [`ensure_peer_is_self`] before a single byte is read. No
//! CSRF, origin, or token machinery is ported from the HTTP shape those checks
//! replace — on a UDS socket it would guard nothing (#6277 design review).
//!
//! ## The wire contract, including streams (#6286)
//!
//! A request is one newline-terminated JSON-RPC frame, unchanged:
//!
//! ```text
//! {"jsonrpc":"2.0","id":7,"method":"chat","params":{…}}
//! ```
//!
//! plus ONE optional field, `"stream": true`. Absent means false, and false is
//! exactly the protocol as it stood — one request frame, one response frame,
//! connection closed. An old client and a new server therefore behave as they
//! always did, byte for byte, and so does a new client calling a method that
//! does not stream.
//!
//! A request that asks for a stream, against a method registered with
//! [`RpcRouter::typed_stream`], is answered with a SEQUENCE of frames on the
//! same connection, each newline-terminated and each carrying a `"stream"`
//! discriminant a plain response never has:
//!
//! ```text
//! {"jsonrpc":"2.0","id":7,"stream":"item","result":"Hel"}
//! {"jsonrpc":"2.0","id":7,"stream":"item","result":"lo"}
//! {"jsonrpc":"2.0","id":7,"stream":"end"}
//! ```
//!
//! **A stream terminates on a frame, never on EOF.** Exactly one terminal frame
//! is written on every path — `"stream":"end"` when the producer finishes, and
//! `"stream":"error"` carrying an [`RpcError`] when the handler fails mid-stream,
//! when it fails to open at all, or when an item does not fit the frame budget.
//! A client that reaches EOF without one reports it rather than returning what
//! it happened to receive: a truncated token stream read as a complete answer is
//! the Fail-Open branch this contract exists to close, and
//! `stream_reports_a_truncated_stream_rather_than_an_empty_success` is its
//! regression test.
//!
//! **The two mismatches both fail immediately, in the shape the caller reads.**
//! A request WITHOUT the flag against a streaming method gets one ordinary
//! response frame carrying [`CODE_STREAM_REQUIRED`]. A request WITH the flag
//! against anything that does not stream — a unary method, a fallback-served
//! name, an unknown name — gets one terminal `"stream":"error"` frame carrying
//! [`CODE_STREAM_UNSUPPORTED`], naming the methods this listener does stream.
//! Neither hangs, and neither leaves the caller decoding a frame shape it does
//! not expect.
//!
//! **The socket file is unlinked explicitly.** [`bind_hardened`] binds and
//! chmods; neither it nor `tokio::net::UnixListener`'s `Drop` removes the path,
//! so a server that just returned would leave a file the next start fails to
//! bind. [`RpcServer::run`] removes it — before dropping the listener, for the
//! reason `webhook_relay::listener` records: with the order reversed there is a
//! window where nothing answers the path but the file is still there, and a
//! successor that rebinds in that window has its fresh socket deleted by this
//! process's `remove_file`.
//!
//! Test: `tests.rs` — `dispatch_*` for the decision, `serve_*` for the socket.

mod idle;
mod router;
mod stream;
mod wire;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::uds::{MAX_FRAME_BYTES, UdsSecurityError, bind_hardened, ensure_peer_is_self};

pub use idle::{IdleGuard, IdleTracker};
pub use router::{RpcFallback, RpcMethod, RpcRouter, typed_method};
pub use stream::{RpcOutcome, RpcStreamItems, RpcStreamMethod, typed_stream_method, write_stream};
pub use wire::{
    CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND,
    CODE_PARSE_ERROR, CODE_STREAM_REQUIRED, CODE_STREAM_UNSUPPORTED, JSONRPC_VERSION, RpcError,
    RpcRequest, RpcResponse, RpcStreamFrame, StreamPhase,
};

/// Everything that can stop this server, or stop one of its connections.
///
/// `#[non_exhaustive]` for the same reason [`UdsSecurityError`] carries it: the
/// list grows as the transport tightens, and no consumer matches it
/// exhaustively — they log it or convert it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RpcServerError {
    /// The socket could not be bound or hardened.
    #[error("bind the rpc socket at {path}: {source}")]
    Bind {
        /// Socket that could not be bound.
        path: PathBuf,
        /// Why the bind failed.
        #[source]
        source: UdsSecurityError,
    },

    /// The connected peer failed the uid check, or its credentials could not be
    /// read. Fatal to the connection by design — see the module docs.
    #[error("refuse an accepted connection: {source}")]
    Peer {
        /// Which check failed.
        #[source]
        source: UdsSecurityError,
    },

    /// Reading the request frame failed.
    #[error("read request frame: {source}")]
    Read {
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The peer sent `limit` bytes without a newline.
    #[error("request frame exceeded {limit} bytes without a newline")]
    FrameTooLarge {
        /// The budget, in bytes.
        limit: u64,
    },

    /// The response frame could not be serialised.
    #[error("serialize response frame: {source}")]
    Encode {
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// Writing the response frame failed.
    #[error("write response frame: {source}")]
    Write {
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// Per-connection budgets for [`serve_until`].
///
/// Test: `serve_rejects_an_oversized_frame`.
#[derive(Debug, Clone, Copy)]
pub struct RpcServeOptions {
    /// Longest one connection may take to deliver a complete request frame.
    ///
    /// Bounds the READ only. A handler is not covered, deliberately: a service
    /// whose method runs for minutes (`trusty-review`'s is the case #6277 is
    /// migrating) would otherwise need this raised to a figure that also lets a
    /// peer which connects and never writes hold a task for that long.
    ///
    /// **It does not apply between streamed response frames** (#6286). The gap
    /// between two frames of a stream is the handler's own production latency —
    /// an LLM deciding on the next token — and a budget there would kill a
    /// legitimately slow stream. It bounds one thing: how long a peer may take
    /// to deliver its REQUEST frame. A service that wants an idle-stream budget
    /// enforces it in its producer, where the difference between "thinking" and
    /// "stuck" is knowable; the client applies its own per-frame budget in
    /// [`crate::uds::send_framed_stream_request`].
    pub read_timeout: Duration,

    /// Largest request frame accepted, counting its terminating newline.
    ///
    /// Precisely: a frame is accepted when its JSON body plus the `\n` is at
    /// most `max_frame_bytes`, so the usable payload is `max_frame_bytes - 1`.
    /// The terminator counting against the budget is not an accident to be
    /// tidied away — [`crate::uds::rpc`]'s `read_one_frame` reads with the same
    /// `take(max_frame_bytes)` and the same comparison, and the two ends have to
    /// draw the line at the same byte. Moving it here alone would produce a
    /// frame one end sends happily and the other refuses.
    ///
    /// Defaults to [`MAX_FRAME_BYTES`], the same control-plane budget
    /// [`crate::uds::send_framed_request`] applies. A service exchanging bulk
    /// payloads raises it here, mirroring
    /// [`crate::uds::send_framed_request_capped`] — and must raise the client's
    /// to match, or it has only moved which side refuses.
    ///
    /// **Per frame, in both directions** (#6286). On a streaming response it
    /// governs each item frame separately, not the stream's total: an item that
    /// would not fit is refused with a terminal error frame rather than
    /// truncated, because the client applies the same budget to each frame it
    /// reads and a frame it cannot buffer would desynchronise the rest of the
    /// connection.
    ///
    /// Not settable per method: the method name lives inside the frame this
    /// budget governs the reading of, so it is not known until after the budget
    /// has already been enforced.
    ///
    /// Test: `frame_of_exactly_the_budget_including_its_newline_is_accepted`,
    /// `serve_rejects_an_oversized_frame`,
    /// `stream_refuses_an_item_larger_than_the_frame_budget`.
    pub max_frame_bytes: u64,

    /// Longest [`serve_until_idle`] waits for in-flight connections to finish
    /// after the shutdown signal, before it returns and the caller unlinks the
    /// socket (#6601).
    ///
    /// Why a budget rather than an unbounded wait: the signal is the start of a
    /// window that ends in SIGKILL, and a handler that never returns must not
    /// spend it. When the budget expires the loop warns and returns anyway —
    /// holding the socket path open longer trades one hazard for a socket file
    /// nobody unlinks.
    ///
    /// Why this is NOT a per-connection read budget: [`Self::read_timeout`]
    /// bounds how long a peer may take to DELIVER a request. This bounds how
    /// long the loop waits for requests it already accepted to be answered, and
    /// a service whose method runs for minutes needs the second to be large
    /// while the first stays small.
    ///
    /// Defaults to [`crate::shutdown::plannable_grace`] — the process-wide
    /// SIGTERM-to-SIGKILL window MINUS [`crate::shutdown::CLEANUP_RESERVE`],
    /// including an operator's `TRUSTY_TERMINATION_GRACE_SECS` override.
    ///
    /// 🔴 **Why the reserve rather than the whole window (#6601).** The signal
    /// that starts this drain is the same signal that starts the SIGKILL
    /// countdown, so a drain sized to the whole window leaves the caller's
    /// post-serve work with no budget at all whenever the drain runs long.
    /// `trusty-memory` is the concrete case: `transport::uds::
    /// serve_with_shutdown` runs the BM25 exit flush AFTER `serve_until`
    /// returns, and `bm25_lane::shutdown` states there is "no window in which a
    /// SIGKILL can land mid-flush" — a full-window drain makes that false.
    ///
    /// A service whose real SIGKILL deadline is shorter than the process grace
    /// window sets this explicitly rather than inheriting the default — but
    /// establish that the deadline REACHES this process first (#6601 review).
    /// `trusty-analyze` briefly set it to its supervisor's `sigterm_patience`
    /// and that was wrong: it runs detached, so it is absent from the
    /// supervisor's population and no reap path signals it while it is serving.
    /// Overriding on a deadline that never fires does not avert a SIGKILL; it
    /// only gives up the drain early.
    ///
    /// Test: `shutdown_drains_an_in_flight_connection_before_it_returns`,
    /// `shutdown_returns_when_the_drain_budget_expires`,
    /// `default_serve_options_reserve_cleanup_time_inside_the_grace_window`.
    pub shutdown_drain: Duration,
}

impl Default for RpcServeOptions {
    /// Thirty seconds, [`MAX_FRAME_BYTES`], and the plannable termination grace.
    ///
    /// The read covers a local socket writing one already-serialised frame, so
    /// thirty seconds is headroom for a stalled writer rather than a latency
    /// budget.
    ///
    /// 🔴 **This `Default` reads the environment on every call (#6601).**
    /// `shutdown_drain` comes from [`crate::shutdown::plannable_grace`], which
    /// reads `TRUSTY_TERMINATION_GRACE_SECS` each time — so two values built
    /// either side of a `set_var` differ, and this cannot be a `const`.
    /// Deliberate: the override exists so a host whose supervisor window cannot
    /// be raised can tell the daemon the truth, and a value frozen at first call
    /// would ignore it. A caller wanting one stable budget for the process
    /// builds the options once and copies them, which is what [`RpcServer`] and
    /// `trusty-analyze`'s `serve_options` both do.
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(30),
            max_frame_bytes: MAX_FRAME_BYTES,
            shutdown_drain: crate::shutdown::plannable_grace(),
        }
    }
}

/// What one accepted connection turned out to be.
///
/// Why: a peer that connects and closes without writing is a liveness probe —
/// [`crate::uds::probe::socket_is_serving`] and `UdsServiceSupervisor` both do
/// exactly that. Collapsing it into the failure arm makes the one warning an
/// operator greps for fire on every successful health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// A frame arrived and a response frame was written.
    Answered {
        /// Whether that response carried a JSON-RPC error.
        errored: bool,
        /// Whether the method answered was marked with
        /// [`RpcRouter::mark_liveness`] (#6621).
        ///
        /// An answer to a liveness method is a monitor asking whether the
        /// process is up. It is a real answer — the client gets its frame — but
        /// it is not work, so [`serve_until_idle`] does not credit it as
        /// activity. Without this, a poller dialling faster than the idle window
        /// keeps an on-demand service resident forever.
        liveness: bool,
    },
    /// The peer connected and closed without sending a byte.
    LivenessProbe,
}

/// Serve one accepted connection: verify the peer, read one frame, answer one.
///
/// Why: split out so a test can drive the wire behaviour against a plain
/// `UnixStream` pair without an accept loop.
/// What: refuses a peer whose uid is not our own, reads bytes up to the first
/// newline or EOF under [`RpcServeOptions::max_frame_bytes`], dispatches through
/// `router`, and writes the answer — one response frame for a unary call, or the
/// frame sequence the module docs' wire contract describes for a streaming one
/// (#6286).
///
/// # Errors
///
/// Any [`RpcServerError`] variant except `Bind`. An error here means no complete
/// answer was written and the client sees a transport failure — which is why
/// every failure the router can reason about is a frame instead. On a stream,
/// [`RpcServerError::Write`] mid-sequence is the client having gone away: the
/// producer stops when its receiver drops here, and the accept loop is
/// unaffected.
///
/// Test: `serve_round_trips_a_request_over_a_real_socket`,
/// `serve_rejects_an_oversized_frame`,
/// `handle_connection_reports_a_liveness_probe_rather_than_a_failure`,
/// `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_survives_a_client_that_disconnects_mid_stream`.
pub async fn handle_connection(
    mut stream: UnixStream,
    router: Arc<RpcRouter>,
    options: RpcServeOptions,
) -> Result<Served, RpcServerError> {
    ensure_peer_is_self(&stream).map_err(|source| RpcServerError::Peer { source })?;

    let mut frame: Vec<u8> = Vec::new();
    {
        let mut reader = BufReader::new((&mut stream).take(options.max_frame_bytes));
        let read = tokio::time::timeout(options.read_timeout, reader.read_until(b'\n', &mut frame))
            .await
            .map_err(|_| RpcServerError::Read {
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no complete frame within {:?}", options.read_timeout),
                ),
            })?
            .map_err(|source| RpcServerError::Read { source })?;
        if read == 0 && frame.is_empty() {
            return Ok(Served::LivenessProbe);
        }
        if !frame.ends_with(b"\n") && frame.len() as u64 >= options.max_frame_bytes {
            return Err(RpcServerError::FrameTooLarge {
                limit: options.max_frame_bytes,
            });
        }
    }

    // #6621: classified BEFORE dispatch, off the frame the loop already read.
    let liveness = router.frame_is_liveness(&frame);

    let errored = match router.dispatch_streaming(&frame).await {
        RpcOutcome::Single(response) => {
            let errored = response.is_error();
            // One owned, already-newline-terminated buffer, so the response
            // leaves in a single write rather than a serialise-then-append pair.
            let bytes = crate::uds::encode_frame(&response)
                .map_err(|source| RpcServerError::Encode { source })?;
            stream
                .write_all(&bytes)
                .await
                .map_err(|source| RpcServerError::Write { source })?;
            stream
                .flush()
                .await
                .map_err(|source| RpcServerError::Write { source })?;
            errored
        }
        // #6286: many frames, ending in exactly one terminal frame. A write
        // failure part-way through is a dead client, not a server fault — it
        // surfaces as `Write`, the connection is dropped, and the accept loop
        // keeps serving. The handler's producer stops on its own when the
        // receiver `write_stream` owns is dropped here.
        RpcOutcome::Stream { id, items } => {
            write_stream(&mut stream, id, items, options.max_frame_bytes)
                .await
                .map_err(|source| RpcServerError::Write { source })?
        }
    };
    Ok(Served::Answered { errored, liveness })
}

/// Accept and serve connections until `shutdown` resolves.
///
/// Why: the whole loop of a UDS daemon, so a service supplies a [`RpcRouter`]
/// and nothing else.
///
/// What: each connection is handed to `tokio::spawn` rather than served inline.
/// That is a REQUIREMENT, not a throughput preference: `uds::probe`'s
/// `SocketVerdict` docs record that on macOS a bound listener with a saturated
/// accept queue answers ECONNREFUSED, which a prober classifies as `NotServing`.
/// A server that dispatched inline would be read as dead under exactly the load
/// it was handling.
///
/// The listener is borrowed, not consumed, so a caller can unlink the socket
/// while it is still bound — see the module docs for why that order matters.
///
/// A connection that errors — or whose handler panics — is logged and dropped
/// without answering, and the loop keeps accepting.
///
/// Test: `serve_round_trips_a_request_over_a_real_socket`,
/// `serve_stops_on_shutdown`,
/// `serve_handles_concurrent_connections_without_serialising`,
/// `serve_survives_a_panicking_handler_and_answers_the_next_connection`.
pub async fn serve_until(
    listener: &UnixListener,
    router: Arc<RpcRouter>,
    options: RpcServeOptions,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let _ = serve_until_idle(listener, router, options, shutdown, None).await;
}

/// Why a serve loop ended.
///
/// Why this is returned rather than logged: an on-demand service's caller
/// distinguishes the two — a shutdown is an operator or supervisor stopping the
/// process, an idle exit is the process reclaiming itself and is the normal end
/// of a successful lifetime. `trusty-analyze` prints a different line for each.
///
/// Test: `serve_until_idle_exits_when_the_window_elapses`,
/// `serve_stops_on_shutdown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExit {
    /// The caller's shutdown future resolved (SIGTERM/SIGINT, or a test), and
    /// the in-flight connections have finished or spent
    /// [`RpcServeOptions::shutdown_drain`] (#6601).
    Shutdown,
    /// No connection answered anything for the whole idle window.
    Idle,
}

/// How long an idle exit waits for a connection already in the kernel backlog.
///
/// Why (#6350): `connect(2)` against a listening socket succeeds the moment the
/// kernel queues it — the server need not have called `accept` yet. So a client
/// that dialled microseconds before the idle window elapsed is sitting in the
/// backlog, and a loop that returned straight from the idle arm would drop the
/// listener and unlink the socket with that client's connection still queued.
/// The client sees a reset, not a refusal, and only `trusty-review`'s adapter
/// retries one; `trusty-analyze deep` and `tctl`'s probe report it as a failure
/// the operator cannot act on.
///
/// What: 50ms is chosen against the cost of being wrong in each direction. A
/// queued connection is already in the backlog, so it is accepted on the first
/// poll and the window is never actually spent; the only case that spends it is
/// a genuinely idle service, which pays 50ms once in its whole lifetime.
const IDLE_EXIT_DRAIN: Duration = Duration::from_millis(50);

/// The future [`serve_until_idle`] races against `accept`.
///
/// Why a named function rather than an inline `async` block: two `async` blocks
/// have two different anonymous types, and the loop re-arms this one in place
/// after a drain. `Pin::set` needs the replacement to be the SAME type, which
/// only one `impl Future` origin gives.
///
/// What: [`IdleTracker::expired`] when a policy is configured. With none it
/// never resolves, so the arm is inert and the loop behaves as [`serve_until`]
/// always has.
async fn idle_expiry(idle: Option<Arc<IdleTracker>>) {
    match idle {
        Some(tracker) => tracker.expired().await,
        None => std::future::pending::<()>().await,
    }
}

/// What [`drain_backlog`] found in the backlog.
enum Drained {
    /// A client was queued and its request has been answered. The exit is
    /// cancelled and the idle window restarts.
    Answered,
    /// Nothing was queued, or what was queued asked nothing. The exit stands.
    Nothing,
}

/// Which arm of [`serve_until_idle`]'s `select!` won.
///
/// Why an enum rather than the drain running inside the arm: the drain re-arms
/// the pinned idle future afterwards, and `Pin::set` cannot run while the
/// `select!` still holds the `&mut` borrow of it. Naming the winner ends that
/// borrow at the `select!`'s closing brace.
enum Step {
    /// `accept` won; this is its result, error included.
    Accepted(std::io::Result<(UnixStream, tokio::net::unix::SocketAddr)>),
    /// The idle window elapsed.
    IdleElapsed,
}

/// Serve one connection queued before the idle window elapsed, if any.
///
/// Why: see [`IDLE_EXIT_DRAIN`]. Resolving the race in the CLIENT's favour is
/// the only safe direction — serving one extra request costs a round trip,
/// while resetting a connection the client believes it made costs that client a
/// failure it has no way to distinguish from a broken service.
///
/// 🔴 Why only an ANSWERED connection cancels the exit: a bare connect-and-close
/// is what [`crate::uds::socket_is_serving`] does on a poll loop, and a drain
/// that treated one as activity would hand the poller exactly the power
/// [`IdleGuard`] exists to deny it — an observed livelock, not a hypothetical.
/// A probe caught in the drain is served and the exit proceeds.
///
/// Why the connection is served INLINE rather than spawned: this is the last
/// thing that happens before the caller unlinks the socket and drops the
/// listener, and a spawned task would race the process exit. Awaiting it is what
/// guarantees the client has its answer first. One connection, once per
/// lifetime.
///
/// Test: `a_client_queued_when_the_idle_window_elapses_is_served_not_reset`,
/// `serve_until_idle_ignores_liveness_probes`.
async fn drain_backlog(
    listener: &UnixListener,
    router: &Arc<RpcRouter>,
    options: RpcServeOptions,
    idle: &Arc<IdleTracker>,
) -> Drained {
    let Ok(accepted) = tokio::time::timeout(IDLE_EXIT_DRAIN, listener.accept()).await else {
        return Drained::Nothing;
    };
    let stream = match accepted {
        Ok((stream, _)) => stream,
        Err(e) => {
            tracing::warn!(error = %e, "uds rpc listener accept failed during idle drain");
            return Drained::Nothing;
        }
    };
    let mut guard = IdleTracker::connection_opened(idle);
    let served = handle_connection(stream, Arc::clone(router), options).await;
    // #6621: a liveness method caught in the drain is served, and the exit still
    // stands — a poller must not be able to cancel the exit any more than it can
    // re-arm the window.
    let answered = matches!(
        served,
        Ok(Served::Answered {
            liveness: false,
            ..
        })
    );
    if let Err(e) = &served {
        tracing::warn!(error = %e, "uds rpc connection failed during idle drain");
    }
    if answered {
        guard.answered();
    }
    guard.release().await;
    if answered {
        Drained::Answered
    } else {
        Drained::Nothing
    }
}

/// How often [`drain_shutdown`] re-reads the in-flight count.
///
/// Five milliseconds: the drain ends the moment the last handler releases its
/// guard, so this bounds how long a clean shutdown lingers past that release.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Let in-flight connections finish before the caller unlinks the socket
/// (#6601).
///
/// Why: the shutdown arm used to return the instant the signal resolved. Every
/// accepted connection holds an `Arc<RpcRouter>` clone — and through it whatever
/// the service's handlers own, a redb `Database` in `trusty-analyze`'s case — so
/// returning under an open connection unlinked the socket while those handles
/// were still live. The unlink is what tells a client to spawn a successor, and
/// the successor then died opening a store this process had not let go of
/// (#6595). Draining HERE gives that guarantee to every service behind this
/// loop rather than to the one caller that noticed.
///
/// What, while the count is above zero and the budget has not expired:
///   - the accept loop is over, so nothing new is served; and
///   - a client that dials anyway is accepted and IMMEDIATELY closed, which
///     reaches it as `UdsRpcError::NoResponse` on the next poll. Leaving it in
///     the backlog instead would hold it open until the listener dropped and
///     then reset it — a failure the client cannot tell from a broken service.
///
/// A budget that expires warns and returns: the process is exiting on a signal
/// either way, and holding the path open longer trades one hazard for a socket
/// file nobody unlinks.
///
/// Connections queued in the kernel backlog but never accepted are NOT counted —
/// `connect(2)` succeeds before this server sees anything. Those are refused by
/// the loop above when a drain is running, and reset by the listener drop when
/// there was nothing to drain.
///
/// Test: `shutdown_drains_an_in_flight_connection_before_it_returns`,
/// `shutdown_refuses_a_connection_dialled_after_the_signal`,
/// `shutdown_returns_when_the_drain_budget_expires`.
async fn drain_shutdown(listener: &UnixListener, open: &Arc<IdleTracker>, budget: Duration) {
    if open.open_connections() == 0 {
        return;
    }
    // #6601 review: a process serving more than one socket needs the warn below
    // to say WHICH one is still busy. `local_addr` is the listener's own answer,
    // so it cannot disagree with the path actually bound; an unnamed socket
    // (which this loop never binds) degrades to "<unknown>" rather than a panic.
    let socket = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "<unknown>".to_string());
    tracing::info!(
        %socket,
        open = open.open_connections(),
        ?budget,
        "uds rpc shutdown signalled; draining in-flight connections"
    );

    let refuse = async {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tracing::debug!("refusing a connection dialled during the shutdown drain");
                    drop(stream);
                }
                // Never a reason to stop refusing: the whole arm is bounded by
                // the `select!`'s budget, and a bare `continue` on a persistent
                // error (EMFILE, say) would spin.
                Err(e) => {
                    tracing::debug!(error = %e, "accept failed during the shutdown drain");
                    tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
                }
            }
        }
    };
    let settled = async {
        while open.open_connections() > 0 {
            tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
        }
    };

    tokio::select! {
        () = settled => {}
        () = refuse => {}
        () = tokio::time::sleep(budget) => tracing::warn!(
            %socket,
            open = open.open_connections(),
            ?budget,
            "connections still in flight after the shutdown drain budget; \
             unlinking anyway"
        ),
    }
}

/// [`serve_until`], plus an optional idle-exit policy (#6350).
///
/// Why: ADR-0032 makes `trusty-analyze` an on-demand service — clients spawn it
/// and nothing supervises it — so the accept loop is the only place that knows
/// enough to end the process. It has to be here rather than in a timer the
/// service arms itself, because "idle" means no connection is OPEN and none has
/// answered recently, and only this loop observes both.
///
/// What: identical to [`serve_until`] with `idle` as `None`. With a tracker, the
/// loop additionally races [`IdleTracker::expired`] against `accept` and returns
/// [`ServeExit::Idle`] when it wins. Each accepted connection holds an
/// [`IdleGuard`] for its lifetime, so the window can never elapse under an open
/// connection; the guard is marked answered only for a connection that produced
/// a response, which is what keeps a liveness-probe poll loop from pinning the
/// process alive.
///
/// #6621: a response to a method registered with [`RpcRouter::mark_liveness`]
/// does not mark the guard either. A monitor that dials a health METHOD instead
/// of connecting and closing was otherwise indistinguishable from a client doing
/// work, and pinned an on-demand `trusty-analyze` process resident for 46 hours.
///
/// #6350: an expired window does not exit immediately. [`drain_backlog`] first
/// gives the kernel backlog [`IDLE_EXIT_DRAIN`] to yield a connection that was
/// queued before the window elapsed; one that appears is served like any other
/// and the loop continues, so the exit only stands when nobody was waiting.
///
/// #6601: the shutdown arm drains too. Every accepted connection is counted —
/// with an idle policy or without one — and the signal runs [`drain_shutdown`]
/// before [`ServeExit::Shutdown`] is returned, so the caller's unlink never
/// lands on top of a handler that is still holding the router.
///
/// Test: `serve_until_idle_exits_when_the_window_elapses`,
/// `serve_until_idle_is_reset_by_an_answered_request`,
/// `serve_until_idle_ignores_liveness_probes`,
/// `serve_until_idle_ignores_a_registered_liveness_method`,
/// `serve_until_idle_is_held_open_by_a_non_liveness_call`,
/// `a_client_queued_when_the_idle_window_elapses_is_served_not_reset`,
/// `shutdown_drains_an_in_flight_connection_before_it_returns`,
/// `shutdown_refuses_a_connection_dialled_after_the_signal`,
/// `shutdown_returns_when_the_drain_budget_expires`.
pub async fn serve_until_idle(
    listener: &UnixListener,
    router: Arc<RpcRouter>,
    options: RpcServeOptions,
    shutdown: impl std::future::Future<Output = ()> + Send,
    idle: Option<Arc<IdleTracker>>,
) -> ServeExit {
    tokio::pin!(shutdown);
    let idle_expired = idle_expiry(idle.clone());
    tokio::pin!(idle_expired);

    // #6601: connections are counted whether or not an idle policy exists — the
    // shutdown drain asks the same question the idle window does.
    let open = match &idle {
        Some(tracker) => Arc::clone(tracker),
        None => IdleTracker::counting_only(),
    };

    loop {
        let step = tokio::select! {
            biased;
            () = &mut shutdown => {
                // #6601: in-flight handlers finish before the caller unlinks.
                drain_shutdown(listener, &open, options.shutdown_drain).await;
                return ServeExit::Shutdown;
            }
            () = &mut idle_expired => Step::IdleElapsed,
            accepted = listener.accept() => Step::Accepted(accepted),
        };
        // #6350: the drain runs HERE, not inside the arm — the `select!`'s
        // borrow of `idle_expired` has ended, so the re-arm below can run. The
        // future is re-armed ONLY on this path: an `async` block panics if
        // polled after it completes, and rebuilding it on every iteration
        // instead would restart `expired`'s open-connection branch, which sleeps
        // a whole window whenever it observes a connection in flight — a client
        // polling faster than the window would then hold the process open
        // forever.
        let accepted = match step {
            Step::Accepted(accepted) => accepted,
            Step::IdleElapsed => match drain_backlog(listener, &router, options, &open).await {
                Drained::Answered => {
                    idle_expired.set(idle_expiry(idle.clone()));
                    continue;
                }
                Drained::Nothing => return ServeExit::Idle,
            },
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "uds rpc listener accept failed");
                continue;
            }
        };
        // Taken BEFORE the connection task is spawned: taking it inside would
        // leave a window in which the count is zero while a connection is
        // already accepted, and the idle future could win the race in it.
        let guard = IdleTracker::connection_opened(&open);
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            // #6277 review: the connection runs in an INNER task whose
            // `JoinHandle` is awaited, because a panicking handler is otherwise
            // invisible. Tokio stores the panic payload in the handle and drops
            // it with the handle, so a dropped handle turns a caller-visible
            // hang-up into a server that logged nothing at all. Awaiting it
            // costs one task per connection and buys the one log line that says
            // which method took the process down a path nobody expected.
            let inner = tokio::spawn(handle_connection(stream, router, options));
            let mut guard = guard;
            match inner.await {
                Ok(Ok(Served::Answered {
                    liveness: false, ..
                })) => guard.answered(),
                // #6621: a liveness METHOD was answered. The client has its
                // frame; the window is deliberately left where it was.
                Ok(Ok(Served::Answered { .. })) => {
                    tracing::debug!("liveness method answered; the idle window is unchanged");
                }
                Ok(Ok(Served::LivenessProbe)) => {
                    tracing::debug!("liveness probe connected and closed without a frame");
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "uds rpc connection failed; nothing was answered");
                }
                // A panic is the handler's bug, not the client's, so it is an
                // ERROR rather than the WARN a refused or broken connection
                // gets. `JoinError`'s `Display` carries the panic message.
                Err(join) if join.is_panic() => {
                    tracing::error!(
                        error = %join,
                        "uds rpc handler panicked; the connection was dropped unanswered"
                    );
                }
                Err(join) => {
                    tracing::warn!(error = %join, "uds rpc connection task was cancelled");
                }
            }
            guard.release().await;
        });
    }
}

/// A framed JSON-RPC server bound to one hardened socket.
///
/// Why: the shape a daemon's `serve` command wants — one value carrying the
/// path, the methods and the budgets, with [`run`] as its whole body.
/// What: [`RpcServer::run`] binds through [`bind_hardened`], serves until the
/// caller's shutdown future resolves, then unlinks the socket file.
/// Test: `server_round_trips_and_removes_its_socket_on_shutdown`.
///
/// [`run`]: RpcServer::run
#[derive(Debug)]
pub struct RpcServer {
    socket: PathBuf,
    router: Arc<RpcRouter>,
    options: RpcServeOptions,
}

impl RpcServer {
    /// A server that will bind `socket` and serve `router`.
    pub fn new(socket: impl Into<PathBuf>, router: RpcRouter) -> Self {
        Self {
            socket: socket.into(),
            router: Arc::new(router),
            options: RpcServeOptions::default(),
        }
    }

    /// Override the per-connection budgets.
    pub fn with_options(mut self, options: RpcServeOptions) -> Self {
        self.options = options;
        self
    }

    /// Socket this server binds.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Bind and serve until `shutdown` resolves, then unlink the socket.
    ///
    /// [`bind_hardened`] refuses a path a socket file already occupies rather
    /// than clobbering what might be a live owner. A service that is respawned
    /// on demand, and so has to take over the corpse a killed predecessor left,
    /// binds with [`crate::uds::bind_singleton_hardened`] itself and drives
    /// [`serve_until`] directly — that decision is the caller's, which is the
    /// same split `bind_hardened`'s own docs make.
    ///
    /// # Errors
    ///
    /// [`RpcServerError::Bind`] when the path cannot be bound or hardened.
    /// Per-connection failures never reach here; they are logged and the loop
    /// continues.
    ///
    /// Test: `server_round_trips_and_removes_its_socket_on_shutdown`.
    pub async fn run(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<(), RpcServerError> {
        let listener = bind_hardened(&self.socket).map_err(|source| RpcServerError::Bind {
            path: self.socket.clone(),
            source,
        })?;

        tracing::info!(
            socket = %self.socket.display(),
            methods = ?self.router.method_names().collect::<Vec<_>>(),
            streams = ?self.router.stream_names().collect::<Vec<_>>(),
            "uds rpc server bound"
        );

        serve_until(&listener, Arc::clone(&self.router), self.options, shutdown).await;

        // Unlink BEFORE dropping the listener — see the module docs. A failure
        // is not worth an exit code: the file is either already gone or belongs
        // to whoever rebound the path.
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(socket = %self.socket.display(), error = %e, "socket already gone");
        }
        drop(listener);
        Ok(())
    }
}
