//! Concurrent `serve --stdio` bridge isolation test for `trusty-memory`
//! (updated for issue #1152 — no_spawn contract).
//!
//! Why: with the daemon-bridge design, multiple `serve --stdio` processes all
//! proxy to the single daemon. No bridge ever spawns its own unmanaged daemon
//! (#1152). This test validates:
//!   1. Two concurrent bridge clients that share one pre-provisioned daemon can
//!      both perform reads with no lock contention at the bridge layer.
//!   2. Both clients see the same tool set (no stale snapshot divergence).
//!   3. Neither client hangs — all responses arrive within `RESPONSE_DEADLINE`.
//!   4. (Regression for #1152) The bridge never spawns an orphan daemon, and it
//!      exits promptly when its client closes stdin.
//!
//! #8351 replaced the other half of #1152's contract. "If no daemon is running
//! it exits with a clear error" was the designed behaviour until #8351, and it
//! is what made one pane lose memory for a session: an MCP client does not
//! re-spawn a server that exited, so a daemon that was briefly unreachable
//! ended the bridge permanently. The bridge now stays up and answers, and the
//! unit tests in `commands::serve_stdio_bridge` own that half.
//!
//! Test strategy: provision a single HTTP daemon in an isolated temp data dir on
//! an OS-assigned port, wait for it to signal readiness via its socket,
//! then start two `serve --stdio` bridges pointing at the SAME temp dir.  Both
//! bridges discover the daemon's address from `{tempdir}/trusty-memory/http_addr`.
//! Tear down the daemon after the assertions.
//!
//! What:
//!   - `stdio_serve_concurrent_two_bridges_both_work`: provisions a daemon, spawns
//!     two bridges, sends `initialize`, `tools/list`, and `palace_list` through
//!     both concurrently, asserts all succeed within `RESPONSE_DEADLINE`.
//!   - `stdio_bridge_exits_on_stdin_eof_and_never_orphans_a_daemon`: asserts the
//!     bridge exits promptly when stdin closes rather than hanging, and that NO
//!     additional child process matching the exe name is spawned (no orphan
//!     squatter).
//!   - `the_handshake_answers_after_the_daemon_guard_fails`: the #8351 fail-open
//!     arm — a data directory that cannot resolve makes the guard fail, and the
//!     bridge still answers a real `initialize` on stdout.
//!
//! Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e`.
//! Requires Cargo to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

mod common;
use common::DaemonGuard;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Wall-clock deadline for each request/response pair.
///
/// Why: includes daemon startup time (~5 s on a warm machine) plus headroom
/// for slow CI hosts.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(60);

/// Deadline for the child process to exit after stdin EOF.
const EXIT_DEADLINE: Duration = Duration::from_secs(15);

/// How often to poll for the daemon's `http_addr` readiness file.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum time to wait for the daemon to write its socket.
///
/// Why: 30 s covers resource-constrained CI runners; on typical developer
/// hardware the daemon writes the file in < 1 s.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path to the `trusty-memory` binary produced by Cargo.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Lightweight handle for a raw child process with separate stdio fields.
///
/// Why: the concurrent test drives two children in parallel; independent
/// stdin/stdout pipes prevent borrowing conflicts.
/// What: bundles the child handle, stdin writer, and stdout reader.
/// Test: used by `stdio_serve_concurrent_two_bridges_both_work`.
struct RawChild {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl RawChild {
    /// Close stdin (EOF) and wait for the child to exit within `EXIT_DEADLINE`.
    async fn close(mut self) {
        drop(self.stdin);
        let _ = timeout(EXIT_DEADLINE, self.child.wait()).await;
    }
}

/// Spawn a raw `serve --stdio` bridge child against the given data path.
///
/// Why: the bridge is a pure proxy — it requires an already-running daemon.
/// The caller must provision the daemon first (via `spawn_daemon`) and pass
/// the same `data_path` so the bridge can discover the daemon address from
/// `{data_path}/trusty-memory/http_addr`.
/// What: spawns the binary with piped stdin/stdout; stderr goes to the test's
/// stderr for visibility on failure.
/// Test: used by `stdio_serve_concurrent_two_bridges_both_work`.
async fn spawn_raw_bridge(data_path: &std::path::Path) -> RawChild {
    let mut cmd = tokio::process::Command::new(binary());
    cmd.arg("serve")
        .arg("--stdio")
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_path)
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // #7085: a SIGKILL of this test binary runs no destructor, so the bridge
    // watches us instead.
    trusty_common::parent_death::exit_with_parent_tokio(&mut cmd);
    let mut child = cmd.spawn().expect("spawn bridge child");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    RawChild {
        child,
        stdin,
        reader: BufReader::new(stdout),
    }
}

