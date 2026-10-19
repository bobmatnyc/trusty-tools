//! The bus socket: bind it, accept on it, and send each connection to ingest
//! or to `events.subscribe` by its first line (#9075, ADR-0065).
//!
//! Why: ADR-0065 names one socket after the bus, and both producers and
//! readers dial it. A producer's first line is a `HarnessEvent`; a reader's is
//! a JSON-RPC request, which always carries `"jsonrpc"` and which no
//! `HarnessEvent` carries. Reading that one line decides the connection, so
//! neither side needs a second socket or a handshake.
//! What: [`bind`] takes the socket with
//! [`trusty_common::uds::bind_singleton_hardened`]: a stale socket file is
//! reclaimed, and a live owner is refused with [`ServeError::Bind`] so the
//! daemon exits instead of serving alongside it. [`serve_with`] accepts until
//! shutdown, bounded at [`ServeLimits::max_connections`] concurrent
//! connections, one task each. An accept error is retried with backoff; after
//! [`ServeLimits::accept_error_budget`] failures in a row it returns
//! [`ServeError::AcceptFailed`], so a dead listener ends the process rather
//! than leaving producers' connects queued and unread. [`handle_connection`]
//! checks the peer's uid, reads the first line under the ingest cap and idle
//! timeout, and hands the connection to `ingest` or to the JSON-RPC router
//! that serves `events.subscribe` (`crate::subscribe`).
//! Test: `super::serve_tests`, `super::tests` (the moved ingest tests), and
//! `tests/daemon_wire.rs`, `tests/daemon_exit.rs` over the real binary.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use trusty_common::uds::{
    UdsSecurityError, accept_sized, bind_singleton_hardened, ensure_peer_is_self,
};

use crate::bus::EventBus;
use crate::ingest::{self, LineRead, READ_IDLE_TIMEOUT};

/// Largest number of connections, producers and readers together, served at
/// once.
///
/// Why a cap at all: an unbounded accept loop hands every accepted socket its
/// own task and `BufReader`, so a burst of connections costs memory with no
/// ceiling. 256 is well above the harnesses this workspace runs per host,
/// with headroom for reconnect storms (#6848).
/// Test: `super::tests::connections_beyond_the_limit_wait_for_a_free_slot`.
pub(crate) const MAX_CONCURRENT_CONNECTIONS: usize = 256;

/// Largest number of open `events.subscribe` streams.
///
/// Why: a subscription holds its connection slot for its whole life. Capping
/// readers below [`MAX_CONCURRENT_CONNECTIONS`] keeps slots free for
/// producers however many readers attach.
/// Test: `super::serve_tests::a_subscriber_beyond_the_limit_is_refused`.
pub(crate) const MAX_SUBSCRIBERS: usize = 64;

/// Accept failures in a row after which [`serve_with`] gives up.
///
/// Why: a listener that only fails (descriptor exhaustion that never clears,
/// a socket file unlinked and replaced) leaves producers' connects queued in
/// the kernel, unread, while each `PushClient` counts its flush as sent. With
/// the backoff below this is about a minute of continuous failure; the exit
/// hands recovery to launchd.
/// Test: `super::serve_tests::the_accept_loop_gives_up_after_its_error_budget`.
pub(crate) const ACCEPT_ERROR_BUDGET: u32 = 64;

/// The serve loop's limits, grouped so a test can shrink any of them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServeLimits {
    /// See [`MAX_CONCURRENT_CONNECTIONS`].
    pub max_connections: usize,
    /// See [`MAX_SUBSCRIBERS`].
    pub max_subscribers: usize,
    /// How long a connection may go without completing a line.
    pub idle_timeout: Duration,
    /// See [`ACCEPT_ERROR_BUDGET`].
    pub accept_error_budget: u32,
}

impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONCURRENT_CONNECTIONS,
            max_subscribers: MAX_SUBSCRIBERS,
            idle_timeout: READ_IDLE_TIMEOUT,
            accept_error_budget: ACCEPT_ERROR_BUDGET,
        }
    }
}

/// Why the bus socket stopped being served.
///
/// Why: each arm is a different operator action — find the other instance, or
/// look at the host's descriptors — and the daemon exits non-zero on either.
/// What: `Bind` when the socket could not be taken, `AcceptFailed` when the
/// accept loop exhausted its error budget.
/// Test: `tests/daemon_exit.rs`,
/// `super::serve_tests::the_accept_loop_gives_up_after_its_error_budget`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// The socket could not be bound, or another process already serves it.
    #[error("bind the trusty-events socket at {path}: {source}")]
    Bind {
        /// Socket that could not be bound.
        path: PathBuf,
        /// Why the bind failed.
        #[source]
        source: UdsSecurityError,
    },
    /// Accepting failed this many times in a row.
    #[error("accept on the trusty-events socket failed {consecutive} times in a row: {source}")]
    AcceptFailed {
        /// Consecutive failures, equal to the budget.
        consecutive: u32,
        /// The last accept error.
        #[source]
        source: std::io::Error,
    },
}

