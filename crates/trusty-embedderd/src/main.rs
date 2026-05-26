//! `trusty-embedderd` — standalone ONNX embedding daemon (issue #110 Phase 1).
//!
//! Why: running the ONNX model in a dedicated process decouples it from the
//! trusty-search daemon — a crash or OOM in one doesn't affect the other, and
//! the model stays resident across search-daemon restarts. The daemon exposes a
//! simple JSON-over-HTTP API so any trusty-* consumer can embed texts without
//! depending on the ONNX runtime directly.
//!
//! What: loads `AllMiniLML6V2Q` once at startup, then serves:
//!
//!   - `GET /health` → `{"status":"ok","model":"AllMiniLML6V2Q","dim":384}`
//!   - `POST /embed`  → `EmbedRequest` → `EmbedResponse`
//!
//! Listens on `--http <addr>` (default `127.0.0.1:7890`).
//! All logs go to stderr (MCP policy — stdout is never written to).
//!
//! Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`

use std::sync::Arc;

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
use trusty_embedder_client::{EmbedRequest, EmbedResponse};

/// CLI arguments for `trusty-embedderd`.
///
/// Why: `clap` derive is the project standard for all trusty-* binaries.
///
/// What: only `--http` for Phase 1; later phases will add `--socket`.
///
/// Test: `clap::Parser::try_parse_from` in unit tests.
#[derive(Parser, Debug)]
#[command(
    name = "trusty-embedderd",
    version,
    about = "Standalone ONNX embedding daemon for trusty-tools (issue #110 Phase 1)."
)]
struct Args {
    /// TCP address to listen on (host:port).
    ///
    /// Why: configurable so CI / tests can bind to an ephemeral port and
    /// avoid collisions with a running production daemon.
    #[arg(long, default_value = "127.0.0.1:7890", env = "TRUSTY_EMBEDDERD_ADDR")]
    http: String,
}

/// Shared application state passed to axum handlers.
///
/// Why: axum requires `Clone` on state; wrapping in `Arc` gives cheap clones
/// without copying the (large) model structure.
///
/// What: holds the loaded `FastEmbedder` so every request can call
/// `embed_batch` without re-loading the model.
///
/// Test: constructed in `main` after model load; indirectly exercised by all
/// handler tests.
#[derive(Clone)]
struct AppState {
    embedder: Arc<FastEmbedder>,
}

/// Entry point.
///
/// Why: load the ONNX model once (expensive), then serve requests until
/// interrupted. All output is on stderr to keep stdout clean for any future
/// MCP framing.
///
/// What: parse CLI args → init tracing → load `FastEmbedder` → build axum
/// router → bind TCP → serve.
///
/// Test: `cargo run -p trusty-embedderd -- --http 127.0.0.1:7890` and verify
/// `curl http://127.0.0.1:7890/health` returns `{"status":"ok",...}`.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Init tracing to stderr — never stdout (MCP policy).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("trusty_embedderd=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    info!("trusty-embedderd starting, addr={}", args.http);

    // Load the ONNX model. This is the expensive one-time init.
    info!("loading AllMiniLML6V2Q model...");
    let embedder = FastEmbedder::new()
        .await
        .context("failed to load AllMiniLML6V2Q model")?;
    let dim = embedder.dimension();
    info!("model loaded: dim={dim}");

    let state = AppState {
        embedder: Arc::new(embedder),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/embed", post(embed_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(&args.http)
        .await
        .with_context(|| format!("failed to bind to {}", args.http))?;
    let local_addr = listener.local_addr()?;
    info!("trusty-embedderd listening on http://{local_addr}");

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

/// `POST /embed` — embed a batch of texts.
///
/// Why: the core service endpoint; receives texts from trusty-search (or any
/// consumer) and returns vectors produced by the ONNX model.
///
/// What: deserialises `EmbedRequest`, calls `FastEmbedder::embed_batch`, and
/// returns `EmbedResponse`. On model error returns HTTP 500 with a JSON error
/// body so callers can distinguish transport failures from model failures.
///
/// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
async fn embed_handler(
    State(state): State<AppState>,
    Json(req): Json<EmbedRequest>,
) -> Result<Json<EmbedResponse>, (StatusCode, Json<serde_json::Value>)> {
    use trusty_common::embedder::Embedder as _;

    let texts = req.texts;
    let n = texts.len();

    if n == 0 {
        return Ok(Json(EmbedResponse { vectors: vec![] }));
    }

    match state.embedder.embed_batch(&texts).await {
        Ok(vectors) => {
            tracing::debug!(n, "embed_handler: batch complete");
            Ok(Json(EmbedResponse { vectors }))
        }
        Err(e) => {
            tracing::error!(error = %e, "embed_handler: embed_batch failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("{e:#}") })),
            ))
        }
    }
}
