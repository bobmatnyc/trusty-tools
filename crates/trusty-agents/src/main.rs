//! Thin `tagent` binary wrapper (trusty-agents).
//!
//! Why: The entire startup pipeline (argv parsing, env load, tracing init,
//!      subcommand dispatch, REPL/CTRL fallback) lives in the library at
//!      `trusty_agents::runtime::run`. Hosting it in the library lets private
//!      launchers (`trusty-agents-local`) install additional agent plugins via
//!      `trusty_agents::install_plugins(...)` BEFORE invoking `run()`, without
//!      polluting the crate with references to `publish = false` agent crates
//!      such as `cto-assistant`.
//! What: Standard `#[tokio::main]` entry point that delegates straight to
//!       `trusty_agents::run()`. No additional setup, no plugin wiring — the
//!       binary ships with an empty plugin registry by default.
//! Test: `cargo run -p trusty-agents -- --version` prints the build banner.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    trusty_agents::run().await
}
