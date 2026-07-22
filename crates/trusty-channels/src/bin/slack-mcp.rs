//! `slack-mcp` binary — stdio MCP server.
//!
//! Why: Claude Code (and other MCP hosts) launch this process per session and
//! talk to it over stdin/stdout JSON-RPC.
//! What: Initialises tracing (to stderr, keeping stdout clean for JSON-RPC
//! framing), builds an `AppState` with a shared `BaseClient`, then runs the
//! stdio loop until EOF. All 19 tools listed in
//! `trusty_channels::slack::tools::TOOL_NAMES` are live — send/read/list
//! (issue #2639), search + reactions (issue #2640), and the
//! claude.ai-connector-parity batch added by epic #3611 (issues
//! #3612-#3618) all make real Slack Web API calls through `BaseClient`. See
//! `crates/trusty-channels/README.md` for the full tool table and required
//! OAuth scopes.
//! Test: Manual via `claude mcp add` / direct stdin piping; the tool bodies
//! themselves are covered by `cargo test -p trusty-channels`
//! (`tests/tools_http.rs`).

use std::sync::Arc;

use trusty_channels::slack::api::client::BaseClient;
use trusty_channels::slack::server::{run_stdio, AppState};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    trusty_common::init_tracing(0);
    let client = BaseClient::new()?;
    let state = AppState {
        client: Arc::new(client),
    };
    run_stdio(state).await
}
