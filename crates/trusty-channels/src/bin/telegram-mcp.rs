//! `telegram-mcp` binary — stdio MCP server (scaffold).
//!
//! Why: Claude Code (and other MCP hosts) launch this process per session and
//! talk to it over stdin/stdout JSON-RPC.
//! What: Initialises tracing (to stderr, keeping stdout clean for JSON-RPC
//! framing), builds an `AppState` with a shared `BaseClient`, then runs the
//! stdio loop until EOF. Tool calls currently return a `not-yet-implemented`
//! MCP error — live Telegram Bot API calls are deferred (see issue #2641).
//! Test: Manual via `claude mcp add` / direct stdin piping.

use std::sync::Arc;

use trusty_channels::telegram::api::client::BaseClient;
use trusty_channels::telegram::server::{run_stdio, AppState};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    trusty_common::init_tracing(0);
    let client = BaseClient::new()?;
    let state = AppState {
        client: Arc::new(client),
    };
    run_stdio(state).await
}
