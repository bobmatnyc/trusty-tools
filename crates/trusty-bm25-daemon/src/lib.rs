//! Per-palace BM25 lexical-search daemon library (issue #156).
//!
//! Why: extracting the daemon logic into a `[lib]` target lets
//! `trusty-memory` (and any other bundler crate) produce the
//! `trusty-bm25-daemon` binary without maintaining a separate install step.
//! The pattern mirrors PR #190 which did the same for `trusty-embedderd`
//! inside `trusty-search`.
//!
//! What: re-exports the internal daemon modules and exposes
//! [`DaemonConfig`] + [`run`] as the in-process entry point. `Cli` is also
//! re-exported so the binary `src/main.rs` and the bundled shim in
//! `trusty-memory/src/bin/bm25_daemon.rs` can share the same clap definition
//! without duplicating flags.
//!
//! Test: unit coverage in `protocol`, `socket`, `index`, `batch_queue`, and
//! `server`. End-to-end coverage in `tests/bm25_daemon.rs`.

pub mod batch_queue;
pub mod index;
pub mod protocol;
pub mod server;
pub mod socket;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};

use batch_queue::{BatchConfig, BatchQueue, DEFAULT_MAX_BATCH_SIZE, DEFAULT_WRITE_WINDOW_MS};
use index::PalaceBm25Index;

/// CLI flags for the BM25 daemon.
///
/// Why: operators (and parent processes like trusty-memory's subprocess
/// spawner) configure the daemon by passing flags. Exposing `Cli` from the
/// library lets the standalone binary (`src/main.rs`) AND the bundled shim
/// (`trusty-memory/src/bin/bm25_daemon.rs`) share one clap definition —
/// avoiding two copies that could drift.
/// What: palace name (determines the default socket path), data directory
/// (where the snapshot lives), optional socket override, batch-tuning knobs,
/// and verbosity. All have documented defaults from the batch_queue / socket
/// constants.
/// Test: covered indirectly by the integration test which constructs custom
/// palace / data-dir / socket arguments.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-bm25-daemon",
    version,
    about = "Per-palace BM25 lexical-index subprocess for the trusty-* ecosystem"
)]
pub struct Cli {
    /// Palace name — used to derive the default socket path
    /// (`$TMPDIR/trusty-bm25-<palace>.sock`) and to identify this instance
    /// in log messages.
    #[arg(long)]
    pub palace: String,

    /// Directory where the BM25 snapshot (`bm25_index.json`) is stored.
    /// Created automatically if it does not exist.
    #[arg(long)]
    pub data_dir: PathBuf,

    /// Override the Unix domain socket path. Defaults to
    /// `$TMPDIR/trusty-bm25-<palace>.sock`.
    #[arg(long, env = "TRUSTY_BM25_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Write-coalescing window in milliseconds.
    #[arg(long, default_value_t = DEFAULT_WRITE_WINDOW_MS, env = "TRUSTY_BM25_WRITE_WINDOW_MS")]
    pub write_window_ms: u64,

    /// Maximum number of write ops in one batch before forcing a flush.
    #[arg(long, default_value_t = DEFAULT_MAX_BATCH_SIZE, env = "TRUSTY_BM25_MAX_BATCH_SIZE")]
    pub max_batch_size: usize,

    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl Cli {
    /// Project the parsed CLI flags onto the library-facing [`DaemonConfig`].
    ///
    /// Why: the library's `run()` deliberately accepts a plain config struct
    /// so it can be driven from tests or embedders that never touch clap.
    /// This adapter is the single place that knows how to map CLI flags onto
    /// the in-process config — both the standalone binary and the bundled
    /// shim call it.
    /// What: moves each CLI field into the matching `DaemonConfig` field. The
    /// `verbose` count is intentionally NOT carried over because tracing
    /// initialisation is the binary's responsibility (a library caller may
    /// already have a subscriber installed).
    /// Test: covered indirectly by the integration test which exercises the
    /// full CLI → config → run path through the spawned binary.
    pub fn into_config(self) -> DaemonConfig {
        DaemonConfig {
            palace: self.palace,
            data_dir: self.data_dir,
            socket: self.socket,
            write_window_ms: self.write_window_ms,
            max_batch_size: self.max_batch_size,
        }
    }
}

/// In-process configuration for [`run`].
///
/// Why: separating config from CLI parsing lets in-process callers (tests,
/// embedders, the bundled shim in trusty-memory) construct the daemon
/// without going through clap or `std::env::args()`. The fields mirror the
/// CLI surface 1:1, minus the verbosity flag (which is a tracing-init
/// concern owned by the binary).
/// What: plain data struct — `palace` identifies the instance and seeds the
/// default socket name; `data_dir` is where the BM25 snapshot lives;
/// `socket` overrides the default UDS path when `Some`; `write_window_ms`
/// and `max_batch_size` tune the write-coalescing queue.
/// Test: end-to-end coverage in `tests/bm25_daemon.rs` constructs a
/// `DaemonConfig` indirectly by spawning the binary with matching CLI
/// flags; unit-level wiring is exercised by `Cli::into_config`.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Palace name. Used for the default socket filename and log fields.
    pub palace: String,
    /// Directory that holds the `bm25_index.json` snapshot. Created if
    /// missing.
    pub data_dir: PathBuf,
    /// Optional override for the UDS path. When `None`, defaults to
    /// `$TMPDIR/trusty-bm25-<palace>.sock`.
    pub socket: Option<PathBuf>,
    /// Write-coalescing window in milliseconds.
    pub write_window_ms: u64,
    /// Maximum write ops per coalesced batch.
    pub max_batch_size: usize,
}

