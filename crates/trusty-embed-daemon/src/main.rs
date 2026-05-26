//! Batching ONNX embedding subprocess (closes #155).
//!
//! Why: trusty-memory's in-process embedder serialises all recall queries
//! through a single ONNX mutex, so under concurrent load p99 latency
//! collapses to ~894 ms. Splitting embedding into a dedicated subprocess
//! with a batching queue eliminates the mutex contention and unlocks
//! coalesced ONNX calls.
//!
//! What: a small Tokio binary that
//!   1. parses CLI flags (--socket, --batch-size, --batch-window-ms),
//!   2. initialises tracing on stderr,
//!   3. cleans up any stale socket file from a prior crash,
//!   4. boots a single `FastEmbedder` (the daemon's only ONNX session),
//!   5. spawns the `BatchQueue` worker,
//!   6. binds a `UnixListener` and runs the accept loop,
//!   7. waits for SIGTERM/SIGINT and removes the socket on clean exit.
//!
//! Test: unit coverage in `protocol.rs`, `socket.rs`, `batch_queue.rs`, and
//! `server.rs`. End-to-end coverage in `tests/embed_daemon.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};

mod batch_queue;
mod protocol;
mod server;
mod socket;

use batch_queue::{BatchConfig, BatchQueue};
use trusty_common::embedder::{Embedder, FastEmbedder};

/// CLI flags for the embed daemon.
///
/// Why: operators (and parent processes like trusty-memory's `--embed-daemon`
/// wiring) configure the daemon by passing flags. Keeping the surface tiny
/// matches the daemon's single responsibility.
/// What: socket path, batch-size cap, and batch-window-ms. All have
/// documented defaults sourced from `batch_queue` / `socket` constants.
/// Test: covered indirectly by the integration test which constructs a
/// custom socket path.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-embed-daemon",
    version,
    about = "Batching ONNX embedding subprocess for the trusty-* ecosystem"
)]
struct Cli {
    /// Path for the Unix domain socket (default: $TMPDIR/trusty-embed.sock).
    #[arg(long, env = "TRUSTY_EMBED_SOCKET")]
    socket: Option<PathBuf>,
    /// Maximum number of texts in one ONNX batch.
    #[arg(long, default_value_t = batch_queue::DEFAULT_BATCH_SIZE, env = "TRUSTY_EMBED_BATCH_SIZE")]
    batch_size: usize,
    /// Batching window in milliseconds.
    #[arg(long, default_value_t = batch_queue::DEFAULT_BATCH_WINDOW_MS, env = "TRUSTY_EMBED_BATCH_WINDOW_MS")]
    batch_window_ms: u64,
    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    trusty_common::init_tracing(cli.verbose);

    let socket_path = cli.socket.unwrap_or_else(socket::default_socket_path);
    let config = BatchConfig {
        batch_size: cli.batch_size.max(1),
        batch_window: Duration::from_millis(cli.batch_window_ms),
    };

    tracing::info!(
        "trusty-embed-daemon starting (socket={}, batch_size={}, batch_window_ms={})",
        socket_path.display(),
        config.batch_size,
        config.batch_window.as_millis()
    );

    // Step 1: ensure the socket's parent directory exists, then clean up any
    // leftover socket file from a prior run.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    socket::cleanup_stale_socket(&socket_path);

    // Step 2: boot the embedder. This downloads / loads the ONNX session and
    // runs a warmup batch, so it can take a few seconds on first run.
    let embedder = FastEmbedder::new()
        .await
        .context("initialise FastEmbedder for embed daemon")?;
    let embedder: Arc<dyn Embedder> = Arc::new(embedder);

    // Step 3: spawn the batch queue worker (takes ownership of the embedder).
    let queue = Arc::new(BatchQueue::new(embedder, config));

    // Step 4: bind the UDS listener.
    let listener = server::bind_listener(&socket_path)
        .with_context(|| format!("bind embed daemon socket at {}", socket_path.display()))?;
    tracing::info!("trusty-embed-daemon ready at {}", socket_path.display());

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
            // The accept loop never returns in normal operation; if it does
            // it means the listener was destroyed unexpectedly.
            tracing::warn!("accept loop exited unexpectedly");
        }
    }

    // Step 6: remove the socket file on clean exit so the next run does not
    // see EADDRINUSE.
    socket::cleanup_stale_socket(&socket_path);
    Ok(())
}