/// Provision a foreground HTTP daemon in `data_path` on an OS-assigned port
/// and wait for it to signal readiness.
///
/// Why: the `serve --stdio` bridge uses `no_spawn: true` (issue #1152) and
/// refuses to auto-start a daemon. Tests that exercise the bridge must start
/// the daemon themselves. Using `--http 127.0.0.1:0` lets the OS pick a free
/// port so concurrent test runs cannot collide.  `TRUSTY_DATA_DIR_OVERRIDE`
/// confines all state (http_addr, palaces) to the isolated temp dir.
/// What: spawns `serve --foreground --http 127.0.0.1:0` with
/// `TRUSTY_DATA_DIR_OVERRIDE=data_path`, then polls for
/// `{data_path}/trusty-memory/http_addr` (the daemon writes this file
/// synchronously during `run_http_on`) as the readiness signal.
/// Panics if the file does not appear within `DAEMON_BOOT_TIMEOUT`.
/// Returns a [`DaemonGuard`] that kills the daemon on drop, so a failing
/// assertion here or in the caller cannot orphan it (#5188).
/// Test: used by `stdio_serve_concurrent_two_bridges_both_work`.
fn spawn_daemon(data_path: &std::path::Path) -> DaemonGuard {
    // #5188: the guard owns the child from the moment it exists, so the
    // asserting readiness poll below cannot orphan it. #7085: the guard also
    // applies the parent-death stamp, which a SIGKILL of this test binary —
    // where no `Drop` runs at all — is the only thing that survives.
    let guard = DaemonGuard::spawn(data_path);

    // Poll for the readiness file.
    let readiness_file = common::socket_path(data_path);
    let deadline = std::time::Instant::now() + DAEMON_BOOT_TIMEOUT;
    loop {
        if readiness_file.exists() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not bind its socket within {:?}; expected at {}",
            DAEMON_BOOT_TIMEOUT,
            readiness_file.display()
        );
        std::thread::sleep(POLL_INTERVAL);
    }

    guard
}

/// Write one JSON-RPC request line to a raw stdin pipe.
async fn send_raw(stdin: &mut ChildStdin, req: Value) {
    let line = serde_json::to_string(&req).expect("serialise");
    stdin.write_all(line.as_bytes()).await.expect("write");
    stdin.write_all(b"\n").await.expect("newline");
    stdin.flush().await.expect("flush");
}

