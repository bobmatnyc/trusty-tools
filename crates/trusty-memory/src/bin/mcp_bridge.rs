//! `trusty-memory-mcp-bridge` — reconnecting byte pipe between Claude Code's
//! stdio MCP transport and the trusty-memory daemon's Unix domain socket.
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
//!   - `UnixStream::connect` with exponential-backoff reconnect on disconnect
//!   - `tokio::io::copy` between (stdin, stdout) and the socket
//!   - reconnect when the socket closes (daemon restart between requests)
//!   - exit on stdin EOF (Claude Code disconnected)
//!
//! Test: see `bridge_byte_pipe_smoke` and `bridge_never_opens_redb` in
//! `tests/uds_roundtrip.rs`.
//!
//! ## Reconnect behaviour and MCP session-state caveat
//!
//! MCP is a stateful JSON-RPC session — the client performs an `initialize`
//! handshake when it first connects. If the daemon restarts while a request
//! is in flight, the partial response is lost and the bridge reconnects to a
//! fresh daemon that has no knowledge of the previous session ID.
//!
//! This bridge implements **idle-safe reconnect only**: it detects socket
//! closure when the downstream direction (socket→stdout) finishes while the
//! upstream direction (stdin→socket) is still waiting for more input from
//! Claude Code. It then loops with exponential backoff until the daemon socket
//! reappears, reconnects, and resumes piping. From Claude Code's perspective
//! the stdio process never exited, so the session continues without disruption
//! between requests.
//!
//! If the daemon restarts mid-request (while Claude Code is actively writing
//! a JSON-RPC payload), the in-flight bytes are lost. The bridge degrades
//! gracefully: it reconnects and resumes piping so subsequent requests work
//! normally. Claude Code's MCP client will time out the in-flight call and
//! retry at the application layer. This is the correct 80/20 trade-off for
//! a single-user localhost daemon — full session-replay would require
//! intercepting and buffering JSON-RPC framing, which violates the byte-pipe
//! contract of this binary.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use trusty_memory::resolve_palace_registry_dir;
use trusty_memory::transport::uds::{socket_path, UDS_ADDR_FILE};

/// Environment variable that, when set, overrides the auto-detected
/// socket path. Useful for integration tests that spin up a daemon on
/// an isolated tempdir-scoped socket.
const SOCKET_ENV: &str = "TRUSTY_MEMORY_SOCKET";

/// Initial reconnect backoff delay (200 ms).
const BACKOFF_INITIAL: Duration = Duration::from_millis(200);

/// Maximum reconnect backoff delay (30 s).
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Locate the running daemon's Unix socket.
///
/// Why: the daemon may have been started with an unusual `$TMPDIR`,
/// or a test may have pinned it to an isolated tempdir. Trying the
/// env override first, then the `<data_root>/uds_addr` discovery
/// file (checking both the data-dir root and the `palaces/` subdir),
/// and finally the OS-default socket path gives the bridge four
/// independent fallbacks before failing. Re-reading the discovery file
/// on every call lets the bridge pick up a new socket path written by
/// a restarted daemon without requiring the bridge process to restart.
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

/// Outcome of a single connection cycle in [`pipe_loop`].
///
/// Why: distinguishes the four ways a bidirectional pipe can terminate so the
/// outer loop can choose the right action (reconnect vs. exit).
/// What: returned by [`pipe_one_connection`].
/// Test: covered implicitly by the pipe-loop integration tests.
enum PipeOutcome {
    /// stdin reached EOF — Claude Code disconnected; exit cleanly.
    StdinEof,
    /// The daemon socket closed cleanly — daemon restarted; reconnect.
    SocketClosed,
    /// stdin→socket copy returned an I/O error (not EOF).
    UpstreamError(std::io::Error),
    /// socket→stdout copy returned an I/O error (not clean close).
    DownstreamError(std::io::Error),
}

