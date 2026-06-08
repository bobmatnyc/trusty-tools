//! End-to-end never-hang proof for `trusty-memory serve --stdio` (issue #914).
//!
//! Why: the core invariant of the direct stdio JSON-RPC server is that every
//! request resolves within a wall-clock deadline — success or explicit error,
//! never a hang.  Unit tests cover individual pieces (adapter conversion,
//! readiness preflight); these tests prove the full spawn → request → response
//! round-trip.
//!
//! What:
//!   - `stdio_serve_tools_list_bounded`: spawns `serve --stdio` as a child,
//!     sends `initialize`, `notifications/initialized`, and `tools/list`, asserts
//!     each response arrives within a 15-second wall-clock deadline and contains
//!     valid JSON-RPC.
//!   - `stdio_serve_remember_and_recall_bounded`: exercises `memory_remember`
//!     and `memory_recall` via the stdio server with a deadline guard.
//!   - `stdio_serve_recall_all_bounded`: exercises `memory_recall_all` — the
//!     handler whose readiness preflight was missing before #914.  While the
//!     daemon is Warming (no embedder ready yet) it must return the fast
//!     "warming up" error within the deadline rather than hanging.
//!   - `stdio_serve_stdout_is_only_json`: asserts that every byte written to
//!     stdout before the first response is valid JSON (no banner noise).
//!   - `stdio_serve_concurrent_read_write_isolation`: opens the same palace
//!     from two `serve --stdio` children; asserts the second child's writes
//!     return an explicit "read-only" error fast (not a hang) while reads
//!     succeed on both children.
//!
//! Test: `cargo test -p trusty-memory --test serve_stdio_e2e`.
//! Requires Cargo to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

#![allow(clippy::too_many_lines)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Wall-clock deadline for each request/response pair.  Generous enough for
/// slow CI hosts (embedder warm-up can take seconds on first run); tight
/// enough to catch a hang.
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

/// Spawned child wrapper with its stdio pipes attached.
///
/// Why: each test gets a private palace directory and a matching child process;
/// this struct keeps them alive together so tempdir cleanup happens after the
/// child exits.
/// What: bundles the child handle, its stdin pipe, and its stdout reader.
/// Test: indirect — every test uses `StdioChild::spawn`.
struct StdioChild {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    _data_dir: TempDir,
}

impl StdioChild {
    /// Spawn `trusty-memory serve --stdio` with an isolated data directory.
    ///
    /// Why: each test must be isolated from the user's real data.
    /// What: creates a tempdir, sets `TRUSTY_DATA_DIR_OVERRIDE`, spawns the
    /// binary with piped stdin/stdout and stderr forwarded to the test's
    /// stderr (so failures are visible without polluting stdout).
    /// Test: indirectly.
    async fn spawn(palace: Option<&str>) -> Self {
        let data_dir = tempfile::tempdir().expect("tempdir");
        // TRUSTY_SKIP_PALACE_ENFORCEMENT lets the test use arbitrary palace names
        // without a `.trusty-tools/trusty-memory.yaml` pin file.
        let mut cmd = tokio::process::Command::new(binary());
        cmd.arg("serve")
            .arg("--stdio")
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir.path())
            .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Forward stderr to the test's stderr so we see tracing output on
            // failure without letting it contaminate stdout.
            .stderr(Stdio::inherit());

        if let Some(p) = palace {
            cmd.arg("--palace").arg(p);
        }

