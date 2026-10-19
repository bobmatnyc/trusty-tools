//! The `trusty-events serve` lifecycle: bind, open the log, serve, drain
//! (#9075, ADR-0065).
//!
//! Why: the daemon's whole job is the bus, so every failure that would leave
//! it running without one — a socket another process owns, a log it cannot
//! open, an accept loop that only fails — ends the process with an error
//! instead. launchd restarts it, and `tm doctor` (#10042) sees the gap. A
//! producer never waits on any of this: `PushClient` buffers (ADR-0065
//! decision 4).
//! What: [`run`] binds the socket first — the singleton bind is what proves
//! no other instance is appending to the same log — then opens the log,
//! resuming `seq`, then serves until `shutdown`. On the way out it stops `seq`
//! assignment, waits up to [`DRAIN_BOUND`] for the log writer to write what it
//! holds, and unlinks the socket while still bound.
//! Test: `tests/daemon_exit.rs`, `tests/daemon_wire.rs`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::bus::{DEFAULT_CAPACITY, EventBus, EventBusConfig};
use crate::log::{DEFAULT_RETAIN_DAYS, DurableLog, LogConfig, LogError};
use crate::serve::{self, ServeError, ServeLimits};

/// Ring capacity when none is given (DOC-73 §4.3).
pub const DEFAULT_RING_CAPACITY: usize = DEFAULT_CAPACITY;

/// Calendar days of log files kept when none is given.
pub const DEFAULT_LOG_RETAIN_DAYS: u32 = DEFAULT_RETAIN_DAYS;

/// How long shutdown waits for the log writer to drain.
const DRAIN_BOUND: Duration = Duration::from_secs(5);

/// Where the daemon binds and logs, and how much it keeps.
///
/// Why: the binary resolves the defaults; a test points both paths into a
/// tempdir instead of the live data directory.
/// What: built by [`DaemonConfig::new`]; the counts default to
/// [`DEFAULT_LOG_RETAIN_DAYS`] and [`DEFAULT_RING_CAPACITY`] and can be set
/// afterwards.
/// Test: `a_subscriber_that_falls_behind_the_ring_gets_a_gap` (sets
/// `ring_capacity` through `--ring-capacity`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DaemonConfig {
    /// The socket to bind, normally `control_bus::events_socket_path()`.
    pub socket: PathBuf,
    /// The day-file directory, normally [`default_log_dir`].
    pub log_dir: PathBuf,
    /// Calendar days of day files kept.
    pub retain_days: u32,
    /// Events held in memory for live readers.
    pub ring_capacity: usize,
}

impl DaemonConfig {
    /// A configuration with these paths and the default counts.
    pub fn new(socket: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            socket,
            log_dir,
            retain_days: DEFAULT_LOG_RETAIN_DAYS,
            ring_capacity: DEFAULT_RING_CAPACITY,
        }
    }
}

/// `<data dir>/trusty-events/event_log/` (DOC-73 §4.3, ADR-0065).
///
/// # Errors
///
/// [`LogError::ResolveDir`] when the data directory cannot be resolved.
pub fn default_log_dir() -> Result<PathBuf, LogError> {
    LogConfig::resolve_default().map(|config| config.dir)
}

/// Why the daemon stopped with an error.
///
/// Why: the binary exits non-zero on every arm and prints the chain; each arm
/// names a different thing for an operator to fix.
/// What: `Serve` for the socket (bind refused, accept loop exhausted), `Log`
/// for the durable log.
/// Test: `tests/daemon_exit.rs`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonError {
    /// The socket could not be bound or served.
    #[error(transparent)]
    Serve(#[from] ServeError),
    /// The durable log could not be opened.
    #[error("open the trusty-events log: {0}")]
    Log(#[from] LogError),
}

/// Serve the bus until `shutdown` resolves.
///
/// Why/What: see the module docs.
///
/// # Errors
///
/// [`DaemonError::Serve`] when the socket is refused or the accept loop gives
/// up; [`DaemonError::Log`] when the log cannot be opened. `Ok` after a clean
/// shutdown.
///
/// Test: `tests/daemon_exit.rs::a_second_instance_exits_non_zero_while_the_first_serves`,
/// `tests/daemon_exit.rs::an_unopenable_log_directory_exits_non_zero`,
/// `tests/daemon_wire.rs::seq_continues_after_a_restart`.
pub async fn run(
    config: DaemonConfig,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), DaemonError> {
    let listener = serve::bind(&config.socket).await?;
    let log_config = LogConfig {
        dir: config.log_dir.clone(),
        retain_days: config.retain_days,
    };
    let (log, recovered) = match DurableLog::open(log_config).await {
        Ok(opened) => opened,
        Err(e) => {
            // #9075: no log means `seq` would restart at 1; refuse to serve.
            unlink_socket(&config.socket);
            return Err(e.into());
        }
    };
    tracing::info!(
        socket = %config.socket.display(),
        log_dir = %config.log_dir.display(),
        next_seq = recovered.next_seq,
        "trusty-events serving"
    );
    let bus = Arc::new(EventBus::with_log(
        EventBusConfig {
            capacity: config.ring_capacity,
        },
        Some(log),
        recovered.next_seq,
    ));
    let served = serve::serve_with(
        &listener,
        Arc::clone(&bus),
        ServeLimits::default(),
        shutdown,
    )
    .await;

    // #9075: stop handing out seq, then let the writer drain what it holds,
    // so every seq already handed out is on disk for the next start.
    if let Some(writer) = bus.close()
        && tokio::time::timeout(DRAIN_BOUND, writer).await.is_err()
    {
        tracing::warn!(bound = ?DRAIN_BOUND, "trusty-events: log writer did not drain in time");
    }
    // Unlink while still bound, so no instance that binds after us loses its
    // socket file to this cleanup.
    unlink_socket(&config.socket);
    drop(listener);
    served.map_err(DaemonError::from)
}

/// Remove the socket file this process bound; a missing file is fine.
fn unlink_socket(socket: &Path) {
    if let Err(e) = std::fs::remove_file(socket)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(error = %e, socket = %socket.display(), "could not remove the socket file");
    }
}
