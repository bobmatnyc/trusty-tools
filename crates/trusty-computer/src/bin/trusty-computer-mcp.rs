//! `trusty-computer-mcp`: the stdio MCP server in front of the broker.
//!
//! Why: the second bin of plan section 2; agents load it as an MCP server.
//! What: `trusty-computer-mcp [--state DIR]` (default `~/.trusty/computer`)
//! serves the 20 `computer_*` tools of `trusty_computer::mcp` on stdio and
//! forwards each to `<state>/agent.sock`. Logs go to stderr only.
//! Test: `mcp_smoke_against_a_fake_broker` in `tests/mcp_server.rs`.

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use trusty_computer::{mcp::McpServer, operator_cli::default_state};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let state = match args.as_slice() {
        [] => default_state(),
        [flag, dir] if flag == "--state" => PathBuf::from(dir),
        _ => {
            eprintln!("usage: trusty-computer-mcp [--state DIR]");
            return ExitCode::from(2);
        }
    };
    let stdin = io::stdin();
    match McpServer::new(state).serve(BufReader::new(stdin.lock()), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trusty-computer-mcp: {e}");
            ExitCode::from(1)
        }
    }
}
