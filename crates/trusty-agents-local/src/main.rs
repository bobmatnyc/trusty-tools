//! Why: `trusty-agents` cannot depend on private (publish=false) agent
//!      crates. This private binary wires local agents into the trusty-agents
//!      plugin registry before starting the server, giving a full-featured
//!      local build without polluting the published crate.
//! What: Installs the cto-assistant plugin, then delegates to trusty_agents::run().
//! Test: `cargo run -p trusty-agents-local` starts the agent server with CTO tools.

use anyhow::{Result, anyhow};

#[tokio::main]
async fn main() -> Result<()> {
    // Why: `install_plugins` writes into a process-wide OnceLock; calling it
    //      twice (e.g. from a re-entrant test) returns the rejected plugin
    //      list. We surface that as an error rather than silently dropping
    //      the plugins so misconfiguration is loud.
    // What: Registers the cto-assistant persona before `run()` so the ctrl
    //       loop sees its `AgentPlugin` when it builds tool surfaces.
    // Test: Indirectly covered by running `trusty-agents-local` and confirming
    //       the cto-assistant persona exposes its CTO DB tools.
    trusty_agents::install_plugins(vec![cto_assistant::agent_plugin()])
        .map_err(|_| anyhow!("install_plugins called more than once"))?;
    trusty_agents::run().await
}