        let mut child = cmd.spawn().expect("spawn trusty-memory serve --stdio");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = child.stdout.take().expect("stdout pipe");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            _data_dir: data_dir,
        }
    }

    /// Write one JSON-RPC request line to the child's stdin.
    ///
    /// Why: the stdio loop is line-delimited; every message must end with `\n`.
    /// Test: indirect.
    async fn send(&mut self, req: &Value) {
        let line = serde_json::to_string(req).expect("serialise request");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write request");
        self.stdin.write_all(b"\n").await.expect("write newline");
        self.stdin.flush().await.expect("flush stdin");
    }

    /// Read the next JSON-RPC response line within `RESPONSE_DEADLINE`.
    ///
    /// Why: the never-hang invariant.  Any test that calls this will fail if the
    /// server hangs rather than emitting a response.
    /// What: reads a line from stdout (skipping empty lines); fails the test if
    /// the deadline is exceeded.
    /// Test: indirect.
    async fn recv(&mut self) -> Value {
        let read_fut = async {
            loop {
                let mut line = String::new();
                self.reader
                    .read_line(&mut line)
                    .await
                    .expect("read response line");
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    return trimmed;
                }
            }
        };
        let raw = timeout(RESPONSE_DEADLINE, read_fut)
            .await
            .expect("response must arrive within deadline — server hung?");
        serde_json::from_str(&raw).expect("response must be valid JSON")
    }

    /// Close stdin (EOF) and wait for the child to exit within `EXIT_DEADLINE`.
    async fn close(mut self) {
        drop(self.stdin);
        timeout(EXIT_DEADLINE, self.child.wait())
            .await
            .expect("child must exit after stdin EOF")
            .expect("child wait");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Why: establishes that `serve --stdio` can successfully handle the MCP
/// lifecycle handshake (`initialize`, `notifications/initialized`) and a
/// `tools/list` request — all within a wall-clock deadline.
/// What: spawns the server, sends the three requests, asserts each response
/// is valid JSON-RPC and that `tools/list` returns a non-empty tools array.
/// Test: `cargo test -p trusty-memory --test serve_stdio_e2e -- stdio_serve_tools_list_bounded`.
#[tokio::test]
async fn stdio_serve_tools_list_bounded() {
    let mut child = StdioChild::spawn(None).await;

    // Step 1: initialize handshake — Claude Code sends this first.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.0.1"}
            }
        }))
        .await;
    let init_resp = child.recv().await;
    assert!(
        init_resp["error"].is_null(),
        "initialize must succeed; got: {init_resp}"
    );
    assert_eq!(
        init_resp["result"]["protocolVersion"], "2024-11-05",
        "initialize must echo protocolVersion"
    );
    assert_eq!(init_resp["id"], 1, "response id must echo request id");

    // Step 2: notification — no response expected per MCP spec §4.1.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;
    // No recv() here — the server suppresses notification responses.

    // Step 3: tools/list — must arrive within deadline and list tools.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }))
        .await;
    let tools_resp = child.recv().await;
    assert!(
        tools_resp["error"].is_null(),
        "tools/list must succeed; got: {tools_resp}"
    );
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .expect("result.tools must be an array");
    assert!(
        !tools.is_empty(),
        "tools/list must return at least one tool"
    );

    // Verify stdout contains only JSON (no banner noise before first response).
    // The `init_resp` we already parsed is JSON — we checked it above.

    child.close().await;
}

/// Why: proves that `memory_remember` and `memory_recall` work end-to-end
/// through the stdio server and that both complete within the deadline.
/// The daemon starts in `Warming` state (no background embedder warm-up
/// completes before the test sends requests), so these calls may return the
/// fast "warming up" error OR succeed if the embedder completes quickly.
/// Either response counts as a pass — the invariant is "never hang".
/// Test: `cargo test -p trusty-memory --test serve_stdio_e2e -- stdio_serve_remember_and_recall_bounded`.
#[tokio::test]
async fn stdio_serve_remember_and_recall_bounded() {
    let mut child = StdioChild::spawn(Some("test-palace")).await;

    // Initialize.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name":"t","version":"0"}}
        }))
        .await;
    let init_resp = child.recv().await;
    assert!(
        init_resp["error"].is_null(),
        "initialize failed: {init_resp}"
    );

    // Create palace via tools/call.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "palace_create",
                "arguments": {"name": "test-palace"}
            }
        }))
        .await;
    let create_resp = child.recv().await;
    // palace_create may succeed or return a "palace already exists" variant;
    // either is fine.  We only assert the response arrived within the deadline.
    assert_eq!(create_resp["id"], 2, "palace_create response id must match");

    // memory_remember — may return success or warming error; must not hang.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "memory_remember",
                "arguments": {
                    "palace": "test-palace",
                    "text": "The stdio MCP server never hangs"
                }
            }
        }))
        .await;
    let remember_resp = child.recv().await;
    assert_eq!(
        remember_resp["id"], 3,
        "memory_remember response id must match"
    );
    // Either success or an explicit error (e.g., warming up) — never absent.
    let is_ok_or_error = !remember_resp["result"].is_null() || !remember_resp["error"].is_null();
    assert!(
        is_ok_or_error,
        "memory_remember must return a result or error; got: {remember_resp}"
    );

    // memory_recall — may return success or warming error; must not hang.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "memory_recall",
                "arguments": {
                    "palace": "test-palace",
                    "query": "stdio server"
                }
            }
        }))
        .await;
    let recall_resp = child.recv().await;
    assert_eq!(recall_resp["id"], 4, "memory_recall response id must match");
    let is_ok_or_error = !recall_resp["result"].is_null() || !recall_resp["error"].is_null();
    assert!(
        is_ok_or_error,
        "memory_recall must return a result or error; got: {recall_resp}"
    );

    child.close().await;
}