/// Bind the bus socket, reclaiming a stale socket file.
///
/// # Errors
///
/// [`ServeError::Bind`] when the path cannot be bound, including when another
/// process is still serving it; this process then does not take over.
///
/// Test: `super::tests::stale_socket_file_is_reclaimed_on_bind`,
/// `tests/daemon_exit.rs::a_second_instance_exits_non_zero_while_the_first_serves`.
pub(crate) async fn bind(socket: &Path) -> Result<UnixListener, ServeError> {
    bind_singleton_hardened(socket)
        .await
        .map_err(|source| ServeError::Bind {
            path: socket.to_path_buf(),
            source,
        })
}

/// Where accepted connections come from: the bound listener in production, a
/// scripted source in a test of the error budget.
pub(crate) trait Accept: Send + Sync {
    /// Accept the next connection.
    fn accept(&self) -> impl Future<Output = std::io::Result<UnixStream>> + Send;
}

impl Accept for UnixListener {
    async fn accept(&self) -> std::io::Result<UnixStream> {
        accept_sized(self).await.map(|(stream, _)| stream)
    }
}

/// What every connection task shares.
pub(crate) struct Connection {
    bus: Arc<EventBus>,
    idle_timeout: Duration,
}

impl Connection {
    /// The shared state for connections on `bus` under `limits`.
    pub(crate) fn new(bus: Arc<EventBus>, limits: ServeLimits) -> Self {
        Self {
            bus,
            idle_timeout: limits.idle_timeout,
        }
    }
}

/// Accept and serve connections until `shutdown` resolves.
///
/// Why: one task per connection, so a slow producer or a long subscription
/// never delays another connection's accept. The permit is taken before the
/// spawn, so at the cap the loop itself waits rather than piling up tasks, and
/// that wait is raced against `shutdown` (#6848).
/// What: see the module docs for the error budget.
///
/// # Errors
///
/// [`ServeError::AcceptFailed`] once accept has failed
/// [`ServeLimits::accept_error_budget`] times in a row. `Ok` on shutdown.
///
/// Test: `super::serve_tests::the_accept_loop_gives_up_after_its_error_budget`,
/// `super::serve_tests::accept_errors_below_the_budget_do_not_stop_the_loop`,
/// `super::tests::connections_beyond_the_limit_wait_for_a_free_slot`,
/// `super::tests::shutdown_is_observed_while_the_connection_pool_is_saturated`.
pub(crate) async fn serve_with<A: Accept>(
    acceptor: &A,
    bus: Arc<EventBus>,
    limits: ServeLimits,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), ServeError> {
    let shared = Arc::new(Connection::new(bus, limits));
    let semaphore = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    let budget = limits.accept_error_budget.max(1);
    let mut failures = 0u32;
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            accepted = acceptor.accept() => accepted,
        };
        let stream = match accepted {
            Ok(stream) => {
                failures = 0;
                stream
            }
            Err(source) => {
                // PRE-FIX (console behaviour): log and keep accepting forever.
                let _ = (budget, &mut failures);
                tracing::warn!(error = %source, "event-bus accept failed");
                continue;
            }
        };
        let permit = tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            permit = Arc::clone(&semaphore).acquire_owned() => {
                permit.expect("event-bus connection semaphore is never closed")
            }
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            handle_connection(stream, &shared).await;
            drop(permit);
        });
    }
}

/// Serve one accepted connection: verify the peer, read the first line, and
/// hand the connection to ingest or to the JSON-RPC router.
///
/// Why a peer check: the ADR-0034 §3 trust boundary every hardened socket in
/// the workspace enforces; the `0600` mode is an intention until the peer's
/// uid is read.
/// Test: `super::serve_tests::a_first_line_with_jsonrpc_is_routed_to_subscribe`,
/// `super::tests::idle_connection_is_dropped_after_the_read_timeout`,
/// `super::tests::ingest_over_uds_socket_reaches_the_bus`.
pub(crate) async fn handle_connection(stream: UnixStream, shared: &Connection) {
    if let Err(e) = ensure_peer_is_self(&stream) {
        tracing::warn!(
            error = %e,
            "event-bus connection refused: peer is not this process's own uid"
        );
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut first = Vec::new();
    match ingest::read_line_bounded(&mut reader, &mut first, shared.idle_timeout).await {
        LineRead::Line => {}
        end => {
            end.log_end(first.len(), shared.idle_timeout);
            return;
        }
    }
    // PRE-FIX (console behaviour): every connection is a producer.
    ingest::ingest_lines(&mut reader, first, &shared.bus, shared.idle_timeout).await;
}
