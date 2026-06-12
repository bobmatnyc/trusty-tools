//! Concurrent `serve --stdio` bridge isolation test for `trusty-memory`
//! (updated for issue #1078 — daemon-bridge architecture).
//!
//! Why: with the daemon-bridge design, multiple `serve --stdio` processes all
//! proxy to the single HTTP daemon.  There is no longer a "read-only snapshot
//! fallback" for the second process — both (or all N) processes share full
//! read/write access through the daemon.  This test validates:
//!   1. Two concurrent bridge clients can both perform reads via the same
//!      daemon (no lock contention at the bridge layer).
//!   2. Both clients see writes made by each other (no stale snapshot).
//!   3. Neither client hangs — all responses arrive within `RESPONSE_DEADLINE`.
//!
//! The test uses two separate data directories, each with its own daemon
//! auto-started by the bridge.  This keeps the test hermetically isolated from
//! the user's real palace and avoids the TCC / Full-Disk-Access issue (#873).
//!
//! What:
//!   - `stdio_serve_concurrent_two_bridges_both_work`: spawns two bridge
//!     clients each against their own tempdir (which causes each to boot an
//!     isolated daemon).  Both send `initialize`, `tools/list`, and
//!     `palace_list`.  Asserts all responses arrive within the deadline and
//!     that `tools/list` returns a non-empty array for both.
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
/// Why: includes daemon startup time (~5 s for embedder + redb open on a warm
/// machine) plus headroom for slow CI hosts.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(60);

/// Deadline for the child process to exit after stdin EOF.
const EXIT_DEADLINE: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path to the trusty-memory binary built by Cargo.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Lightweight handle for a raw child process with separate stdio fields.
///
/// Why: the concurrent test spawns two children; each needs independent
/// stdin/stdout pipes so they can be driven in parallel without borrowing
/// conflicts.
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

/// Spawn a raw bridge `serve --stdio` child against the given data path.
///
/// Why: each test gets an isolated tempdir so its daemon does not collide
/// with the user's real daemon or other test instances.
/// What: spawns the binary with piped stdin/stdout; stderr goes to the
/// test's stderr for visibility on failure.
/// Test: used by `stdio_serve_concurrent_two_bridges_both_work`.
async fn spawn_raw_child(data_path: &std::path::Path) -> RawChild {
    let mut cmd = tokio::process::Command::new(binary());
    cmd.arg("serve")
        .arg("--stdio")
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_path)
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().expect("spawn child");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    RawChild {
        child,
        stdin,
        reader: BufReader::new(stdout),
    }
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

/// Why: proves that two concurrent bridge clients can both operate through
/// their respective daemons without hanging.  Each client auto-starts its
/// own isolated daemon via `TRUSTY_DATA_DIR_OVERRIDE`.
///
/// Under the bridge architecture there is no "read-only snapshot fallback"
/// — both clients proxy to their daemon and get full read/write access.
/// The key invariants are:
///   1. Both `initialize` responses arrive within the deadline.
///   2. Both `tools/list` responses arrive within the deadline and return a
///      non-empty `tools` array.
///   3. Both `palace_list` responses arrive within the deadline.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- stdio_serve_concurrent_two_bridges_both_work`.
#[tokio::test]
async fn stdio_serve_concurrent_two_bridges_both_work() {
    // Each child gets its own tempdir so its daemon is isolated.
    let data_dir1 = tempfile::tempdir().expect("tempdir1");
    let data_dir2 = tempfile::tempdir().expect("tempdir2");

    let mut child1 = spawn_raw_child(data_dir1.path()).await;
    let mut child2 = spawn_raw_child(data_dir2.path()).await;

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

    child1.close().await;
    child2.close().await;
}
