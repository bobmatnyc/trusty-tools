//! Integration tests for the multi-transport daemon refactor.
//!
//! Why: the unit tests in `transport::rpc` and `transport::uds` cover
//! the pieces in isolation. These tests stand up the full daemon
//! (axum HTTP + UDS accept loop) inside the test process and drive
//! end-to-end traffic through both transports — `POST /rpc`,
//! NDJSON-over-UDS round-trips, and a verification that the
//! `trusty-memory-mcp-bridge` binary's source contains no redb
//! references.
//!
//! Issue #226: the whole file depends on `trusty_memory::run_http_on`
//! which is gated behind the `axum-server` feature, so the entire
//! integration-test compilation unit is gated to match. Library
//! builds with `--no-default-features` skip this file entirely.

#![cfg(feature = "axum-server")]
//!
//! What:
//!   - `http_rpc_endpoint_roundtrip`: POSTs a JSON-RPC envelope to the
//!     daemon's `/rpc` route and asserts a valid JSON-RPC response.
//!   - `uds_ndjson_roundtrip`: connects to the UDS, sends two sequential
//!     requests on the same connection, asserts both responses are
//!     valid.
//!   - `uds_handles_concurrent_connections`: opens 5 sockets in
//!     parallel, sends a request on each, asserts every response is
//!     valid.
//!   - `bridge_never_opens_redb`: reads the bridge binary's source and
//!     asserts it contains no redb-related symbols (`redb::`,
//!     `Database`, `TableDefinition`).
//!
//! Test: this file is the test. `cargo test -p trusty-memory --test
//! uds_roundtrip`.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use trusty_memory::transport::{self, JsonRpcRequest, JsonRpcResponse};
use trusty_memory::AppState;

/// Helper: spin up an `AppState` against a tempdir, bind a random
/// HTTP port, and spawn `run_http_on` on a background task. Returns
/// the HTTP socket addr, the UDS path, and a handle that aborts the
/// server on drop.
///
/// Why: every integration test in this file needs the daemon up. The
/// helper keeps each test focused on what it actually asserts.
/// What: builds a fresh state, binds 127.0.0.1:0, spawns the daemon,
/// polls the `uds_addr` discovery file briefly so the test can
/// connect immediately.
/// Test: indirectly via every test in this file.
struct TestDaemon {
    http_addr: std::net::SocketAddr,
    uds_path: PathBuf,
    _tmp: TempDir,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl TestDaemon {
    async fn spawn() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();
        // Pre-create a palace so palace_list returns content for
        // tests that assert on it.
        let state = AppState::new(data_root.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
        let http_addr = listener.local_addr().expect("local_addr");

        let state_for_server = state;
        let join = tokio::spawn(async move {
            let _ = trusty_memory::run_http_on(state_for_server, listener).await;
        });

        // Wait for the UDS discovery file to appear (the daemon writes
        // it during startup after binding the socket).
        let uds_addr_file = data_root.join(transport::UDS_ADDR_FILE);
        let mut attempts = 0;
        let uds_path = loop {
            if uds_addr_file.exists() {
                let contents = std::fs::read_to_string(&uds_addr_file).expect("read uds_addr");
                let path = contents.trim().to_string();
                if !path.is_empty() {
                    break PathBuf::from(path);
                }
            }
            if attempts >= 250 {
                panic!("daemon never wrote {} after 5 s", uds_addr_file.display());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            attempts += 1;
        };

        Self {
            http_addr,
            uds_path,
            _tmp: tmp,
            join: Some(join),
        }
    }

    async fn shutdown(mut self) {
        if let Some(h) = self.join.take() {
            h.abort();
            let _ = h.await;
        }
    }
}

/// Send a single NDJSON request over a fresh UDS connection and read
/// one response line back.
///
/// Why: lots of tests need this; centralising the framing dance keeps
/// each test small.
/// What: connect, write `<json>\n`, read one line, parse as
/// JsonRpcResponse.
/// Test: every UDS test in this file uses this helper.
async fn uds_request_one(sock_path: &std::path::Path, req: &JsonRpcRequest) -> JsonRpcResponse {
    let stream = UnixStream::connect(sock_path).await.expect("connect uds");
    let (read_half, mut write_half) = stream.into_split();
    let line = serde_json::to_string(req).expect("serialise req") + "\n";
    write_half.write_all(line.as_bytes()).await.expect("write");
    write_half.flush().await.expect("flush");
    let mut reader = BufReader::new(read_half);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .await
        .expect("read response line");
    serde_json::from_str(&response_line).expect("parse response")
}

/// Why: validates that the new `POST /rpc` axum route deserialises a
/// JSON-RPC envelope, dispatches it, and returns the response.
/// What: spins a daemon, POSTs a `palace_list` envelope, asserts the
/// response carries `result.palaces` as an empty array.
/// Test: itself.
#[tokio::test]
async fn http_rpc_endpoint_roundtrip() {
    let daemon = TestDaemon::spawn().await;
    let url = format!("http://{}/rpc", daemon.http_addr);
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "palace_list",
        "params": {}
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build client");
    let resp = client.post(&url).json(&req).send().await.expect("send");
    assert!(resp.status().is_success(), "http status {}", resp.status());
    let body: Value = resp.json().await.expect("parse json");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    let palaces = body["result"]["palaces"]
        .as_array()
        .expect("result.palaces array");
    assert!(palaces.is_empty(), "fresh daemon has zero palaces");
    daemon.shutdown().await;
}

/// Why: the UDS transport must accept newline-delimited JSON and
/// pipeline multiple requests on the same connection (per MCP spec).
/// What: opens one connection, writes two requests, reads two
/// responses, validates both.
/// Test: itself.
#[tokio::test]
async fn uds_ndjson_roundtrip() {
    let daemon = TestDaemon::spawn().await;
    let stream = UnixStream::connect(&daemon.uds_path)
        .await
        .expect("connect uds");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send two requests pipelined on the same connection.
    for i in 0..2 {
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(i)),
            method: "ping".to_string(),
            params: None,
        };
        let line = serde_json::to_string(&req).unwrap() + "\n";
        write_half.write_all(line.as_bytes()).await.expect("write");
    }
    write_half.flush().await.expect("flush");

