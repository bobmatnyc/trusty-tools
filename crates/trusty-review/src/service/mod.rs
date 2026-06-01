//! HTTP service layer for trusty-review — axum router and shared state.
//!
//! Why: wraps the existing review pipeline in a long-lived HTTP daemon so
//! trusty-review can receive GitHub webhooks and respond to on-demand review
//! requests without requiring the caller to spawn a CLI process.
//!
//! What: exports `AppState`, `build_router`, and the `serve` entry-point.
//! All axum handler logic lives in focused sibling files:
//!   - `handlers.rs`  — route handler functions
//!   - `webhook.rs`   — GitHub webhook event parsing + dispatch
//!
//! Test: `cargo test -p trusty-review --features http-server` exercises the
//! router via `tower::ServiceExt::oneshot` without a bound socket.
//!
//! Feature gate: the entire module is compiled only under `http-server`.

pub mod handlers;
pub mod webhook;

pub use handlers::AppState;

use std::net::SocketAddr;

use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::service::handlers::{handle_health, handle_review, handle_status};
use crate::service::webhook::handle_github_webhook;

/// Default listen port for the trusty-review daemon.
///
/// Why: must be distinct from sibling daemons (7878 = search, 7879 = analyze).
/// What: 7880 per spec REV-803.
/// Test: `serve_help_shows_default_port` checks the CLI default.
pub const DEFAULT_PORT: u16 = 7880;

/// Build the axum router for trusty-review.
///
/// Why: separating router construction from `serve` lets tests call
/// `build_router(state).oneshot(request)` without binding a socket.
/// What: registers GET /health, GET /status, POST /review, and
/// POST /pr/github/webhook; attaches a tracing layer.
/// Test: `router_builds_without_panic`, `health_returns_ok_json`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/status", get(handle_status))
        .route("/review", post(handle_review))
        .route("/pr/github/webhook", post(handle_github_webhook))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind the axum server and run until SIGTERM/SIGINT.
///
/// Why: the `serve` CLI subcommand needs a single async entry-point that
/// builds state, binds the socket, and runs the graceful-shutdown loop
/// following the workspace's connection-safe daemon convention (issue #534).
/// What: calls `build_router`, binds `addr`, logs the listening address to
/// stderr, then awaits `axum::serve` with `trusty_common::shutdown_signal`.
/// Test: covered by the `serve --help` smoke-test; full integration would
/// require a network round-trip (out of scope for unit tests).
pub async fn serve(state: AppState, addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    // Log to stderr — stdout must stay clean (spec REV-722, CLAUDE.md).
    info!("trusty-review listening on http://{actual}");
    eprintln!("trusty-review: listening on http://{actual}");
    let app = build_router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await?;
    Ok(())
}
