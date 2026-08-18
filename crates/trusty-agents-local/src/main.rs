//! Why: architecture-review tranche 0 (item 4) severed this launcher's only
//!      plugin-install edge, and #3732 then dissolved the crate it pointed at.
//!      Agent-specific tools are now declared, not compiled in — the CTO DB
//!      queries this binary once wired in as a Rust `AgentPlugin` are reachable
//!      through the declarative `cto-db` Python skill instead. So the launcher
//!      has nothing left to install.
//! What: Thin pass-through launcher — delegates straight to
//!       `trusty_agents::run_to_completion()` (which owns the tokio runtime)
//!       with no local plugin installation.
//!
//! #3655: this used to be `#[tokio::main]` over `run()`. That attribute drops
//! the runtime with an unbounded wait on the blocking pool, so one background
//! task stuck in a syscall made the process un-exitable. The shared launcher
//! bounds that wait for both this binary and `tagent`.
//! Test: `cargo run -p trusty-agents-local` starts the agent server; agent
//!       tools come from the bundled agent packages, not from this binary.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::Result;

fn main() -> Result<()> {
    trusty_agents::run_to_completion()
}