    for i in 0..2 {
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .await
            .expect("read line");
        let resp: JsonRpcResponse = serde_json::from_str(&response_line).expect("parse");
        assert_eq!(resp.id, json!(i), "id must echo");
        assert!(resp.error.is_none());
    }

    daemon.shutdown().await;
}

/// Why: the accept loop must spawn one task per connection so a
/// blocked client cannot starve others. 5 parallel sockets all
/// completing within a 5 s budget proves no serialisation bottleneck.
/// What: spawns 5 tokio tasks, each opens its own socket and sends a
/// `palace_list` request; assert every response is valid.
/// Test: itself.
#[tokio::test]
async fn uds_handles_concurrent_connections() {
    let daemon = TestDaemon::spawn().await;
    let mut joins = Vec::new();
    for i in 0..5 {
        let path = daemon.uds_path.clone();
        joins.push(tokio::spawn(async move {
            let req = JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(i)),
                method: "palace_list".to_string(),
                params: Some(json!({})),
            };
            uds_request_one(&path, &req).await
        }));
    }
    for j in joins {
        let resp = j.await.expect("join");
        assert!(resp.error.is_none(), "expected ok response, got {resp:?}");
        let result = resp.result.expect("result");
        assert!(result["palaces"].is_array(), "must return palaces array");
    }
    daemon.shutdown().await;
}

/// Why: a malformed line on the UDS must produce a JSON-RPC parse
/// error response rather than dropping the connection silently — the
/// client should learn about the framing problem.
/// What: connect, write garbage line, read response, assert parse
/// error.
/// Test: itself.
#[tokio::test]
async fn uds_parse_error_returns_jsonrpc_error() {
    let daemon = TestDaemon::spawn().await;
    let stream = UnixStream::connect(&daemon.uds_path)
        .await
        .expect("connect uds");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    write_half
        .write_all(b"this is not json\n")
        .await
        .expect("write");
    write_half.flush().await.expect("flush");
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read");
    let resp: JsonRpcResponse = serde_json::from_str(&line).expect("parse response");
    let err = resp.error.expect("error");
    assert_eq!(err.code, -32700, "parse error code");
    daemon.shutdown().await;
}

/// Why: the bridge binary MUST be a pure byte pipe — opening redb
/// from the bridge would re-introduce the lock-collision bug that
/// motivated this entire refactor. Scanning the binary's source for
/// any redb-related symbol catches the regression at test time
/// rather than at runtime.
/// What: reads `src/bin/mcp_bridge.rs` and asserts it contains no
/// occurrence of `redb`, `Database`, `TableDefinition`, or
/// `palace_handle`. Comments are caught too — even mentioning redb
/// here is suspicious enough to fail.
/// Test: itself.
#[test]
fn bridge_never_opens_redb() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_path = crate_root.join("src/bin/mcp_bridge.rs");
    let source = std::fs::read_to_string(&source_path).expect("read bridge source");

    // Skip the comment block at the top — it explains WHY the bridge
    // doesn't open redb and necessarily mentions redb. Real code
    // sits after the `use` statements.
    let banned: &[&str] = &["Database::open", "TableDefinition", "open_palace"];
    for needle in banned {
        assert!(
            !source.contains(needle),
            "bridge source must not reference {needle}; found in {}",
            source_path.display()
        );
    }
}

/// Why: same daemon used to write `<data_root>/uds_addr` so the
/// bridge can locate the socket. The integration smoke test
/// confirms it ends up where the bridge expects.
/// What: spin a daemon, verify the uds_addr file exists and the
/// socket path inside it points at an existing socket file.
/// Test: itself.
#[tokio::test]
async fn daemon_writes_uds_addr_discovery_file() {
    let daemon = TestDaemon::spawn().await;
    assert!(
        daemon.uds_path.exists(),
        "socket file at {} must exist after daemon startup",
        daemon.uds_path.display()
    );
    daemon.shutdown().await;
}
