//! A real MCP stdio session against a freshly-built binary (#5264).
//!
//! Why: #5264 was closed once and reopened the same day. The tests that shipped
//! with the fix — `setup_codex_writes_the_serve_entrypoint`,
//! `setup_codex_is_idempotent` — assert FILE CONTENTS. They prove the argument
//! vector on disk says `serve`; they cannot prove that launching it produces a
//! process that completes an MCP handshake and answers tool calls, which is the
//! property the reporter actually lost. This file removes that gap: it spawns
//! the built binary, speaks JSON-RPC over its stdin and stdout, and asserts the
//! session initializes, lists tools, reports health, discovers an index, and
//! returns a search hit.
//!
//! Hermeticity is the load-bearing constraint, not a nicety. The scoping repro
//! for this issue accidentally probed the machine's PRODUCTION daemon — 42
//! indexes, 430k chunks — and got a green answer back from it. A test that can
//! reach that daemon would pass for the wrong reason and could disturb it, so
//! this file isolates on three axes and then ASSERTS the isolation held:
//!
//! 1. `TRUSTY_DATA_DIR` points at a fresh tempdir, which is what
//!    `service::http_addr_path()` and `daemon_port_path()` resolve against.
//! 2. Both discovery files in that dir are seeded with THIS test's ephemeral
//!    port, so even `daemon_base_url()`'s fallback path cannot reach the
//!    compiled-in default `127.0.0.1:7878`.
//! 3. `HOME` points at a second tempdir, so nothing reads or writes the
//!    operator's real `~/.trusty-search/` or `~/.codex/`.
//!
//! The daemon under test is a real `service::server::build_router` on a
//! loopback port inside this process — the same construction
//! `mcp_structured_503_5350.rs` uses — so the assertions are about the real
//! request path, not a hand-written fixture. `assert_answering_daemon_is_ours`
//! then proves the child talked to it and not to anything else.
//!
//! Not covered here: a `serve` session with NO reachable daemon.
//! `handle_serve` builds its `DaemonBridgeConfig` with `no_spawn: false` and
//! reads no environment override, so such a session spawns a real
//! `start --foreground` daemon which outlives the killed `serve` child — a
//! first draft of that case left one listening on port 7882. There is no
//! hermetic way to drive it until `serve` gains a no-spawn switch. The
//! unreachable-daemon report itself is covered in `mcp/tools/tests_health.rs`.
//!
//! Test: `cargo test -p trusty-search --test mcp_stdio_e2e_5264`

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::RwLock;

use trusty_common::embedder::MockEmbedder;
use trusty_search::core::chunker::{ChunkType, RawChunk};
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::Embedder;
use trusty_search::mcp::tools::HEALTH_OK;
use trusty_search::service::server::{build_router, SearchAppState};

/// Index id this session works with. Deliberately unlike any real project id on
/// a developer machine, so a leak to the production daemon cannot accidentally
/// find a same-named index and make the test pass anyway.
const INDEX_ID: &str = "e2e-5264-isolated-fixture";

/// Distinctive token planted in the fixture's only chunk. A search that returns
/// it proves the hit came from OUR corpus.
const NEEDLE: &str = "zqx_authenticate_5264";

