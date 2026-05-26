//! `trusty-embedderd` — standalone ONNX embedding daemon (issue #110 Phase 1,
//! consolidated in issue #164).
//!
//! Why: running the ONNX model in a dedicated process decouples it from the
//! trusty-search daemon — a crash or OOM in one doesn't affect the other, and
//! the model stays resident across search-daemon restarts. The daemon exposes
//! both a JSON-over-HTTP API (for cross-host / firewall-friendly access) and
//! a JSON-RPC 2.0 over UDS interface (zero-TCP-overhead on-host access).
//!
//! What: loads `AllMiniLML6V2Q` once at startup, wraps it in a `BatchQueue`
//! to coalesce concurrent requests, then serves:
//!
//!   HTTP (--http <addr>, default 127.0.0.1:7890):
//!     - `GET  /health` → `{"status":"ok","model":"AllMiniLML6V2Q","dim":384}`
//!     - `POST /embed`  → `EmbedRequest` → `EmbedResponse`
//!
//!   UDS (--socket <path>, optional):
//!     - newline-framed JSON-RPC 2.0, method `"embed"`, same semantics as HTTP
//!
//! All logs go to stderr (MCP policy — stdout is never written to).
//!
//! Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
//!       `cargo test -p trusty-embedderd --test uds_integration -- --include-ignored`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde_json::json;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;
use trusty_common::embedder::{Embedder as _, FastEmbedder};
use trusty_common::embedder_client::{EmbedRequest, EmbedResponse};

mod batch_queue;
mod uds_server;

use batch_queue::{BatchConfig, BatchQueue, DEFAULT_BATCH_SIZE, DEFAULT_BATCH_WINDOW_MS};

/// CLI arguments for `trusty-embedderd`.
///
/// Why: `clap` derive is the project standard for all trusty-* binaries.
/// Expanding from Phase 1 (HTTP only) to also support UDS and batch tuning
/// keeps the operator surface consistent with the retired `trusty-embed-daemon`.
///
/// What: HTTP address, optional UDS socket path, batch size/window tuning,
/// and a verbosity knob.
///
/// Test: `args_parse_defaults` and related tests in this module.
#[derive(Parser, Debug)]
#[command(
    name = "trusty-embedderd",
    version,
    about = "Standalone ONNX embedding daemon for trusty-tools (issue #110)."
)]
struct Args {
    /// TCP address to listen on (host:port).
    ///
    /// Why: configurable so CI / tests can bind to an ephemeral port and
    /// avoid collisions with a running production daemon.
    #[arg(long, default_value = "127.0.0.1:7890", env = "TRUSTY_EMBEDDERD_ADDR")]
    http: String,

    /// Path for the Unix domain socket (optional).
    ///
    /// Why: UDS transport is lower-latency than HTTP for on-host callers.
    /// When set the daemon binds both the HTTP listener and the UDS socket,
    /// sharing the same `BatchQueue` and `FastEmbedder`.
    #[arg(long, env = "TRUSTY_EMBEDDERD_SOCKET")]
    socket: Option<PathBuf>,

    /// Maximum number of texts in one ONNX batch.
    ///
    /// Why: caps tensor allocation size; 32 is the empirical sweet spot.
    #[arg(long, default_value_t = DEFAULT_BATCH_SIZE, env = "TRUSTY_EMBEDDERD_BATCH_SIZE")]
    batch_size: usize,

    /// Batching window in milliseconds.
    ///
    /// Why: the window allows nearly-simultaneous arrivals to be coalesced
    /// into a single ONNX call; 10 ms is imperceptible to human-facing queries.
    #[arg(long, default_value_t = DEFAULT_BATCH_WINDOW_MS, env = "TRUSTY_EMBEDDERD_BATCH_WINDOW_MS")]
    batch_window_ms: u64,

    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// Shared application state passed to axum handlers.
///
/// Why: axum requires `Clone` on state; `BatchQueue` is cheaply cloneable via
/// its internal `Arc<mpsc::Sender>`, so this does not duplicate the model.
///
/// What: holds a `BatchQueue` handle. All HTTP handlers route through it so
/// the UDS accept loop and the HTTP server share the single ONNX session.
///
/// Test: constructed in `main` after model load; exercised by all handler tests.
#[derive(Clone)]
struct AppState {
    queue: Arc<BatchQueue>,
}

/// Entry point.
///
/// Why: load the ONNX model once (expensive), wrap it in a `BatchQueue`,
/// then serve both HTTP and optionally UDS until interrupted.
///
/// What: parse CLI args → init tracing → load `FastEmbedder` → spawn
/// `BatchQueue` worker → bind HTTP listener → optionally bind UDS listener
/// → serve both concurrently.
///
/// Test: `cargo run -p trusty-embedderd -- --http 127.0.0.1:7890` and verify
/// `curl http://127.0.0.1:7890/health` returns `{"status":"ok",...}`.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    trusty_common::init_tracing(args.verbose);