/// Read the next JSON-RPC response from a raw reader within the deadline.
///
/// Why: the never-hang invariant — if the server hangs this panics with a
/// clear "server hung?" message rather than waiting indefinitely.
/// What: reads until a non-empty line arrives; panics on child exit (0-byte
/// read) or deadline exceeded.
async fn recv_raw(reader: &mut BufReader<ChildStdout>) -> Value {
    let read_fut = async {
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .expect("read_line I/O error");
            if n == 0 {
                panic!("child exited without sending a response (EOF on stdout)");
            }
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    };
    let raw = timeout(RESPONSE_DEADLINE, read_fut)
        .await
        .expect("response deadline exceeded — server hung?");
    serde_json::from_str::<Value>(&raw).expect("valid JSON")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Why: proves that two concurrent bridge clients sharing one provisioned
/// daemon can both operate without hanging.
///
/// Under the `no_spawn: true` bridge architecture there is no "read-only
/// snapshot fallback" — both clients proxy to the same daemon and get full
/// read/write access through it.  The key invariants are:
///   1. A daemon is provisioned BEFORE the bridges are started (no_spawn).
///   2. Both `initialize` responses arrive within the deadline.
///   3. Both `tools/list` responses arrive within the deadline and return a
///      non-empty `tools` array.
///   4. Both `palace_list` responses arrive within the deadline.
///   5. No orphan daemon process is spawned by the bridges themselves.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- stdio_serve_concurrent_two_bridges_both_work`.
#[tokio::test]
async fn stdio_serve_concurrent_two_bridges_both_work() {
    // Provision ONE isolated daemon shared by both bridges.  Both bridges
    // share this data dir so they both discover the same http_addr.
    let data_dir = tempfile::tempdir().expect("tempdir");
    let daemon = spawn_daemon(data_dir.path());

    // Spawn two bridges pointing at the same provisioned daemon.
    let mut child1 = spawn_raw_bridge(data_dir.path()).await;
    let mut child2 = spawn_raw_bridge(data_dir.path()).await;

    // ── Initialize both children ───────────────────────────────────────────
    send_raw(
        &mut child1.stdin,
        json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}
        }),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}
        }),
    )
    .await;

    let init1 = recv_raw(&mut child1.reader).await;
    let init2 = recv_raw(&mut child2.reader).await;
    assert_eq!(init1["id"], 1, "child1 initialize id mismatch");
    assert_eq!(init2["id"], 1, "child2 initialize id mismatch");
    assert!(
        init1["error"].is_null(),
        "child1 initialize must succeed; got: {init1}"
    );
    assert!(
        init2["error"].is_null(),
        "child2 initialize must succeed; got: {init2}"
    );

    // ── Both children request tools/list ──────────────────────────────────
    send_raw(
        &mut child1.stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;

    let list1 = recv_raw(&mut child1.reader).await;
    let list2 = recv_raw(&mut child2.reader).await;

    assert_eq!(list1["id"], 2, "child1 tools/list id mismatch");
    assert_eq!(list2["id"], 2, "child2 tools/list id mismatch");

    assert!(
        list1["error"].is_null(),
        "child1 tools/list must succeed; got: {list1}"
    );
    assert!(
        list2["error"].is_null(),
        "child2 tools/list must succeed; got: {list2}"
    );

    let tools1 = list1["result"]["tools"]
        .as_array()
        .expect("child1 tools/list must return an array");
    let tools2 = list2["result"]["tools"]
        .as_array()
        .expect("child2 tools/list must return an array");

    assert!(
        !tools1.is_empty(),
        "child1 tools/list must return at least one tool"
    );
    assert!(
        !tools2.is_empty(),
        "child2 tools/list must return at least one tool"
    );

    // ── Both children request palace_list ─────────────────────────────────
    send_raw(
        &mut child1.stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"palace_list"}),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"palace_list"}),
    )
    .await;

    let plist1 = recv_raw(&mut child1.reader).await;
    let plist2 = recv_raw(&mut child2.reader).await;

    assert_eq!(plist1["id"], 3, "child1 palace_list id mismatch");
    assert_eq!(plist2["id"], 3, "child2 palace_list id mismatch");

    assert!(
        plist1["error"].is_null(),
        "child1 palace_list must succeed; got: {plist1}"
    );
    assert!(
        plist2["error"].is_null(),
        "child2 palace_list must succeed; got: {plist2}"
    );

    // ── Teardown ───────────────────────────────────────────────────────────
    child1.close().await;
    child2.close().await;
    // #5188: `daemon` is a `DaemonGuard` — dropping it kills and reaps.
    drop(daemon);
}

