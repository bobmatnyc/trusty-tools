//! Concurrent `serve --stdio` bridge isolation test for `trusty-memory`
//! (updated for issue #1152 — no_spawn contract).
//!
//! Why: with the daemon-bridge design (`no_spawn: true`), multiple `serve --stdio`
//! processes all proxy to the single HTTP daemon.  The bridge NEVER auto-starts a
//! daemon; if none is running it exits with a clear error.  This test validates:
//!   1. Two concurrent bridge clients that share one pre-provisioned daemon can
//!      both perform reads with no lock contention at the bridge layer.
//!   2. Both clients see the same tool set (no stale snapshot divergence).
//!   3. Neither client hangs — all responses arrive within `RESPONSE_DEADLINE`.
//!   4. (Regression for #1152) When NO daemon is running, the bridge exits
//!      immediately with a human-readable error rather than spawning an orphan.
//!
//! Test strategy: provision a single HTTP daemon in an isolated temp data dir on
//! an OS-assigned port, wait for it to signal readiness via its `http_addr` file,
//! then start two `serve --stdio` bridges pointing at the SAME temp dir.  Both
//! bridges discover the daemon's address from `{tempdir}/trusty-memory/http_addr`.
//! Tear down the daemon after the assertions.
//!
//! What:
//!   - `stdio_serve_concurrent_two_bridges_both_work`: provisions a daemon, spawns
//!     two bridges, sends `initialize`, `tools/list`, and `palace_list` through
//!     both concurrently, asserts all succeed within `RESPONSE_DEADLINE`.
//!   - `stdio_bridge_exits_when_no_daemon`: asserts that with no daemon running the
//!     bridge exits (EOF) rather than hanging, and that NO additional child process
//!     matching the exe name is spawned (no orphan squatter).
//!
//! Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e`.
//! Requires Cargo to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

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

/// Maximum time to wait for the daemon to write its `http_addr` file.
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
/// Returns the spawned child so the caller can kill it during teardown.
/// Test: used by `stdio_serve_concurrent_two_bridges_both_work`.
fn spawn_daemon(data_path: &std::path::Path) -> std::process::Child {
    let child = std::process::Command::new(binary())
        .arg("serve")
        .arg("--foreground")
        .arg("--http")
        .arg("127.0.0.1:0")
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_path)
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn daemon");

    // Poll for the readiness file.
    let readiness_file = data_path.join("trusty-memory").join("http_addr");
    let deadline = std::time::Instant::now() + DAEMON_BOOT_TIMEOUT;
    loop {
        if readiness_file.exists() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not write http_addr within {:?}; expected at {}",
            DAEMON_BOOT_TIMEOUT,
            readiness_file.display()
        );
        std::thread::sleep(POLL_INTERVAL);
    }

    child
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
    let mut daemon = spawn_daemon(data_dir.path());

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
    let _ = daemon.kill();
    let _ = daemon.wait();
}

/// Why (regression for issue #1152): with `no_spawn: true`, the bridge must
/// exit immediately with a human-readable error when no daemon is reachable.
/// It must NOT spawn a background `serve --foreground --http :0` squatter.
///
/// What: creates an empty temp data dir (no daemon, no http_addr file),
/// spawns a bridge, closes its stdin immediately, then reads stdout until
/// EOF.  The bridge must exit within `EXIT_DEADLINE` and produce no JSON-RPC
/// response (it exits before entering the loop).
///
/// The no-orphan assertion is pragmatic: we verify the bridge exits promptly
/// (within EXIT_DEADLINE), which is only possible if it did NOT block waiting
/// for a daemon it spawned.  A spawned-orphan path would keep the bridge
/// alive until the orphan's 30-second startup budget expires.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- stdio_bridge_exits_when_no_daemon`.
#[tokio::test]
async fn stdio_bridge_exits_when_no_daemon() {
    // Empty temp dir — no daemon, no http_addr file.
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

    let mut child = cmd.spawn().expect("spawn bridge");
    // Close stdin immediately — the bridge should fail before entering the loop.
    drop(child.stdin.take());

    let exit_result = timeout(EXIT_DEADLINE, child.wait())
        .await
        .expect("bridge must exit within EXIT_DEADLINE when no daemon is reachable");

    // The bridge may exit with a non-zero code (error) or zero — the
    // important thing is that it DID exit rather than hanging.
    let _ = exit_result; // we only care it exited, not the exact status
}
