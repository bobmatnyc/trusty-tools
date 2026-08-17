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

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    trusty_agents::run().await
}
