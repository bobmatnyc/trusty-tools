//! `trusty-embedderd` library entry point.
//!
//! Why: exposes the daemon's startup logic as a library function so that
//! downstream consumers (notably the `trusty-embedderd` shim binary bundled
//! inside `trusty-search`) can call into it without duplicating any logic.
//! The binary in this crate and the bundled binary in `trusty-search` both
//! call `trusty_embedderd::run()` — one command, two install paths, zero
//! divergence.
//!
//! What: re-exports the internal submodules (for integration tests that import
//! by module path) and provides `run()`, which parses `std::env::args()` via
//! clap, initialises tracing, loads the ONNX model, and dispatches to the
//! selected transport. Returns `Ok(())` on clean shutdown; propagates any
//! startup error.
//!
//! Test: `cargo test -p trusty-embedderd` (unit + integration).
//! The `run()` path is exercised indirectly by `embedder_supervisor_e2e`
//! integration tests in `trusty-search` (which spawn the binary).

pub mod batch_queue;
pub mod protocol;
pub mod stdio_server;
pub mod uds_server;

// Why (issue #250): the daemon's HTTP startup sequence (`run`, `run_with_args`,
// `AppState`, `health_handler`, `embed_handler`, and the `Args` clap struct
// that drives them) only compiles under the `http-server` feature, mirroring
// the `trusty-common` / `trusty-memory` rule that axum + tower-http are
// HTTP-server-only deps. The `protocol`, `batch_queue`, `stdio_server`, and
// `uds_server` modules stay unconditional — they have no axum surface.
#[cfg(feature = "http-server")]
use std::sync::Arc;
#[cfg(feature = "http-server")]
use std::time::Duration;

#[cfg(feature = "http-server")]
use anyhow::{bail, Context, Result};
#[cfg(feature = "http-server")]
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
#[cfg(feature = "http-server")]
use clap::Parser;
#[cfg(feature = "http-server")]
use serde_json::json;
#[cfg(feature = "http-server")]
use tokio::net::TcpListener;
#[cfg(feature = "http-server")]
use tokio::signal::unix::{signal, SignalKind};
#[cfg(feature = "http-server")]
use tower_http::trace::TraceLayer;
#[cfg(feature = "http-server")]
use tracing::info;
#[cfg(feature = "http-server")]
use trusty_common::embedder::{Embedder as _, FastEmbedder};
#[cfg(feature = "http-server")]
use trusty_common::embedder_client::{EmbedRequest, EmbedResponse};

#[cfg(feature = "http-server")]
use batch_queue::{BatchConfig, BatchQueue};

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for `trusty-embedderd`.
///
/// Why: clap derive is the workspace standard for all trusty-* binaries.
/// What: `--stdio` for the sidecar transport (piped stdin/stdout),
/// `--http` for the TCP listener, `--socket` for the UDS listener.
/// `--stdio` is mutually exclusive with `--http` and `--socket`.
/// `--batch-size` and `--batch-window-ms` configure the `BatchQueue`
/// coalescing window.
/// Test: `clap::Parser::try_parse_from` in unit tests.
#[cfg(feature = "http-server")]
#[derive(Parser, Debug)]
#[command(
    name = "trusty-embedderd",
    version,
    about = "Unified ONNX embedding daemon for trusty-tools (issue #164 consolidation)."
)]
pub struct Args {
    /// Run in stdio sidecar mode: read JSON-RPC requests from stdin,
    /// write responses to stdout. Mutually exclusive with --http and
    /// --socket. This is the transport used when trusty-search auto-spawns
    /// trusty-embedderd as a child process (issue #110 Phase 2 default).
    ///
    /// Why: avoids socket-file management — the parent owns the pipe handles
    /// and the child exits automatically when the parent closes its end.
    #[arg(long, conflicts_with_all = ["http_addr", "socket"])]
    pub stdio: bool,

