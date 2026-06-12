//! `trusty-memory-mcp-bridge` — DEPRECATED compatibility shim.
//!
//! Why: the #914 PR3 epic (commit ba60e0c3) deleted the original byte-pipe
//! bridge binary and the UDS transport. Users who installed trusty-memory
//! before that deletion still have `trusty-memory-mcp-bridge` (no args) in
//! their `.mcp.json` — those deployments receive JSON-RPC -32000 because the
//! binary no longer ships with `cargo install trusty-memory`.  This shim
//! restores the binary so existing configs stop breaking; it simply
//! re-execs the same process as `trusty-memory serve --stdio`, which is the
//! canonical MCP integration today.
//!
//! Migration: update your `.mcp.json` (or `~/.claude/mcp.json`) to replace:
//!   `"command": "trusty-memory-mcp-bridge", "args": []`
//! with:
//!   `"command": "trusty-memory", "args": ["serve", "--stdio"]`
//! Run `trusty-memory migrate kuzu-memory` for an automated rewrite.
//!
//! What: emits one deprecation warning to stderr (never stdout — MCP JSON-RPC
//! framing owns stdout), then re-execs `trusty-memory serve --stdio` using
//! the same binary path so no extra process stays alive. On platforms where
//! `exec` is unavailable it falls back to spawning a child, forwarding
//! stdio, and exiting with the child's status.
//!
//! Test: `cargo run --bin trusty-memory-mcp-bridge -- --help 2>&1 | grep DEPRECATED`
//! confirms the deprecation warning.  The MCP round-trip path is covered by
//! `tests/serve_stdio_e2e.rs` (which tests `serve --stdio` directly).

use std::process::ExitCode;

fn main() -> ExitCode {
    // Why: MCP framing owns stdout — any byte written to stdout that is not
    // a valid JSON-RPC message will corrupt the protocol and cause -32000.
    // We emit the deprecation warning to stderr only.
    eprintln!(
        "[DEPRECATED] trusty-memory-mcp-bridge is deprecated and will be removed in a future version.\n\
         Update your MCP config to use: \"command\": \"trusty-memory\", \"args\": [\"serve\", \"--stdio\"]\n\
         Run `trusty-memory migrate kuzu-memory` for an automated config rewrite.\n\
         Forwarding to `trusty-memory serve --stdio` now…"
    );

    // Resolve the canonical `trusty-memory` binary path: prefer the sibling
    // binary in the same directory as this process (covers `cargo install`
    // and PATH-based invocations equally), then fall back to plain
    // "trusty-memory" on PATH.
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("trusty-memory")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("trusty-memory"));

    // Re-exec as `trusty-memory serve --stdio`.
    // On Unix, `exec` replaces this process entirely so no extra zombie stays
    // alive — the PID Claude Code tracks keeps working.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe)
            .arg("serve")
            .arg("--stdio")
            .exec(); // only returns on error
        eprintln!("trusty-memory-mcp-bridge: exec failed: {err}");
        ExitCode::FAILURE
    }

    // Non-Unix fallback: spawn a child, forward stdio, propagate exit status.
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(&exe)
            .arg("serve")
            .arg("--stdio")
            .status();
        match status {
            Ok(s) => {
                if s.success() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("trusty-memory-mcp-bridge: spawn failed: {e}");
                ExitCode::FAILURE
            }
        }
    }
}
