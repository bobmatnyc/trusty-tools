//! Per-palace BM25 lexical-search subprocess (issue #156).
//!
//! Why: trusty-memory's recall path lacked a lexical lane — only vector
//! similarity. For short, identifier-heavy queries ("cargo test",
//! "PalaceHandle") BM25 routinely wins; hybrid recall via Reciprocal Rank
//! Fusion needs both lanes. Running BM25 in-process blocks the hot path on
//! disk I/O and contends with redb/usearch locks; a subprocess per palace
//! gives each palace its own writer (the subprocess IS the lock) and mirrors
//! the `trusty-embed-daemon` architecture (PR #157).
//!
//! What: a small Tokio binary that
//!   1. parses CLI flags (--palace, --data-dir, --socket, --write-window-ms,
//!      --max-batch-size, --verbose),
//!   2. initialises tracing on stderr,
//!   3. loads (or creates) the `PalaceBm25Index` snapshot from `data_dir`,
//!   4. spawns the `BatchQueue` worker,
//!   5. cleans up any stale socket file, binds the `UnixListener`,
//!   6. runs the accept loop alongside a SIGTERM/SIGINT shutdown handler,
//!   7. removes the socket file on clean exit.
//!
//! Test: unit coverage in `protocol.rs`, `socket.rs`, `index.rs`, `batch_queue.rs`,
//! and `server.rs`. End-to-end coverage in `tests/bm25_daemon.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};

mod batch_queue;
mod index;
mod protocol;
mod server;
mod socket;

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
struct Cli {
    /// Palace name — used to derive the default socket path
    /// (`$TMPDIR/trusty-bm25-<palace>.sock`) and to identify this instance
    /// in log messages.
    #[arg(long)]
    palace: String,

    /// Directory where the BM25 snapshot (`bm25_index.json`) is stored.
    /// Created automatically if it does not exist.
    #[arg(long)]
    data_dir: PathBuf,

    /// Override the Unix domain socket path. Defaults to
    /// `$TMPDIR/trusty-bm25-<palace>.sock`.
    #[arg(long, env = "TRUSTY_BM25_SOCKET")]
    socket: Option<PathBuf>,

    /// Write-coalescing window in milliseconds.
    #[arg(long, default_value_t = DEFAULT_WRITE_WINDOW_MS, env = "TRUSTY_BM25_WRITE_WINDOW_MS")]
    write_window_ms: u64,

    /// Maximum number of write ops in one batch before forcing a flush.
    #[arg(long, default_value_t = DEFAULT_MAX_BATCH_SIZE, env = "TRUSTY_BM25_MAX_BATCH_SIZE")]
    max_batch_size: usize,

    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> Result<()> {
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