    /// TCP address to listen on for HTTP (host:port).
    ///
    /// Why: configurable so CI / tests can bind to ephemeral ports and avoid
    /// collisions with a running production daemon. Pass an empty string to
    /// disable the HTTP listener (requires --socket or --stdio).
    #[arg(
        long = "http",
        default_value = "127.0.0.1:7890",
        env = "TRUSTY_EMBEDDERD_ADDR"
    )]
    pub http_addr: String,

    /// Path for the Unix domain socket.
    ///
    /// Why: provides a low-latency in-host transport for consumers that
    /// cannot reach the HTTP port or want sub-millisecond IPC. When omitted
    /// no UDS listener is started.
    #[arg(long, env = "TRUSTY_EMBEDDERD_SOCKET")]
    pub socket: Option<std::path::PathBuf>,

    /// Maximum number of texts in one ONNX batch.
    ///
    /// Why: caps the tensor size so the ONNX session doesn't run out of
    /// arena memory on hosts with constrained RAM.
    #[arg(
        long,
        default_value_t = batch_queue::DEFAULT_BATCH_SIZE,
        env = "TRUSTY_EMBED_BATCH_SIZE"
    )]
    pub batch_size: usize,

    /// Batching coalescing window in milliseconds.
    ///
    /// Why: the window lets concurrent callers pile up before the worker
    /// flushes, maximising ONNX throughput. 10 ms is imperceptible to users.
    #[arg(
        long,
        default_value_t = batch_queue::DEFAULT_BATCH_WINDOW_MS,
        env = "TRUSTY_EMBED_BATCH_WINDOW_MS"
    )]
    pub batch_window_ms: u64,
}

// ── HTTP state ───────────────────────────────────────────────────────────────

/// Shared application state passed to axum handlers.
///
/// Why: axum requires `Clone` on state; wrapping in `Arc` gives cheap clones.
/// What: holds the `BatchQueue` handle so every HTTP request is served through
/// the shared batching worker rather than calling the ONNX session directly.
/// Test: constructed in `run` after model load; exercised by handler tests.
#[cfg(feature = "http-server")]
#[derive(Clone)]
pub struct AppState {
    pub queue: Arc<BatchQueue>,
}

// ── Library entry point ──────────────────────────────────────────────────────

/// Parse `std::env::args()`, init tracing, load the model, and run the daemon.
///
/// Why: extracted from `main` so the same startup sequence can be invoked from
/// both the standalone `trusty-embedderd` binary and the bundled shim inside
/// `trusty-search`. Both binaries call `trusty_embedderd::run().await` and
/// rely on clap's standard argv parsing — no change to user-visible CLI
/// surface.
///
/// What: parse CLI args → validate flags → init tracing to stderr → load
/// `FastEmbedder` → spawn `BatchQueue` → dispatch to the selected transport:
///   - `--stdio`: run the stdio sidecar loop (exits on stdin EOF / SIGTERM)
///   - `--http` / `--socket`: bind listeners, wait for SIGTERM/SIGINT, clean up
///
/// Note: in `--stdio` mode stdout is reserved for JSON-RPC frames. All
/// tracing goes to stderr in every mode (MCP policy).
///
/// Test: `cargo run -p trusty-embedderd -- --http 127.0.0.1:7890` and verify
/// `curl http://127.0.0.1:7890/health`. For stdio mode:
/// `cargo run -p trusty-embedderd -- --stdio` (parent drives via pipes).
/// Covered indirectly by `embedder_supervisor_e2e` in `trusty-search`.
#[cfg(feature = "http-server")]
pub async fn run() -> Result<()> {
    let args = Args::parse();
    run_with_args(args).await
}

