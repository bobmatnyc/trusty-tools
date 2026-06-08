//! trusty-console — web console for trusty services.
//!
//! Why: Operators need a single browser page that shows the runtime state of
//! every trusty service on their machine. P0 implements detection + home cards;
//! later phases add service-specific tabs and MCP integration.
//! What: Parses `serve` subcommand, starts the axum HTTP server, optionally
//! opens the browser. All logs go to stderr.
//! Test: `cargo test -p trusty-console` covers detection and server routes.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;

mod connector;
mod detect;
mod server;

// ─── CLI ─────────────────────────────────────────────────────────────────────

/// trusty-console: web dashboard for trusty services.
///
/// Why: Provides a single entry point for all console subcommands so future
/// phases (status, doctor, open) can be added without breaking existing usage.
/// What: Parses top-level arguments and delegates to subcommand handlers.
/// Test: `cargo run -p trusty-console -- serve --help` must succeed.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-console",
    version,
    about = "Web dashboard for trusty services"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available subcommands.
///
/// Why: P0 only has `serve`; future phases add `status` (CLI-only) etc.
/// What: Clap enum; each variant carries its own args.
/// Test: Subcommand selection tested via `Cli::parse_from`.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the HTTP server and serve the console dashboard.
    Serve(ServeArgs),
}

/// Arguments for `trusty-console serve`.
///
/// Why: The bind address must be configurable so users can change the port when
/// 7788 is taken; `--open` is a convenience for developers.
/// What: Optional `--http` (default `127.0.0.1:7788`) and `--open` flag.
/// Test: Default address tested in `test_serve_args_defaults` below.
#[derive(Debug, Parser)]
struct ServeArgs {
    /// Address to listen on (default: 127.0.0.1:7788).
    #[arg(long, default_value = "127.0.0.1:7788")]
    http: String,

    /// Open the console in the default browser after starting.
    #[arg(long, default_value_t = false)]
    open: bool,
}

// ─── entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Init tracing — always to stderr so stdout stays clean.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve(args) => run_serve(args).await,
    }
}

/// Run the `serve` subcommand.
///
/// Why: Separating the serve logic from `main` keeps main() thin and allows
/// this function to be called from integration tests.
/// What: Builds the router, binds the TCP listener, optionally opens a browser,
/// then serves until SIGTERM/SIGINT.
/// Test: Server integration tests in `server.rs` cover the router directly
/// without exercising this function (to avoid real TCP binding in unit tests).
async fn run_serve(args: ServeArgs) -> Result<()> {
    let connectors = detect::all_connectors();
    let state = server::AppState::new(connectors);
    let router = server::build_router(state);

    let listener = tokio::net::TcpListener::bind(&args.http)
        .await
        .with_context(|| format!("failed to bind {}", args.http))?;

    let addr = listener.local_addr().context("get local addr")?;
    info!("trusty-console listening on http://{addr}");

    let console_url = format!("http://{addr}");
    eprintln!("trusty-console: {console_url}");

    if args.open {
        // Best-effort browser open; ignore errors.
        let _ = open::that(&console_url);
    }

    axum::serve(listener, router)
        .await
        .context("server error")?;

    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: default http address must be 127.0.0.1:7788.
    /// What: parses `serve` with no flags and checks the default.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_defaults() {
        let cli = Cli::parse_from(["trusty-console", "serve"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "127.0.0.1:7788");
                assert!(!args.open);
            }
        }
    }

    /// Why: custom --http flag must override the default.
    /// What: parses `serve --http 0.0.0.0:9000`.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_custom_http() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--http", "0.0.0.0:9000"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "0.0.0.0:9000");
            }
        }
    }
}
