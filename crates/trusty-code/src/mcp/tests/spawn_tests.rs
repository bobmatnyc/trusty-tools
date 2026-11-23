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

/// Why: #11650 — a wrapper command (`npx`, `uvx`) runs the real server as a
/// grandchild. The client's teardown must kill it too, not only the direct
/// child. The fixture backgrounds a `sleep` (the grandchild), records its pid,
/// then `exec`s a process that never answers, so the handshake times out and
/// the client is dropped.
#[cfg(unix)]
#[tokio::test]
async fn a_wrapper_grandchild_dies_with_the_client() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("grandchild.pid");
    let script = format!(
        "sleep 300 & echo $! > '{}'; exec cat > /dev/null",
        pidfile.display()
    );
    let server = stdio("wrapper", "/bin/sh", &["-c", &script]);

    let outcome = crate::mcp::spawn::connect_within(&server, Duration::from_secs(1)).await;
    assert!(outcome.is_err(), "the fixture never answers the handshake");

    let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
        .expect("the wrapper recorded its grandchild")
        .trim()
        .parse()
        .expect("a pid");
    // SAFETY: kill with signal 0 only probes for existence.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while alive() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if alive() {
        // SAFETY: as above; clean up the leaked fixture before failing.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        panic!("the grandchild {pid} outlived the client");
    }
}