/// Wait for the daemon socket to be available, retrying with exponential backoff.
///
/// Why: after a daemon restart (e.g. `launchctl bootout` then `cargo install`
/// then `launchctl bootstrap`) the socket file disappears while the new daemon
/// is initialising. Without retry, the bridge would exit and Claude Code would
/// lose the MCP server for the rest of the session. Looping with backoff makes
/// daemon restarts transparent to Claude Code when they occur between requests.
///
/// What: loops indefinitely, re-resolving the socket path on every attempt
/// (the daemon may write a new path on restart), sleeping with exponential
/// backoff starting at [`BACKOFF_INITIAL`] and capped at [`BACKOFF_MAX`].
/// Logs each retry to STDERR only — never stdout, which is the MCP framing channel.
///
/// Test: backoff schedule is unit-tested in this module; end-to-end reconnect
/// is verified in `tests/uds_roundtrip.rs`.
async fn connect_with_backoff() -> UnixStream {
    let mut backoff = BACKOFF_INITIAL;
    let mut attempt: u32 = 0;

    loop {
        let sock_path = resolve_socket_path();
        match UnixStream::connect(&sock_path).await {
            Ok(stream) => {
                if attempt > 0 {
                    eprintln!(
                        "trusty-memory-mcp-bridge: reconnected to daemon socket {} \
                         after {attempt} attempt(s)",
                        sock_path.display()
                    );
                }
                return stream;
            }
            Err(e) => {
                attempt += 1;
                eprintln!(
                    "trusty-memory-mcp-bridge: connect attempt {attempt} failed \
                     ({}): {e} — retrying in {}ms",
                    sock_path.display(),
                    backoff.as_millis(),
                );
                tokio::time::sleep(backoff).await;
                // Double the delay, capped at BACKOFF_MAX.
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Run one bidirectional copy cycle on the provided socket.
///
/// Why: separating per-connection logic from the reconnect loop makes both
/// halves independently testable and keeps `pipe_loop` readable.
///
/// What: splits `sock` into read/write halves, then races two async copy
/// tasks in a `tokio::select!` (upstream: stdin to socket write half;
/// downstream: socket read half to stdout). Whichever task finishes first
/// (or errors) determines the [`PipeOutcome`]. stdin EOF becomes `StdinEof`;
/// socket clean close becomes `SocketClosed`; I/O errors propagate as the
/// corresponding error variant.
///
/// Test: `bridge_byte_pipe_smoke` in `tests/uds_roundtrip.rs` exercises the
/// happy path (upstream finishes first, `StdinEof`).
async fn pipe_one_connection(sock: UnixStream) -> PipeOutcome {
    let (mut sock_r, mut sock_w) = sock.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let upstream = async {
        let result = tokio::io::copy(&mut stdin, &mut sock_w).await;
        // Half-close so the daemon sees EOF on its side.
        let _ = sock_w.shutdown().await;
        result
    };
    let downstream = async {
        let result = tokio::io::copy(&mut sock_r, &mut stdout).await;
        let _ = stdout.flush().await;
        result
    };

    tokio::select! {
        upstream_res = upstream => match upstream_res {
            Ok(_) => PipeOutcome::StdinEof,
            Err(e) => PipeOutcome::UpstreamError(e),
        },
        downstream_res = downstream => match downstream_res {
            Ok(_) => PipeOutcome::SocketClosed,
            Err(e) => PipeOutcome::DownstreamError(e),
        },
    }
}

/// Pipe loop: connect → pipe until socket closes → reconnect until stdin EOF.
///
/// Why: encapsulates the reconnect loop so `main` stays readable. The outer
/// loop handles daemon restarts; `pipe_one_connection` handles the per-
/// connection bidirectional copy.
///
/// What: on each iteration, calls `pipe_one_connection`. If the socket closes
/// (daemon restarted), logs to stderr and calls `connect_with_backoff` to
/// wait for the new daemon. If stdin closes (`StdinEof`), or an I/O error
/// occurs on either side, exits with the appropriate code.
///
/// Test: integration tests in `tests/uds_roundtrip.rs` exercise the full loop.
async fn pipe_loop(first_sock: UnixStream) -> ExitCode {
    let mut sock = first_sock;
    loop {
        match pipe_one_connection(sock).await {
            PipeOutcome::StdinEof => {
                return ExitCode::from(0);
            }
            PipeOutcome::SocketClosed => {
                eprintln!(
                    "trusty-memory-mcp-bridge: daemon socket closed — \
                     reconnecting with backoff…"
                );
                sock = connect_with_backoff().await;
            }
            PipeOutcome::UpstreamError(e) => {
                eprintln!("trusty-memory-mcp-bridge: stdin→socket copy failed: {e}");
                return ExitCode::from(1);
            }
            PipeOutcome::DownstreamError(e) => {
                eprintln!("trusty-memory-mcp-bridge: socket→stdout copy failed: {e}");
                return ExitCode::from(1);
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let sock_path = resolve_socket_path();
    let first_sock = match UnixStream::connect(&sock_path).await {
        Ok(s) => s,
        Err(e) => {
            // Log to stderr (MCP protocol uses stdout — anything we
            // emit on stdout would corrupt the framing).
            // Enter backoff loop rather than exiting immediately so
            // Claude Code doesn't lose the bridge if the daemon is
            // still starting up after an install/restart.
            eprintln!(
                "trusty-memory-mcp-bridge: could not connect to daemon socket {}: {e}",
                sock_path.display()
            );
            eprintln!(
                "hint: start the daemon with `trusty-memory start` or \
                 `trusty-memory serve --foreground`"
            );
            eprintln!("trusty-memory-mcp-bridge: waiting for daemon to become available…");
            connect_with_backoff().await
        }
    };

    pipe_loop(first_sock).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: verify the doubling backoff schedule stays within the defined cap.
    /// What: simulates 20 doublings from BACKOFF_INITIAL and asserts the cap.
    /// Test: `cargo test -p trusty-memory -- mcp_bridge`.
    #[test]
    fn backoff_schedule_caps_at_max() {
        let mut b = BACKOFF_INITIAL;
        for _ in 0..20 {
            b = (b * 2).min(BACKOFF_MAX);
        }
        assert_eq!(b, BACKOFF_MAX);
    }

    /// Why: guard against configuration mistakes where initial > max.
    /// What: trivial ordering assertion.
    /// Test: same test binary as above.
    #[test]
    fn backoff_initial_less_than_max() {
        assert!(BACKOFF_INITIAL < BACKOFF_MAX);
    }
}