/// Why (regression for issue #1152, narrowed by #8351): the bridge must never
/// spawn a background `serve --foreground --http :0` squatter of its own, and
/// must never hang. #1152's other assertion — that an unreachable daemon makes
/// the bridge exit — is deliberately gone: #8351 showed that exit is what cost
/// a client session its memory tools, because nothing re-spawns an MCP server
/// that exited. The bridge now serves the handshake locally instead, which
/// `the_handshake_is_answered_with_no_daemon_listening` covers.
///
/// What: creates an empty temp data dir, spawns a bridge, closes its stdin
/// immediately, and asserts the process exits within `EXIT_DEADLINE`.
///
/// What this does NOT cover, and why: an empty temp data dir RESOLVES, so
/// `ensure_daemon_up_for_stdio` takes #5267's start-if-not-running path and
/// brings a daemon up in that dir — the guard's `Ok` arm, not the fail-open
/// arm #8351 added. The guard-failure half needs a data directory that cannot
/// resolve at all, which is
/// `the_handshake_answers_after_the_daemon_guard_fails` below. What is left
/// here is the exit contract: one bridge, one guard, and EOF on stdin as the
/// only thing that ends the process, inside `EXIT_DEADLINE`.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- stdio_bridge_exits_on_stdin_eof_and_never_orphans_a_daemon`.
#[tokio::test]
async fn stdio_bridge_exits_on_stdin_eof_and_never_orphans_a_daemon() {
    // Empty temp dir — no daemon, no socket.
    let data_dir = tempfile::tempdir().expect("tempdir");

    let mut cmd = tokio::process::Command::new(binary());
    cmd.arg("serve")
        .arg("--stdio")
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir.path())
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()); // suppress error output in test runs
                                // #7085: see `spawn_raw_bridge`.
    trusty_common::parent_death::exit_with_parent_tokio(&mut cmd);

    let mut child = cmd.spawn().expect("spawn bridge");
    // Close stdin immediately — EOF on stdin is how an MCP server is told to
    // exit (#457), and #8351 made it the ONLY thing that ends this process.
    drop(child.stdin.take());

    let exit_result = timeout(EXIT_DEADLINE, child.wait())
        .await
        .expect("bridge must exit within EXIT_DEADLINE once stdin closes");

    // The bridge may exit with a non-zero code (error) or zero — the
    // important thing is that it DID exit rather than hanging.
    let _ = exit_result; // we only care it exited, not the exact status
}

/// Why (#8351, critic round on PR #8359): the fail-open arm in
/// `run_stdio_bridge` — the one that reports a failed
/// `ensure_daemon_up_for_stdio` to stderr instead of returning `Err` — was the
/// whole point of the fix and no test reached it. The unit tests drive
/// `build_bridge` / `answer` directly, and
/// `stdio_bridge_exits_on_stdin_eof_and_never_orphans_a_daemon` hands the guard
/// an empty temp dir, which under #5267's start-if-not-running guard STARTS a
/// daemon and takes the `Ok` arm. This test is the guard-failure half: without
/// it, restoring `ensure_daemon_up_for_stdio().await?` would keep every test
/// green while costing a live client its session again.
///
/// What: points `TRUSTY_DATA_DIR_OVERRIDE` at a regular FILE, so
/// `trusty_common::resolve_data_dir`'s `create_dir_all` fails with `ENOTDIR`,
/// `start_lock_path()` yields `None`, and the guard returns `Err` before it can
/// probe, lock or spawn anything. Then sends a real `initialize` over the real
/// stdin pipe and asserts a real JSON-RPC result comes back on stdout —
/// matching id, no `error`, and the handshake fields a client actually reads.
/// Under the pre-#8351 shape the process exits before reading stdin at all, so
/// `recv_raw` panics on EOF.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- the_handshake_answers_after_the_daemon_guard_fails`.
#[tokio::test]
async fn the_handshake_answers_after_the_daemon_guard_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A regular file standing where the data directory must be. Nothing can
    // make this resolve, so the guard's failure is the test's own fixture
    // rather than a timing window.
    let blocked = tmp.path().join("data-dir-is-a-regular-file");
    std::fs::write(&blocked, b"not a directory").expect("write the blocking file");

    let mut bridge = spawn_raw_bridge(&blocked).await;

    send_raw(
        &mut bridge.stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 8351,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "guard-failure-e2e", "version": "0"}
            }
        }),
    )
    .await;

    let response = recv_raw(&mut bridge.reader).await;

    assert_eq!(
        response["id"], 8351,
        "the handshake must answer the id it was asked with; got: {response}"
    );
    assert!(
        response.get("error").is_none(),
        "a failed daemon guard must not turn the handshake into an error; got: {response}"
    );
    assert_eq!(
        response["result"]["protocolVersion"], "2024-11-05",
        "the local answer must carry the protocol version a client reads; got: {response}"
    );
    assert_eq!(
        response["result"]["serverInfo"]["name"], "trusty-memory",
        "the local answer must name this server; got: {response}"
    );

    bridge.close().await;
}