/// Inner entry point that accepts a pre-parsed `Args`.
///
/// Why: allows callers (including tests) to supply a specific `Args` struct
/// rather than relying on `std::env::args()`, which is process-global.
/// What: performs the full daemon startup sequence using the provided args.
/// Test: the public `run()` is tested indirectly via the supervisor e2e tests;
/// `run_with_args` can be unit-tested with controlled `Args` values.
#[cfg(feature = "http-server")]
pub async fn run_with_args(args: Args) -> Result<()> {
    // Init tracing to stderr — never stdout (MCP policy; stdout is used for
    // JSON-RPC frames in --stdio mode).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("trusty_embedderd=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .try_init();

    // Validate: at least one transport must be configured.
    let stdio_mode = args.stdio;
    let http_enabled = !stdio_mode && !args.http_addr.is_empty();
    let uds_enabled = !stdio_mode && args.socket.is_some();

    if !stdio_mode && !http_enabled && !uds_enabled {
        bail!("at least one of --stdio, --http, or --socket must be specified");
    }

    let config = BatchConfig {
        batch_size: args.batch_size.max(1),
        batch_window: Duration::from_millis(args.batch_window_ms),
    };

    if stdio_mode {
        info!(
            "trusty-embedderd starting (transport=stdio, batch_size={}, batch_window_ms={})",
            config.batch_size,
            config.batch_window.as_millis(),
        );
    } else {
        info!(
            "trusty-embedderd starting (http={:?}, socket={:?}, batch_size={}, batch_window_ms={})",
            if http_enabled {
                Some(&args.http_addr)
            } else {
                None
            },
            args.socket,
            config.batch_size,
            config.batch_window.as_millis(),
        );
    }

    // Load the ONNX model (expensive one-time init).
    info!("loading AllMiniLML6V2Q model...");
    let embedder = FastEmbedder::new()
        .await
        .context("failed to load AllMiniLML6V2Q model")?;
    let dim = embedder.dimension();
    info!("model loaded: dim={dim}");

    // Spawn the BatchQueue — it owns the embedder exclusively.
    let embedder: Arc<dyn trusty_common::embedder::Embedder> = Arc::new(embedder);
    let queue = Arc::new(BatchQueue::new(embedder, config));
    info!(
        "BatchQueue started (batch_size={}, window_ms={})",
        config.batch_size,
        config.batch_window.as_millis()
    );

    // ── Stdio sidecar mode ───────────────────────────────────────────────────
    // In stdio mode we own stdout exclusively for JSON-RPC frames. We do NOT
    // install signal handlers for SIGTERM/SIGINT here — the OS delivers EOF
    // on stdin when the parent exits, which is the clean termination signal.
    // We do handle SIGTERM so `kill` works from a shell.
    if stdio_mode {
        let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
        let q = Arc::clone(&queue);
        tokio::select! {
            result = stdio_server::run_stdio_server(q) => {
                if let Err(e) = result {
                    tracing::error!("stdio server error: {e:#}");
                    std::process::exit(1);
                }
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM — shutting down");
            }
        }
        return Ok(());
    }

    // ── HTTP / UDS listener mode ─────────────────────────────────────────────

    // Optionally bind the HTTP listener.
    if http_enabled {
        let state = AppState {
            queue: Arc::clone(&queue),
        };
        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/embed", post(embed_handler))
            .layer(TraceLayer::new_for_http())
            .with_state(state);

        let listener = TcpListener::bind(&args.http_addr)
            .await
            .with_context(|| format!("failed to bind HTTP to {}", args.http_addr))?;
        let local_addr = listener.local_addr()?;
        info!("trusty-embedderd HTTP listening on http://{local_addr}");

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("HTTP server error: {e:#}");
            }
        });
    }

    // Optionally bind the UDS listener.
    let socket_path_for_cleanup = args.socket.clone();
    if let Some(socket_path) = &args.socket {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create socket directory {}", parent.display()))?;
        }
        let listener = uds_server::bind_uds_listener(socket_path)
            .with_context(|| format!("bind UDS socket at {}", socket_path.display()))?;
        info!(
            "trusty-embedderd UDS listening at {}",
            socket_path.display()
        );
        let q = Arc::clone(&queue);
        tokio::spawn(uds_server::run_uds_accept_loop(listener, q));
    }

    // Wait for shutdown signal.
    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;

    tokio::select! {
        _ = sigterm.recv() => {
            info!("received SIGTERM — shutting down");
        }
        _ = sigint.recv() => {
            info!("received SIGINT — shutting down");
        }
    }

    // Remove the UDS socket file on clean exit so the next run starts fresh.
    if let Some(socket_path) = &socket_path_for_cleanup {
        if let Err(e) = std::fs::remove_file(socket_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("failed to remove UDS socket on shutdown: {e}");
            }
        }
    }

    Ok(())
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
///
/// Why: allows operators and trusty-search to verify the daemon is up and
/// serving requests before sending embedding work.
/// What: returns a static JSON body with `status`, `model`, and `dim` fields.
/// Test: `curl http://127.0.0.1:7890/health` returns HTTP 200.
#[cfg(feature = "http-server")]
pub async fn health_handler() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "model": "AllMiniLML6V2Q",
        "dim": trusty_common::embedder::EMBED_DIM,
    }))
}

/// `POST /embed` — embed a batch of texts via the shared `BatchQueue`.
///
/// Why: the core HTTP service endpoint; routes embedding requests through the
/// same `BatchQueue` as the UDS transport so the ONNX session is shared.
/// What: deserialises `EmbedRequest`, enqueues via `BatchQueue::embed_many`,
/// and returns `EmbedResponse`. On error returns HTTP 500 with a JSON body.
/// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
#[cfg(feature = "http-server")]
pub async fn embed_handler(
    State(state): State<AppState>,
    Json(req): Json<EmbedRequest>,
) -> Result<Json<EmbedResponse>, (StatusCode, Json<serde_json::Value>)> {
    let texts = req.texts;
    let n = texts.len();

    if n == 0 {
        return Ok(Json(EmbedResponse { vectors: vec![] }));
    }

    match state.queue.embed_many(texts).await {
        Ok(vectors) => {
            tracing::debug!(n, "embed_handler: batch complete");
            Ok(Json(EmbedResponse { vectors }))
        }
        Err(e) => {
            tracing::error!(error = %e, "embed_handler: embed_many failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("{e:#}") })),
            ))
        }
    }
}
