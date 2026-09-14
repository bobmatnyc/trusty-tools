//! Per-server spawn isolation (#5428).

use std::time::Duration;

use super::support::stdio;
use crate::mcp::REMOTE_UNSUPPORTED;
use crate::mcp::spawn::{HANDSHAKE_TIMEOUT, connect};

/// Why: `StdioMcpClient` bounds each REQUEST at 30s, which at task startup —
/// twice, for handshake plus `tools/list` — is a minute of dead time behind
/// one wedged server. The tighter bound is the point, so it is asserted.
#[test]
fn handshake_timeout_is_ten_seconds() {
    assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
}

/// Why: slice 1 spawns local subprocesses only, and an `http` entry must come
/// back as a stated reason rather than as an error that ends the load.
#[tokio::test]
async fn a_remote_transport_is_refused_with_a_reason() {
    let mut server = stdio("remote", "unused", &[]);
    server.transport = trusty_mcp::config::McpTransport::Http {
        url: "https://example.invalid/mcp".to_string(),
        headers: std::collections::BTreeMap::new(),
    };

    let reason = connect(&server).await.err().expect("must refuse");

    assert_eq!(reason, REMOTE_UNSUPPORTED);
}

/// Why: a typo'd command is the commonest configuration mistake, and it must
/// cost exactly itself — a reason for this server, never a failed task run.
#[tokio::test]
async fn a_missing_binary_is_refused_with_a_reason() {
    let server = stdio("ghost", "/nonexistent/mcp/binary/xyzzy-5428", &[]);

    let reason = connect(&server).await.err().expect("must refuse");

    assert!(
        reason.contains("xyzzy-5428"),
        "the reason names the command: {reason}",
    );
}

/// Why: a server that starts and immediately exits speaks no MCP, and must
/// fail fast rather than sitting out the request timeout.
#[tokio::test]
#[cfg(unix)]
async fn a_server_that_speaks_no_mcp_is_refused_with_a_reason() {
    // `/bin/cat` stays alive and echoes the request back, so `initialize`
    // reads a frame carrying no `result` and fails at once.
    let server = stdio("parrot", "/bin/cat", &[]);

    let reason = connect(&server).await.err().expect("must refuse");

    assert!(
        reason.contains("handshake"),
        "the reason says what stage failed: {reason}",
    );
}
