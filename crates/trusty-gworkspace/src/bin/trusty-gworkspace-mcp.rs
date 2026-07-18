//! `trusty-gworkspace-mcp` binary — stdio MCP server + OAuth onboarding CLI.
//!
//! Why: MCP hosts (Claude Code and friends) launch this process with NO
//! arguments and talk JSON-RPC over stdin/stdout, so that path must stay the
//! default. Issue #2631 adds `setup` / `doctor` / `accounts` subcommands for
//! native onboarding without breaking the argument-less server invocation.
//! Issue #2644 renamed the binary from `gworkspace-mcp` to
//! `trusty-gworkspace-mcp` to eliminate a `$PATH` collision with the legacy
//! pipx-installed Python `gworkspace-mcp` package — the two implementations
//! previously shared one name on `$PATH`, so which binary launched depended
//! on install order.
//! What: Parses args with clap; a present subcommand dispatches to the CLI
//! (and exits), while no subcommand runs the stdio MCP server exactly as
//! before.
//! Test: CLI parsing is unit-tested in `cli`; the server path is exercised
//! manually via `claude mcp add` / stdin piping.

use std::sync::Arc;

use clap::Parser;

use trusty_gworkspace::api::client::BaseClient;
use trusty_gworkspace::cli::{self, Cli};
use trusty_gworkspace::server::{AppState, run_stdio};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    trusty_common::init_tracing(0);

    let cli = Cli::parse();
    if let Some(command) = cli.command {
        return cli::dispatch(command).await;
    }

    // No subcommand: preserve legacy behavior — run the stdio MCP server.
    let client = BaseClient::new()?;
    let state = AppState {
        client: Arc::new(client),
    };
    run_stdio(state).await
}
