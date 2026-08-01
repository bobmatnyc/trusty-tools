//! Why: architecture-review tranche 0 (item 4) severed this launcher's only
//!      plugin-install edge, and #3732 then dissolved the crate it pointed at.
//!      Agent-specific tools are now declared, not compiled in — the CTO DB
//!      queries this binary once wired in as a Rust `AgentPlugin` are reachable
//!      through the declarative `cto-db` Python skill instead. So the launcher
//!      has nothing left to install.
//! What: Thin pass-through launcher — delegates straight to
//!       `trusty_agents::run()` with no local plugin installation.
//! Test: `cargo run -p trusty-agents-local` starts the agent server; agent
//!       tools come from the bundled agent packages, not from this binary.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    trusty_agents::run().await
}
