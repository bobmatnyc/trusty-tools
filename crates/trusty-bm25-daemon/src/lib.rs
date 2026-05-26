//! Per-palace BM25 lexical-search daemon library (issue #156).
//!
//! Why: extracting the daemon logic into a `[lib]` target lets
//! `trusty-memory` (and any other bundler crate) produce the
//! `trusty-bm25-daemon` binary without maintaining a separate install step.
//! The pattern mirrors PR #190 which did the same for `trusty-embedderd`
//! inside `trusty-search`.
//!
//! What: re-exports the internal daemon modules and exposes the single
//! `run()` entry point that `main.rs` (and the bundled shim in
//! `trusty-memory/src/bin/bm25_daemon.rs`) delegate to.
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
/// spawner) configure the daemon by passing flags. Keeping the surface small
/// matches the daemon's single responsibility.
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

/// Run the BM25 daemon from `std::env::args()`.
///
/// Why: the library entry point lets the bundled shim in trusty-memory and
/// the standalone `src/main.rs` both delegate here without duplicating any
/// daemon logic.
/// What: parses `Cli` via clap, initialises tracing to stderr, loads or
/// creates the `PalaceBm25Index`, spawns the batch-queue worker, binds the
/// Unix domain socket, then drives the accept loop until SIGTERM or SIGINT.
/// Removes the socket file on clean exit.
/// Test: end-to-end coverage in `tests/bm25_daemon.rs` (spawns the binary
/// and drives it over the UDS protocol).
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    trusty_common::init_tracing(cli.verbose);

    let socket_path = cli
        .socket
        .unwrap_or_else(|| socket::default_socket_path(&cli.palace));

    let config = BatchConfig {
        max_batch_size: cli.max_batch_size.max(1),
        write_window: Duration::from_millis(cli.write_window_ms),
    };

    tracing::info!(
        palace = %cli.palace,
        data_dir = %cli.data_dir.display(),
        socket = %socket_path.display(),
        max_batch_size = config.max_batch_size,
        write_window_ms = config.write_window.as_millis(),
        "trusty-bm25-daemon starting"
    );

    // Step 1: load (or create) the palace BM25 snapshot. This validates the
    // data-dir exists and is writable before we bind the socket.
    let palace_index = PalaceBm25Index::load_or_create(&cli.data_dir)
        .with_context(|| format!("load BM25 palace index from {}", cli.data_dir.display()))?;

    // Step 2: spawn the batch-queue worker. The worker takes ownership of
    // the index and is the sole writer for the rest of the daemon's lifetime.
    let queue = Arc::new(BatchQueue::new(palace_index, config));

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
        palace = %cli.palace,
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
