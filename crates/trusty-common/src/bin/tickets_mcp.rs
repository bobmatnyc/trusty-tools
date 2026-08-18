//! `tickets-mcp` binary — stdio MCP server for the trusty-tickets surface.
//!
//! Why: Claude Code (and other MCP hosts) launch this process per session
//! and talk to it over stdin/stdout JSON-RPC 2.0. This binary was formerly
//! the `tickets-mcp` binary in the standalone `trusty-tickets` crate; it is
//! now shipped as part of `trusty-common` behind the `tickets` feature.
//! What: Initialises tracing, loads config + builds a `BackendClient`,
//! and runs the stdio loop until EOF.
//! Test: Manual via `claude mcp add` / piping JSON-RPC to stdin.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use std::sync::Arc;

use trusty_common::tickets::api::client::BackendClient;
use trusty_common::tickets::api::config::Config;
use trusty_common::tickets::server::{AppState, run_stdio};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    trusty_common::init_tracing(0);
    let config = Config::load()?;
    let client = BackendClient::from_config(config).await?;
    let state = AppState {
        client: Arc::new(client),
    };
    run_stdio(state).await
}