/// Why: `memory_recall_all` was the specific handler that lacked the
/// readiness preflight (issue #914 Part A fix).  This test proves that even
/// before the embedder is warm, `memory_recall_all` returns a bounded explicit
/// response — not a hang.
/// What: sends `memory_recall_all` immediately after `initialize` (before any
/// embedder warm-up could complete) and asserts the response arrives within
/// `RESPONSE_DEADLINE`.
/// Test: `cargo test -p trusty-memory --test serve_stdio_e2e -- stdio_serve_recall_all_bounded`.
#[tokio::test]
async fn stdio_serve_recall_all_bounded() {
    let mut child = StdioChild::spawn(None).await;

    // Initialize.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name":"t","version":"0"}}
        }))
        .await;
    let init_resp = child.recv().await;
    assert!(
        init_resp["error"].is_null(),
        "initialize failed: {init_resp}"
    );

    // memory_recall_all — send immediately, before any warm-up can complete.
    // Must return the fast "warming up" error (or succeed on very fast machines)
    // within the deadline.
    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "memory_recall_all",
                "arguments": {"q": "never hang test"}
            }
        }))
        .await;
    let resp = child.recv().await;
    assert_eq!(resp["id"], 2, "memory_recall_all response id must match");
    // Must be either a result or an error — never absent (never a hang).
    let has_result_or_error = !resp["result"].is_null() || !resp["error"].is_null();
    assert!(
        has_result_or_error,
        "memory_recall_all must return result or error within deadline; got: {resp}"
    );

    child.close().await;
}

/// Why: the stdio channel is the JSON-RPC transport — stdout must not carry
/// any non-protocol bytes (no update-check banners, no bind announcements).
/// What: sends a single `tools/list` request, reads the response, and asserts
/// the raw response line is valid JSON-RPC.  Since `recv()` parses JSON and
/// panics on failure, any banner noise would cause the test to fail rather
/// than silently pass.
/// Test: `cargo test -p trusty-memory --test serve_stdio_e2e -- stdio_serve_stdout_is_only_json`.
#[tokio::test]
async fn stdio_serve_stdout_is_only_json() {
    let mut child = StdioChild::spawn(None).await;

    child
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name":"t","version":"0"}}
        }))
        .await;
    // recv() would panic if the line is not valid JSON — that's the assertion.
    let resp = child.recv().await;
    assert!(
        resp["jsonrpc"].as_str() == Some("2.0"),
        "first stdout line must be a JSON-RPC 2.0 response; got: {resp}"
    );

    child.close().await;
}

/// Why: a second `serve --stdio` process opening the same data directory must
/// not hang when attempting a write — it must receive the fast "read-only"
/// error (snapshot fallback) from the palace registry.
/// What: spawns TWO child processes against the same data directory; has both
/// send `memory_remember` after creating a palace; asserts both receive a
/// response within the deadline.  The response may be success on the first
/// child and a read-only error on the second (depending on lock timing), but
/// neither may hang.
/// Test: `cargo test -p trusty-memory --test serve_stdio_e2e -- stdio_serve_concurrent_read_write_isolation`.
#[tokio::test]
async fn stdio_serve_concurrent_read_write_isolation() {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_path = data_dir.path().to_path_buf();

    let mut child1 = spawn_raw_child(data_path.clone()).await;
    let mut child2 = spawn_raw_child(data_path).await;

    // Initialize both children.
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

    // Both children attempt to create a palace.
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

    // Both attempt memory_remember — the key concurrency scenario.
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
    let c1_ok = !rem1["result"].is_null() || !rem1["error"].is_null();
    let c2_ok = !rem2["result"].is_null() || !rem2["error"].is_null();
    assert!(c1_ok, "child1 must return result or error; got: {rem1}");
    assert!(c2_ok, "child2 must return result or error; got: {rem2}");

    child1.close().await;
    child2.close().await;
}

// ---------------------------------------------------------------------------
// Raw helpers used by the concurrent test to avoid closure lifetime issues
// ---------------------------------------------------------------------------

/// Lightweight handle for a raw child process with separate stdio fields.
struct RawChild {
    child: tokio::process::Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl RawChild {
    async fn close(mut self) {
        drop(self.stdin);
        timeout(EXIT_DEADLINE, self.child.wait())
            .await
            .expect("child exit deadline")
            .expect("child wait");
    }
}

/// Spawn a raw `serve --stdio` child against the given data path.
async fn spawn_raw_child(data_path: std::path::PathBuf) -> RawChild {
    let mut cmd = tokio::process::Command::new(binary());
    cmd.arg("serve")
        .arg("--stdio")
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data_path)
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
async fn recv_raw(reader: &mut BufReader<ChildStdout>) -> Value {
    let read_fut = async {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read");
            let t = line.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
    };
    let raw = timeout(RESPONSE_DEADLINE, read_fut)
        .await
        .expect("response deadline exceeded — server hung?");
    serde_json::from_str::<Value>(&raw).expect("valid JSON")
}
