//! Clean-session regression for the vector Codex is registered with (#5265).
//!
//! Why: Codex launches `trusty-memory` with `args = ["serve"]` — no `--stdio`,
//! no other flag — and listed the connection as enabled while exposing no
//! tools. Every existing stdio e2e test spawns `serve --stdio`, so the exact
//! argument vector a Codex session uses was covered by nothing. A registration
//! contract that no test exercises is a contract that can drift back.
//!
//! What: spawns an isolated daemon, then spawns the binary with EXACTLY the
//! registered vector and drives a whole clean session over stdio — `initialize`,
//! `notifications/initialized`, `tools/list`, and one `get_prompt_context` tool
//! call — asserting each response arrives within a deadline and that stdout
//! carries JSON-RPC and nothing else.
//!
//! `TRUSTY_DATA_DIR_OVERRIDE` confines every byte of state to a tempdir, so the
//! test never reaches the operator's real palace or daemon.
//!
//! Test: `cargo test -p trusty-memory --test codex_stdio_e2e_5265`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

/// Wall-clock deadline for one request/response pair.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(60);

/// Deadline for the bridge to exit after stdin EOF.
const EXIT_DEADLINE: Duration = Duration::from_secs(15);

/// How long to wait for the isolated daemon to publish its address.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Polling interval for the daemon readiness file.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The argument vector `trusty-memory setup` registers with Codex (#5265).
///
/// Why: the test's whole point is that THIS vector reaches MCP. Spelling it out
/// as a constant makes a drift back to `["serve", "--stdio"]` a visible edit.
const REGISTERED_ARGS: &[&str] = &["serve"];

/// Path to the `trusty-memory` binary Cargo built for this test.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// An isolated daemon plus a bridge launched with the registered vector.
struct CodexSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    daemon: std::process::Child,
    _data_dir: tempfile::TempDir,
}

impl CodexSession {
    /// Provision the daemon, then launch the binary exactly as Codex does.
    ///
    /// Why: `--http 127.0.0.1:0` lets the OS pick a free port so concurrent test
    /// runs cannot collide, and provisioning the daemon here (rather than
    /// letting the bridge start one) keeps the test from leaving a detached
    /// daemon behind. The single-flight start the bridge would use instead is
    /// proven in `crates/trusty-common/tests/single_flight_exclusion.rs`.
    /// What: spawns `serve --foreground --http 127.0.0.1:0`, waits for its
    /// socket, then spawns the binary with [`REGISTERED_ARGS`] against
    /// the same data dir.
    async fn spawn() -> Self {
        let data_dir = tempfile::tempdir().expect("tempdir");

        let daemon = std::process::Command::new(binary())
            .args(["serve", "--foreground"])
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir.path())
            .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn daemon");

        let readiness_file = data_dir
            .path()
            .join("trusty-memory")
            .join("trusty-memory.sock");
        let deadline = Instant::now() + DAEMON_BOOT_TIMEOUT;
        while !readiness_file.exists() {
            assert!(
                Instant::now() < deadline,
                "daemon did not bind its socket within {DAEMON_BOOT_TIMEOUT:?}; expected at {}",
                readiness_file.display()
            );
            std::thread::sleep(POLL_INTERVAL);
        }

        let mut child = tokio::process::Command::new(binary())
            .args(REGISTERED_ARGS)
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir.path())
            .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn `trusty-memory serve`");

        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = child.stdout.take().expect("stdout pipe");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            daemon,
            _data_dir: data_dir,
        }
    }

    /// Write one JSON-RPC request line to the child's stdin.
    async fn send(&mut self, req: &Value) {
        let line = serde_json::to_string(req).expect("serialise request");
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write request");
        self.stdin.flush().await.expect("flush request");
    }

    /// Read one response line, failing the test if the deadline elapses.
    async fn recv(&mut self) -> Value {
        let mut raw = String::new();
        let read = self.reader.read_line(&mut raw);
        let n = timeout(RESPONSE_DEADLINE, read)
            .await
            .expect("response must arrive within the deadline — server hung?")
            .expect("read response line");
        assert!(n > 0, "server closed stdout before responding");
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("stdout must be JSON-RPC only; got {raw:?} ({e})"))
    }

    /// EOF the bridge, wait for it to exit, then kill the daemon.
    async fn close(mut self) {
        drop(self.stdin);
        timeout(EXIT_DEADLINE, self.child.wait())
            .await
            .expect("bridge must exit after stdin EOF")
            .expect("bridge wait");
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Why (#5265): this is the acceptance for the registration contract — the
/// vector Codex is registered with must complete a whole clean MCP session.
/// What: `initialize`, `notifications/initialized` (which must produce NO
/// response line), `tools/list` (which must advertise `get_prompt_context`), and
/// one `get_prompt_context` tool call. Every line read is parsed as JSON, so
/// any banner or log byte on stdout fails the test rather than corrupting the
/// framing silently.
#[tokio::test]
async fn bare_serve_completes_a_clean_mcp_session() {
    let mut session = CodexSession::spawn().await;

    session
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "codex-regression-5265", "version": "0"}
            }
        }))
        .await;
    let init = session.recv().await;
    assert!(
        init.get("result").is_some(),
        "bare `serve` must initialize MCP; got: {init}"
    );

    // A notification must not produce a response. If one leaked, the id below
    // would read 1 instead of 2.
    session
        .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;

    session
        .send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    let listed = session.recv().await;
    assert_eq!(
        listed["id"], 2,
        "a notification must not produce a response line; got: {listed}"
    );
    let tools = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must return a tools array; got: {listed}"));
    assert!(
        tools.iter().any(|t| t["name"] == "get_prompt_context"),
        "tools/list must advertise get_prompt_context; got {} tools",
        tools.len()
    );

    session
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "get_prompt_context", "arguments": {}}
        }))
        .await;
    let called = session.recv().await;
    assert_eq!(called["id"], 3, "response must match the request id");
    assert!(
        called.get("result").is_some(),
        "get_prompt_context must succeed over the registered vector; got: {called}"
    );

    session.close().await;
}