/// Serve the real daemon router with one populated index, on an ephemeral
/// loopback port.
///
/// What: builds a `CodeIndexer` over a `MockEmbedder` — no ONNX, no sidecar, no
/// network — adds one chunk containing [`NEEDLE`], registers it, and serves
/// `build_router` on `127.0.0.1:0`. Returns the bound address.
async fn spawn_isolated_daemon(root: &Path) -> std::net::SocketAddr {
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new(INDEX_ID, root).with_components(embedder, store);

    let rel = "src/auth.rs";
    let content = format!("fn {NEEDLE}(token: &str) -> bool {{ !token.is_empty() }}\n");
    let abs = root.join(rel);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdirs");
    std::fs::write(&abs, &content).expect("write fixture file");

    indexer
        .add_chunk(RawChunk {
            id: "c0".into(),
            file: rel.into(),
            start_line: 1,
            end_line: 2,
            content,
            function_name: Some(NEEDLE.into()),
            language: Some("rust".into()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        })
        .await
        .expect("add_chunk");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    ));

    let app = build_router(SearchAppState::new(registry));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// A live MCP stdio session against the built binary.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    /// Spawn `trusty-search serve` with a fully isolated environment.
    ///
    /// Why: `serve`'s `ensure_search_daemon_up` will SPAWN a daemon if it
    /// cannot reach one, and an un-isolated spawn would bind the real default
    /// port. Seeding both discovery files with the in-process daemon's address
    /// means the reachability probe succeeds on the first try and no child
    /// daemon is ever launched.
    fn spawn(data_dir: &Path, home: &Path, addr: std::net::SocketAddr) -> Self {
        // `service::http_addr_path()` → `$TRUSTY_DATA_DIR/http_addr`;
        // `commands::daemon_utils::daemon_port_path()` → `$TRUSTY_DATA_DIR/daemon.port`.
        // Seeding BOTH closes the fallback that would otherwise resolve the
        // compiled-in default port.
        std::fs::write(data_dir.join("http_addr"), addr.to_string()).expect("seed http_addr");
        std::fs::write(data_dir.join("daemon.port"), addr.port().to_string())
            .expect("seed daemon.port");

        let mut child = Command::new(env!("CARGO_BIN_EXE_trusty-search"))
            .arg("serve")
            .env("TRUSTY_DATA_DIR", data_dir)
            .env("HOME", home)
            // Leave nothing to inherit that could re-pin the session or
            // re-point discovery at the operator's real instance.
            .env_remove("TRUSTY_INDEX")
            .env_remove("TRUSTY_DATA_DIR_OVERRIDE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn trusty-search serve");

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    /// Send one JSON-RPC request and read its response.
    ///
    /// The stdio transport is line-delimited JSON (`trusty_mcp::
    /// run_stdio_loop`), so one line out is exactly one response.
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush request");

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(
            n > 0,
            "the MCP server closed stdout before answering {method} — the process \
             exited instead of speaking MCP, which is the #5264 defect itself"
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response to {method} is not JSON-RPC: {e}\n{line}"))
    }

    /// Call a tool through `tools/call` and return its decoded payload.
    ///
    /// Fails loudly on `isError`, so a tool that reports failure can never be
    /// mistaken for one that returned an empty result.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let resp = self.call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(
            resp.get("error").is_none(),
            "tools/call {name} returned a JSON-RPC error: {resp}"
        );
        let result = &resp["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tools/call {name} has no text content: {result}"));
        assert_ne!(
            result["isError"],
            Value::Bool(true),
            "tools/call {name} reported a tool error: {text}"
        );
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("tools/call {name} payload is not JSON: {e}\n{text}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Prove the session is talking to THIS test's daemon.
///
/// Why: this is the assertion that makes the whole file trustworthy. Without
/// it, a regression in discovery that silently routed the child to the
/// machine-wide daemon would leave every other assertion still passing —
/// exactly the failure mode the scoping repro hit.
fn assert_answering_daemon_is_ours(report: &Value, addr: std::net::SocketAddr) {
    let base = report["daemon"]["base_url"]
        .as_str()
        .expect("the health report names the answering daemon (#5264)");
    assert_eq!(
        base,
        format!("http://{addr}"),
        "the MCP session reached a daemon other than this test's isolated instance"
    );
    assert_ne!(
        addr.port(),
        7878,
        "the ephemeral port must never collide with the default production port"
    );
    let indexes = report["daemon"]["indexes"].as_u64().expect("index count");
    assert_eq!(
        indexes, 1,
        "our isolated daemon serves exactly one index; anything else means this \
         session attached to a shared daemon"
    );
}

/// The full session: handshake, tool discovery, health, index discovery, search.
///
/// This is the criterion-5 regression the reopened #5264 lacked. Every step is
/// driven over a real pipe against a real process; nothing is stubbed on the
/// MCP side.
#[test]
fn mcp_stdio_session_initializes_and_serves_tools_against_an_isolated_daemon() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let home = tempfile::tempdir().expect("home");
    let root = tempfile::tempdir().expect("index root");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let addr = runtime.block_on(spawn_isolated_daemon(root.path()));
    // Hold the runtime for the whole test: dropping it would stop serving.
    let _guard = runtime.enter();

    let mut session = Session::spawn(data_dir.path(), home.path(), addr);

    // 1. initialize — the step the #5264 process never reached.
    let init = session.call(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-5264", "version": "0.0.0" },
        }),
    );
    assert!(init.get("error").is_none(), "initialize failed: {init}");
    assert_eq!(
        init["result"]["serverInfo"]["name"], "trusty-search",
        "initialize must identify the server: {init}"
    );

    // 2. tools/list — a connection that initializes but advertises no tools is
    //    just as useless to the model as one that never started.
    let listed = session.call("tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for required in ["search", "list_indexes", "search_health", "grep"] {
        assert!(
            names.contains(&required),
            "tools/list omits {required}: {names:?}"
        );
    }

    // 3. search_health — reports ok AND names the daemon that answered (#5264).
    let health = session.tool("search_health", json!({ "index_id": INDEX_ID }));
    assert_answering_daemon_is_ours(&health, addr);
    assert_eq!(
        health["status"], HEALTH_OK,
        "health against a populated index must be ok: {health}"
    );
    assert_eq!(health["healthy"], Value::Bool(true));
    assert_eq!(health["index"]["index_id"], INDEX_ID);

    // 4. index discovery — the session finds the index rather than being told.
    let indexes = session.tool("list_indexes", json!({}));
    // The tool forwards `GET /indexes?details=true`, so each element is an
    // object carrying `id` alongside its disk stats.
    let ids: Vec<&str> = indexes["indexes"]
        .as_array()
        .expect("indexes array")
        .iter()
        .filter_map(|e| e["id"].as_str().or_else(|| e.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec![INDEX_ID],
        "an isolated daemon serves only this test's index; a longer list means \
         the session reached a shared daemon"
    );

    // 5. one search, end to end, returning a hit from our own corpus.
    let hits = session.tool(
        "search",
        json!({ "index_id": INDEX_ID, "query": NEEDLE, "top_k": 5 }),
    );
    let results = hits["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "the search returned no rows against a populated index: {hits}"
    );
    assert!(
        results
            .iter()
            .any(|r| r["content"].as_str().is_some_and(|c| c.contains(NEEDLE))),
        "no result carried the fixture's needle, so the hit did not come from \
         this test's corpus: {hits}"
    );
}