    info!(
        "trusty-embedderd starting (http={}, batch_size={}, batch_window_ms={})",
        args.http, args.batch_size, args.batch_window_ms
    );

    let batch_config = BatchConfig {
        batch_size: args.batch_size.max(1),
        batch_window: Duration::from_millis(args.batch_window_ms),
    };

    // Load the ONNX model. This is the expensive one-time init.
    info!("loading AllMiniLML6V2Q model...");
    let embedder = FastEmbedder::new()
        .await
        .context("failed to load AllMiniLML6V2Q model")?;
    let dim = embedder.dimension();
    info!("model loaded: dim={dim}");

    // Wrap the embedder in the BatchQueue. All requests (HTTP and UDS) share
    // this single queue so the ONNX session has exactly one caller at a time.
    let embedder_dyn: Arc<dyn trusty_common::embedder::Embedder> = Arc::new(embedder);
    let queue = Arc::new(BatchQueue::new(embedder_dyn, batch_config));

    let state = AppState {
        queue: Arc::clone(&queue),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/embed", post(embed_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Bind the HTTP listener.
    let listener = TcpListener::bind(&args.http)
        .await
        .with_context(|| format!("failed to bind to {}", args.http))?;
    let local_addr = listener.local_addr()?;
    info!("trusty-embedderd HTTP listening on http://{local_addr}");

    // Optionally bind the UDS listener alongside the HTTP server.
    if let Some(ref socket_path) = args.socket {
        info!(
            "trusty-embedderd UDS listening at {}",
            socket_path.display()
        );
        uds_server::cleanup_stale_socket(socket_path);
        let uds_listener = uds_server::bind_uds_listener(socket_path)
            .with_context(|| format!("bind UDS at {}", socket_path.display()))?;
        let uds_queue = Arc::clone(&queue);

        // Spawn the UDS accept loop as a detached task. Both the HTTP server
        // and the UDS loop share the same `queue`; Tokio's scheduler keeps them
        // running concurrently on the same thread pool.
        tokio::spawn(async move {
            uds_server::run_uds_accept_loop(uds_listener, uds_queue).await;
        });
    }

    axum::serve(listener, app)
        .await
        .context("HTTP server error")?;

    Ok(())
}

/// `GET /health` — liveness probe.
///
/// Why: allows operators and trusty-search to verify the daemon is up and
/// serving requests before sending embedding work.
///
/// What: returns a static JSON body with `status`, `model`, and `dim` fields.
///
/// Test: `curl http://127.0.0.1:7890/health` returns HTTP 200 with
/// `{"status":"ok","model":"AllMiniLML6V2Q","dim":384}`.
async fn health_handler() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "model": "AllMiniLML6V2Q",
        "dim": trusty_common::embedder::EMBED_DIM,
    }))
}

/// `POST /embed` — embed a batch of texts via the shared `BatchQueue`.
///
/// Why: routing through the `BatchQueue` means concurrent HTTP and UDS
/// requests are coalesced into the same ONNX call, halving model RSS
/// compared to running two separate daemons.
///
/// What: deserialises `EmbedRequest`, calls `BatchQueue::embed_many`, and
/// returns `EmbedResponse`. On model/queue error returns HTTP 500 with a
/// JSON error body so callers can distinguish transport failures from model
/// failures.
///
/// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
async fn embed_handler(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parse_defaults() {
        // Why: guard against accidental default value changes that would
        // break existing deployments.
        // What: parse with no arguments and assert field values.
        // Test: this test.
        let args = Args::try_parse_from(["trusty-embedderd"]).unwrap();
        assert_eq!(args.http, "127.0.0.1:7890");
        assert!(args.socket.is_none());
        assert_eq!(args.batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(args.batch_window_ms, DEFAULT_BATCH_WINDOW_MS);
    }

    #[test]
    fn args_parse_socket_flag() {
        // Why: the --socket flag is the primary new surface for UDS consumers.
        // What: parse with --socket and assert the path is captured.
        // Test: this test.
        let args =
            Args::try_parse_from(["trusty-embedderd", "--socket", "/tmp/test.sock"]).unwrap();
        assert_eq!(
            args.socket.as_ref().and_then(|p| p.to_str()),
            Some("/tmp/test.sock")
        );
    }

    #[test]
    fn args_parse_batch_flags() {
        // Why: batch tuning flags must override defaults.
        // What: parse with both batch flags and assert overrides.
        // Test: this test.
        let args = Args::try_parse_from([
            "trusty-embedderd",
            "--batch-size",
            "64",
            "--batch-window-ms",
            "5",
        ])
        .unwrap();
        assert_eq!(args.batch_size, 64);
        assert_eq!(args.batch_window_ms, 5);
    }
}
