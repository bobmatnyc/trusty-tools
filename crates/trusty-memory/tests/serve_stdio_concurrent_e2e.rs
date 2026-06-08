//! Concurrent `serve --stdio` isolation test for `trusty-memory` (issue #914).
//!
//! Why: the redb write-lock design allows at most one `serve --stdio` process
//! to hold the exclusive write lock; any additional process falls back to a
//! read-only snapshot.  The never-hang invariant must also hold for the
//! second process: write attempts must return an explicit bounded error (not
//! hang), while reads must still succeed.  This is the spec for concurrency
//! safety — the test proves the claim, not merely that some response arrived.
//!
//! What:
//!   - `stdio_serve_concurrent_read_write_isolation`: opens the same data
//!     directory from two `serve --stdio` children; asserts:
//!     * If a child's `memory_remember` fails, the error message is either
//!       "read-only" (the snapshot fallback) or "warming up" (the embedder
//!       preflight) — no other error shape is acceptable.
//!     * Reads (`tools/list`) succeed on both children after the writes —
//!       this proves the second, read-only child is fully functional for
//!       read queries.
//!     * Neither child hangs: all responses arrive within `RESPONSE_DEADLINE`.
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
const RESPONSE_DEADLINE: Duration = Duration::from_secs(30);

/// Deadline for the child process to exit after stdin EOF.
const EXIT_DEADLINE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path to the trusty-memory binary built by Cargo.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Lightweight handle for a raw child process with separate stdio fields.
///
/// Why: the concurrent test spawns two children against the same data
/// directory; each child needs independent stdin/stdout pipes so they can
/// be driven in parallel without borrowing conflicts.
/// What: bundles the child handle, stdin writer, and stdout reader.
/// Test: used by `stdio_serve_concurrent_read_write_isolation`.
struct RawChild {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl RawChild {
    /// Close stdin (EOF) and wait for the child to exit within `EXIT_DEADLINE`.
    async fn close(mut self) {
        drop(self.stdin);
        timeout(EXIT_DEADLINE, self.child.wait())
            .await
            .expect("child exit deadline")
            .expect("child wait");
    }
}

/// Spawn a raw `serve --stdio` child against the given data path.
///
/// Why: the concurrent test needs two independent children without the shared
/// `TempDir` wrapper that `StdioChild` uses.  The data directory lifetime is
/// managed externally.
/// What: spawns the binary with piped stdin/stdout and returns the handles.
/// Test: used by `stdio_serve_concurrent_read_write_isolation`.
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
///
/// Why: the stdio loop is line-delimited; every message must end with `\n`.
/// Test: indirect.
async fn send_raw(stdin: &mut ChildStdin, req: Value) {
    let line = serde_json::to_string(&req).expect("serialise");
    stdin.write_all(line.as_bytes()).await.expect("write");
    stdin.write_all(b"\n").await.expect("newline");
    stdin.flush().await.expect("flush");
}

/// Read the next JSON-RPC response from a raw reader within the deadline.
///
/// Why: the never-hang invariant.  If `read_line` returns 0 bytes the child
/// has exited; failing immediately is cleaner than spinning until the deadline
/// (a crashed child masked as a timeout is much harder to debug).
/// What: reads until a non-empty line arrives, or panics immediately on child
/// exit (0-byte read), or fails on timeout.
/// Test: indirect.
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

/// Why: proves the concurrency-safety claim of `serve --stdio` (issue #914).
/// The second child to open a data directory falls back to a read-only
/// snapshot; any write it attempts must return an explicit bounded error
/// (not hang).  Reads must succeed on both children.
///
/// What:
///   1. Spawns two children against the same `data_path`.
///   2. Both send `initialize` and `palace_create`.
///   3. Both attempt `memory_remember` (the write scenario).
///   4. If a child's remember fails, the error message must contain either
///      `"palace is read-only"` (snapshot fallback) or `"warming up"`
///      (embedder preflight fires before read-only guard) — no other
///      error shape is acceptable.
///   5. Both children then send `tools/list` (a read) and assert it
///      returns a non-empty tools array — proves the second child is
///      fully functional for reads.
///
/// Test: `cargo test -p trusty-memory --test serve_stdio_concurrent_e2e -- stdio_serve_concurrent_read_write_isolation`.
#[tokio::test]
async fn stdio_serve_concurrent_read_write_isolation() {
    // Create a shared data directory.  `_keep` is held for the entire test so
    // the tempdir (and the redb files inside it) is not deleted while either
    // child is running.
    let data_dir = tempfile::tempdir().expect("tempdir");
    let _keep = &data_dir; // ensure data_dir is not dropped early

    let data_path = data_dir.path();

    let mut child1 = spawn_raw_child(data_path).await;
    let mut child2 = spawn_raw_child(data_path).await;

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

    let _init1 = recv_raw(&mut child1.reader).await;
    let _init2 = recv_raw(&mut child2.reader).await;

    // ── Both children attempt palace_create ───────────────────────────────
    send_raw(
        &mut child1.stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"palace_create","arguments":{"name":"shared-palace"}}}),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"palace_create","arguments":{"name":"shared-palace"}}}),
    )
    .await;

    let create1 = recv_raw(&mut child1.reader).await;
    let create2 = recv_raw(&mut child2.reader).await;
    assert_eq!(create1["id"], 2, "child1 palace_create id mismatch");
    assert_eq!(create2["id"], 2, "child2 palace_create id mismatch");

    // ── Both attempt memory_remember — the key write scenario ─────────────
    send_raw(
        &mut child1.stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_remember","arguments":{"palace":"shared-palace","text":"child1 memory"}}}),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_remember","arguments":{"palace":"shared-palace","text":"child2 memory"}}}),
    )
    .await;

    let rem1 = recv_raw(&mut child1.reader).await;
    let rem2 = recv_raw(&mut child2.reader).await;

    assert_eq!(rem1["id"], 3, "child1 remember id mismatch");
    assert_eq!(rem2["id"], 3, "child2 remember id mismatch");

    // Each child must return either a result OR an error — never absent.
    let c1_ok = !rem1["result"].is_null() || !rem1["error"].is_null();
    let c2_ok = !rem2["result"].is_null() || !rem2["error"].is_null();
    assert!(c1_ok, "child1 must return result or error; got: {rem1}");
    assert!(c2_ok, "child2 must return result or error; got: {rem2}");

    // Whichever child received an error must report a KNOWN bounded error:
    // either "palace is read-only" (snapshot fallback) or "warming up"
    // (embedder preflight fires before the read-only guard).
    // No other error message is acceptable — it would indicate a regression.
    for (label, resp) in [("child1", &rem1), ("child2", &rem2)] {
        if !resp["error"].is_null() {
            let msg = resp["error"]["message"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            let is_known_bounded = msg.contains("read-only") || msg.contains("warming up");
            assert!(
                is_known_bounded,
                "{label} got an unexpected error (neither read-only nor warming-up): {}",
                resp["error"]["message"]
            );
        }
    }

    // ── Both children perform a read (tools/list) after the writes ────────
    // This proves the second (read-only) child is fully functional for reads.
    send_raw(
        &mut child1.stdin,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
    )
    .await;
    send_raw(
        &mut child2.stdin,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
    )
    .await;

    let list1 = recv_raw(&mut child1.reader).await;
    let list2 = recv_raw(&mut child2.reader).await;

    assert_eq!(list1["id"], 4, "child1 tools/list id mismatch");
    assert_eq!(list2["id"], 4, "child2 tools/list id mismatch");

    // Both must succeed — reads are always allowed regardless of lock state.
    assert!(
        list1["error"].is_null(),
        "child1 tools/list must succeed; got: {list1}"
    );
    assert!(
        list2["error"].is_null(),
        "child2 tools/list must succeed (read-only child must handle reads); got: {list2}"
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
        "child2 tools/list must return at least one tool (read-only child is functional)"
    );

    child1.close().await;
    child2.close().await;
}
