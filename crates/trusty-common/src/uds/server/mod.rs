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

mod router;
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

pub use router::{RpcMethod, RpcRouter, typed_method};
pub use wire::{
    CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND,
    CODE_PARSE_ERROR, JSONRPC_VERSION, RpcError, RpcRequest, RpcResponse,
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
    pub read_timeout: Duration,

    /// Largest request frame accepted without a newline.
    ///
    /// Defaults to [`MAX_FRAME_BYTES`], the same control-plane budget
    /// [`crate::uds::send_framed_request`] applies on the client side. A service
    /// exchanging bulk payloads raises it here, mirroring
    /// [`crate::uds::send_framed_request_capped`] — the two ends must agree, so
    /// raising one without the other only moves which side refuses.
    ///
    /// Not settable per method: the method name lives inside the frame this
    /// budget governs the reading of, so it is not known until after the budget
    /// has already been enforced.
    pub max_frame_bytes: u64,
}

impl Default for RpcServeOptions {
    /// Thirty seconds, and [`MAX_FRAME_BYTES`].
    ///
    /// The read covers a local socket writing one already-serialised frame, so
    /// thirty seconds is headroom for a stalled writer rather than a latency
    /// budget.
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(30),
            max_frame_bytes: MAX_FRAME_BYTES,
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
/// `router`, and writes one newline-terminated response frame.
///
/// # Errors
///
/// Any [`RpcServerError`] variant except `Bind`. An error here means no
/// response was written and the client sees a transport failure — which is why
/// every failure the router can reason about is a response frame instead.
///
/// Test: `serve_round_trips_a_request_over_a_real_socket`,
/// `serve_rejects_an_oversized_frame`,
/// `handle_connection_reports_a_liveness_probe_rather_than_a_failure`.
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

    let response = router.dispatch(&frame).await;
    let errored = response.is_error();
    // One owned, already-newline-terminated buffer, so the response leaves in a
    // single write rather than a serialise-then-append pair.
    let bytes =
        crate::uds::encode_frame(&response).map_err(|source| RpcServerError::Encode { source })?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|source| RpcServerError::Write { source })?;
    stream
        .flush()
        .await
        .map_err(|source| RpcServerError::Write { source })?;
    Ok(Served::Answered { errored })
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
/// A connection that errors is logged and dropped without answering.
///
/// Test: `serve_round_trips_a_request_over_a_real_socket`,
/// `serve_stops_on_shutdown`,
/// `serve_handles_concurrent_connections_without_serialising`.
pub async fn serve_until(
    listener: &UnixListener,
    router: Arc<RpcRouter>,
    options: RpcServeOptions,
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
                tracing::warn!(error = %e, "uds rpc listener accept failed");
                continue;
            }
        };
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            match handle_connection(stream, router, options).await {
                Ok(Served::Answered { .. }) => {}
                Ok(Served::LivenessProbe) => {
                    tracing::debug!("liveness probe connected and closed without a frame");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "uds rpc connection failed; nothing was answered");
                }
            }
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
