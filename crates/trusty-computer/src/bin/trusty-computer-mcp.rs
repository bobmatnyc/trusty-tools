//! `trusty-computer-mcp`: the stdio MCP server (Phase 0 stub).
//!
//! Why: the second bin of plan section 2, present so signing and packaging
//! cover both from the start.
//! What: prints usage and exits 2; the 20 `computer_*` tools arrive in Phase 5.
//! Test: none yet; the `tools/list` schema test comes in Phase 5.

use std::process::ExitCode;

const USAGE: &str = "\
usage: trusty-computer-mcp

Speaks MCP JSON-RPC on stdio and forwards to the trusty-computer broker.
Not implemented yet (issue #11460, Phase 5).";

fn main() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