/// Run the BM25 daemon to completion.
///
/// Why: the library entry point lets the bundled shim in trusty-memory and
/// the standalone `src/main.rs` both delegate here without duplicating any
/// daemon logic. Accepting a `DaemonConfig` (instead of parsing
/// `std::env::args()` internally) keeps the function testable and embeddable
/// — a caller can construct the config in code and drive the daemon
/// in-process.
/// What: loads or creates the `PalaceBm25Index`, spawns the batch-queue
/// worker, ensures the socket's parent directory exists, cleans up any
/// stale socket file, binds the Unix domain socket, then drives the accept
/// loop until SIGTERM or SIGINT. Removes the socket file on clean exit so
/// the next run does not see EADDRINUSE.
/// Test: end-to-end coverage in `tests/bm25_daemon.rs` (spawns the binary
/// and drives it over the UDS protocol).
pub async fn run(config: DaemonConfig) -> Result<()> {
    let DaemonConfig {
        palace,
        data_dir,
        socket,
        write_window_ms,
        max_batch_size,
    } = config;

    let socket_path = socket.unwrap_or_else(|| socket::default_socket_path(&palace));

    let batch_config = BatchConfig {
        max_batch_size: max_batch_size.max(1),
        write_window: Duration::from_millis(write_window_ms),
    };

    tracing::info!(
        palace = %palace,
        data_dir = %data_dir.display(),
        socket = %socket_path.display(),
        max_batch_size = batch_config.max_batch_size,
        write_window_ms = batch_config.write_window.as_millis(),
        "trusty-bm25-daemon starting"
    );

    // Step 1: load (or create) the palace BM25 snapshot. This validates the
    // data-dir exists and is writable before we bind the socket.
    let palace_index = PalaceBm25Index::load_or_create(&data_dir)
        .with_context(|| format!("load BM25 palace index from {}", data_dir.display()))?;

    // Step 2: spawn the batch-queue worker. The worker takes ownership of
    // the index and is the sole writer for the rest of the daemon's lifetime.
    let queue = Arc::new(BatchQueue::new(palace_index, batch_config));

    // Step 3: ensure the socket's parent directory exists, then clean up any
    // leftover socket file from a prior crash.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    socket::cleanup_stale_socket(&socket_path);

    // Step 4: bind the UDS listener.
    let listener = server::bind_listener(&socket_path)
        .with_context(|| format!("bind bm25 daemon socket at {}", socket_path.display()))?;
    tracing::info!(
        palace = %palace,
        socket = %socket_path.display(),
        "trusty-bm25-daemon ready"
    );

    // Step 5: run the accept loop alongside a signal-driven shutdown.
    let accept = tokio::spawn(server::run_accept_loop(listener, queue));

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM — shutting down");
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT — shutting down");
        }
        _ = accept => {
            // The accept loop never returns in normal operation.
            tracing::warn!("accept loop exited unexpectedly");
        }
    }

    // Step 6: remove the socket file on clean exit so the next run does not
    // see EADDRINUSE.
    socket::cleanup_stale_socket(&socket_path);
    Ok(())
}
