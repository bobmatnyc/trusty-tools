//! Handler for the `serve` subcommand (HTTP and MCP stdio modes).
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//! The `--stdio` flag runs the MCP stdio JSON-RPC loop; without it the standard
//! axum HTTP daemon starts on the configured port.
//!
//! What: builds `AppState` (LLM + verifier + search + analyze + dedup store),
//! then either calls `serve_http` (HTTP mode) or `mcp::run` (stdio mode).
//!
//! Test: `cargo run -p trusty-review --features http-server -- serve --help`
//! must exit 0; endpoint tests live in `service::handlers` and
//! `service::webhook`; MCP dispatch covered by `mcp::tests`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use tracing::{info, warn};

use trusty_review::{
    config::ReviewConfig,
    integrations::{
        search_client::HttpSearchClient, subprocess_analyze_client::SubprocessAnalyzeClient,
    },
    llm::build_provider,
    pipeline::enforce_verifier_liveness,
    service::{AppState, DEFAULT_PORT, serve as serve_http},
};

use crate::cli_verify;

// ─── serve args ──────────────────────────────────────────────────────────────

/// Arguments for the `serve` subcommand.
///
/// Why: collects the port, bind address, and mode flags so the server can be
/// configured purely from CLI flags without requiring env-var wrangling.
/// What: `--port` sets the listen port (default 7880); `--stdio` activates the
/// MCP JSON-RPC stdio loop instead of binding TCP.
/// Test: `cargo run -p trusty-review --features http-server -- serve --help`.
#[derive(Debug, clap::Parser)]
pub struct ServeArgs {
    /// HTTP listen port.
    /// Default: 7880 (distinct from trusty-search :7878 and trusty-analyze :7879).
    /// Ignored when --stdio is set.
    #[arg(long, default_value_t = DEFAULT_PORT, value_name = "PORT")]
    pub port: u16,

    /// Bind address (default: 127.0.0.1).
    /// Ignored when --stdio is set.
    #[arg(long, default_value = "127.0.0.1", value_name = "ADDR")]
    pub bind: String,

    /// Run as a JSON-RPC 2.0 / MCP stdio service instead of binding TCP.
    ///
    /// In this mode stdout is the JSON-RPC transport; all logs go to stderr.
    /// Wire into Claude Code via .mcp.json:
    ///   { "mcpServers": { "trusty-review": { "command": "trusty-review",
    ///                                        "args": ["serve", "--stdio"] } } }
    #[cfg(feature = "mcp")]
    #[arg(long, default_value_t = false)]
    pub stdio: bool,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `serve` subcommand.
///
/// Why: the HTTP and MCP stdio daemon modes share the same dependency-building
/// logic; only the final transport differs.
/// What: builds `AppState`, then either calls `serve_http` (TCP mode) or
/// `mcp::run` (stdio mode).  All logs go to stderr; stdout stays clean.
/// Test: see module doc.
pub async fn cmd_serve(config: ReviewConfig, args: ServeArgs) -> Result<()> {
    // Build deps shared between both modes.
    let state = build_app_state(config.clone()).await?;

    #[cfg(feature = "mcp")]
    if args.stdio {
        info!("trusty-review MCP stdio service starting");
        return trusty_review::mcp::run(state).await;
    }

    // HTTP mode.
    use std::net::SocketAddr;
    let addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", args.bind, args.port))?;

    info!(
        port = args.port,
        bind = %args.bind,
        reviewer_model = %config.role_models.reviewer.model,
        dry_run = config.dry_run,
        "trusty-review serve starting"
    );

    serve_http(state, addr).await
}

// ─── dep builder ─────────────────────────────────────────────────────────────

/// Build the shared `AppState` used by both HTTP and MCP stdio modes.
///
/// Why: both modes need the same set of deps; building them once avoids
/// repetition.  Async because `BedrockProvider::new` loads AWS credentials
/// asynchronously.
/// What: builds the reviewer and verifier LLM providers, HTTP search/analyze
/// clients, opens the durable dedup store, and wraps everything in `AppState`.
/// Test: covered transitively by handler unit tests that inject fakes.
async fn build_app_state(config: ReviewConfig) -> Result<AppState> {
    let reviewer_model = config.role_models.reviewer.model.clone();
    let default_provider = config.role_models.reviewer.provider.clone();
    let llm = build_provider(
        &reviewer_model,
        &default_provider,
        &config.openrouter_api_key,
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let verifier = cli_verify::build_verifier_for_serve(&config).await?;
    enforce_verifier_liveness(&config, verifier.as_ref())
        .await
        .map_err(|reason| anyhow::anyhow!(reason))?;

    let search = HttpSearchClient::from_config(&config);
    // Use the on-demand subprocess client instead of the HTTP daemon client.
    // Rationale: #632 — trusty-analyze is invoked on demand as a subprocess
    // (trusty-analyze review --index-id <id> -) rather than requiring a
    // long-running trusty-analyze serve daemon.
    let analyze = SubprocessAnalyzeClient::from_config(&config);

    let dedup_path = config.log_dir.join("dedup.redb");
    let dedup = match trusty_review::store::DedupStore::open(&dedup_path) {
        Ok(store) => {
            info!(path = %dedup_path.display(), "dedup store opened");
            Some(Arc::new(store))
        }
        Err(e) => {
            warn!(
                path = %dedup_path.display(),
                "failed to open dedup store (continuing without it): {e}"
            );
            None
        }
    };

    Ok(AppState::with_verifier_and_dedup(
        config,
        llm,
        verifier,
        Arc::new(search),
        Some(Arc::new(analyze)),
        dedup,
    ))
}
