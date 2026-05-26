//! `trusty-memory-mcp-bridge` — pure byte pipe between Claude Code's
//! stdio MCP transport and the trusty-memory daemon's Unix domain
//! socket.
//!
//! 🔴 HARD RULE: this binary MUST NEVER open redb. It carries no
//! database knowledge, no schema awareness, no parsing of JSON-RPC
//! envelopes. It is a verbatim byte pipe — anything that arrives on
//! stdin is forwarded to the socket, and anything that arrives on the
//! socket is forwarded to stdout. The daemon owns the redb locks
//! exclusively; sidestepping that contract is what made the previous
//! `trusty-memory serve --stdio` path unusable (redb's exclusive locks
//! refused to grant a second handle to the same files while the HTTP
//! daemon held them).
//!
//! Why: Claude Code launches MCP servers as stdio child processes,
//! but the trusty-memory daemon must remain a single long-lived
//! process so it owns the redb locks for the life of the user's
//! session. This bridge gives Claude Code its expected stdio child
//! while the actual work runs in the daemon over UDS.
//!
//! What:
//!   - resolve the daemon's socket path (env override → `<data_root>/uds_addr` → OS default)
//!   - `UnixStream::connect`
//!   - `tokio::io::copy_bidirectional` between (stdin, stdout) and the
//!     socket
//!   - exit when either direction closes
//!
//! Test: see `bridge_byte_pipe_smoke` and `bridge_never_opens_redb` in
//! `tests/uds_roundtrip.rs`.

use std::path::PathBuf;
use std::process::ExitCode;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use trusty_memory::resolve_palace_registry_dir;
use trusty_memory::transport::uds::{socket_path, UDS_ADDR_FILE};

/// Environment variable that, when set, overrides the auto-detected
/// socket path. Useful for integration tests that spin up a daemon on
/// an isolated tempdir-scoped socket.
const SOCKET_ENV: &str = "TRUSTY_MEMORY_SOCKET";

/// Locate the running daemon's Unix socket.
///
/// Why: the daemon may have been started with an unusual `$TMPDIR`,
/// or a test may have pinned it to an isolated tempdir. Trying the
/// env override first, then the `<data_root>/uds_addr` discovery
/// file (checking both the data-dir root and the `palaces/` subdir),
/// and finally the OS-default socket path gives the bridge four
/// independent fallbacks before failing.
/// What: returns the resolved [`PathBuf`].
/// Test: `bridge_byte_pipe_smoke` exercises the env-override path.
fn resolve_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(SOCKET_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    // Try the `<data_root>/uds_addr` discovery file.
    // The daemon writes to `<data_root>/uds_addr` where `data_root`
    // is resolved via `resolve_palace_registry_dir`, which prefers
    // `<data_dir>/palaces` if that directory exists. So we need to
    // check both locations (palaces first, then the data_dir root).
    if let Ok(data_dir) = trusty_common::resolve_data_dir("trusty-memory") {
        // Try `<data_dir>/palaces/uds_addr` first (the production case)
        let palace_registry_dir = resolve_palace_registry_dir(data_dir.clone());
        if palace_registry_dir != data_dir {
            let addr_file = palace_registry_dir.join(UDS_ADDR_FILE);
            if let Ok(contents) = std::fs::read_to_string(&addr_file) {
                let path = contents.trim();
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
        }
        // Fallback: try `<data_dir>/uds_addr` (test case or legacy layout)
        let addr_file = data_dir.join(UDS_ADDR_FILE);
        if let Ok(contents) = std::fs::read_to_string(&addr_file) {
            let path = contents.trim();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    socket_path()
}

#[tokio::main]
async fn main() -> ExitCode {
    let sock_path = resolve_socket_path();
    let sock = match UnixStream::connect(&sock_path).await {
        Ok(s) => s,
        Err(e) => {
            // Log to stderr (MCP protocol uses stdout — anything we
            // emit on stdout would corrupt the framing). Then exit 1
            // so Claude Code surfaces the failure to the user.
            eprintln!(
                "trusty-memory-mcp-bridge: could not connect to daemon socket {}: {e}",
                sock_path.display()
            );
            eprintln!(
                "hint: start the daemon with `trusty-memory start` or `trusty-memory serve --foreground`"
            );
            return ExitCode::from(1);
        }
    };

    let (mut sock_r, mut sock_w) = sock.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // The bridge is two independent copy tasks running concurrently.
    // We don't use `tokio::io::copy_bidirectional` because we want to
    // exit as soon as EITHER direction closes (stdin EOF means Claude
    // Code disconnected; socket close means the daemon disappeared).
    let upstream = async move {
        let n = tokio::io::copy(&mut stdin, &mut sock_w).await?;
        // Half-close the socket write side so the daemon sees EOF.
        sock_w.shutdown().await?;
        Ok::<u64, std::io::Error>(n)
    };
    let downstream = async move {
        let n = tokio::io::copy(&mut sock_r, &mut stdout).await?;
        stdout.flush().await?;
        Ok::<u64, std::io::Error>(n)
    };

    tokio::select! {
        res = upstream => match res {
            Ok(_) => ExitCode::from(0),
            Err(e) => {
                eprintln!("trusty-memory-mcp-bridge: stdin→socket copy failed: {e}");
                ExitCode::from(1)
            }
        },
        res = downstream => match res {
            Ok(_) => ExitCode::from(0),
            Err(e) => {
                eprintln!("trusty-memory-mcp-bridge: socket→stdout copy failed: {e}");
                ExitCode::from(1)
            }
        },
    }
}
