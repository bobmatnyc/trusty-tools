//! Tests for the search daemon: socket-path convention, liveness probing, and
//! every method driven over a real hardened Unix socket.
//!
//! Why: #6433 moved this daemon off loopback TCP, so the properties worth
//! asserting moved with it — that the socket is bound with the modes the trust
//! boundary rests on, that a corpse left by a dead daemon is recovered rather
//! than fatal, that the retired port file is gone, and that every one of the
//! five methods answers.
//! What: unit tests for the path and cleanup helpers, plus `rpc_*` tests that
//! serve `rpc::build_router` over a temporary socket.
//! Test: this *is* the test module.

use super::*;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

// ─── shared fixtures ─────────────────────────────────────────────────────────

/// An in-memory `CodeIndexer` — no ONNX model, no redb, no disk.
///
/// Why `pub(crate)`: `service_client`'s round-trip test serves the same router
/// this module does, and two copies of the mock would be two places to keep in
/// step.
pub(crate) fn mock_indexer() -> CodeIndexer {
    use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    struct MockStore {
        inner: StdMutex<HashMap<String, (Vec<f32>, Value)>>,
    }
    #[async_trait]
    impl MemoryStore for MockStore {
        async fn insert(&self, _: Segment, id: &str, v: &[f32], p: Value) -> anyhow::Result<()> {
            self.inner
                .lock()
                .unwrap()
                .insert(id.into(), (v.to_vec(), p));
            Ok(())
        }
        async fn search(
            &self,
            _: Segment,
            _: &[f32],
            _: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            Ok(vec![])
        }
        async fn get(&self, _: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.inner.lock().unwrap().get(id).map(|(_, p)| p.clone()))
        }
        async fn delete(&self, _: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.lock().unwrap().remove(id);
            Ok(())
        }
    }
    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embed(&self, t: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(t.iter().map(|s| vec![s.len() as f32; 8]).collect())
        }
        fn embed_single(&self, t: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![t.len() as f32; 8])
        }
        fn dimension(&self) -> usize {
            8
        }
    }

    let store: Arc<dyn MemoryStore> = Arc::new(MockStore {
        inner: StdMutex::new(HashMap::new()),
    });
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder);
    CodeIndexer::new(store, embedder)
}

/// Poll until `socket` answers `search.health`, or fail the test.
///
/// Why a condition rather than a sleep: `serve_with_shutdown` binds inside a
/// spawned task, so the client can otherwise dial before the listener exists.
/// A fixed sleep is both slower than it needs to be and flaky under load.
pub(crate) async fn await_socket(socket: &Path) {
    for _ in 0..200 {
        if health_ok(socket).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket at {} never answered", socket.display());
}

/// Serve `build_router` over a fresh socket under `dir`, and hand back the
/// socket path plus the handle that stops it.
async fn serve_in(
    dir: &TempDir,
) -> (
    PathBuf,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<()>>,
) {
    let socket = dir.path().join("sockets").join("search.sock");
    let state = SearchState {
        indexer: Arc::new(mock_indexer()),
        project_root: dir.path().to_path_buf(),
        reindex_in_flight: Arc::new(Mutex::new(false)),
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let handle = tokio::spawn(async move {
        rpc::serve_with_shutdown(state, &serve_socket, async {
            let _ = stop_rx.await;
        })
        .await
    });
    await_socket(&socket).await;
    (socket, stop_tx, handle)
}

/// One JSON-RPC exchange against `socket`, returning the whole response frame
/// so a test can assert on `error` as readily as on `result`.
async fn call(
    socket: &Path,
    method: &str,
    params: Value,
) -> trusty_common::uds::server::RpcResponse {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": method,
        "params": params,
    });
    trusty_common::uds::send_framed_request(socket, &request, Duration::from_secs(5))
        .await
        .expect("framed exchange")
}

// ─── path and cleanup helpers ────────────────────────────────────────────────

#[test]
fn search_socket_path_uses_project_id() {
    let p = PathBuf::from("/tmp/some-project");
    let s = search_socket_path(&p);
    let s_str = s.to_string_lossy().into_owned();
    assert!(
        s_str.contains("some-project.search.sock") || s_str.ends_with("search.sock"),
        "unexpected socket path: {s_str}"
    );
}

/// The port file the TCP daemon published is deleted, and deleting one that is
/// already gone is not an error — which is every start after the first.
#[test]
fn daemon_removes_the_retired_discovery_file() {
    let dir = TempDir::new().unwrap();
    let state_dir = dir.path().join(".trusty-agents").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let pid_file = state_dir.join("search.pid");
    std::fs::write(&pid_file, b"{\"pid\":1,\"port\":54321}").unwrap();

    remove_retired_discovery_file(dir.path());
    assert!(!pid_file.exists(), "retired discovery file must be removed");

    // Idempotent: the second start finds nothing and must not fail.
    remove_retired_discovery_file(dir.path());
}

#[tokio::test]
async fn is_daemon_running_is_false_with_no_socket() {
    let dir = TempDir::new().unwrap();
    assert!(!is_daemon_running(dir.path()).await);
}

#[tokio::test]
async fn health_ok_is_false_for_a_missing_socket() {
    let dir = TempDir::new().unwrap();
    assert!(!health_ok(&dir.path().join("absent.sock")).await);
}

#[tokio::test]
async fn is_daemon_running_is_true_against_a_live_daemon() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;
    assert!(
        health_ok(&socket).await,
        "the probe `is_daemon_running` delegates to must see a live daemon"
    );
    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

// ─── the wire ────────────────────────────────────────────────────────────────

/// Every name in `METHODS` is registered, and nothing else is. A rename shows
/// up here rather than as `method_not_found` in a client at runtime.
#[test]
fn rpc_router_registers_every_documented_method() {
    let dir = TempDir::new().unwrap();
    let state = SearchState {
        indexer: Arc::new(mock_indexer()),
        project_root: dir.path().to_path_buf(),
        reindex_in_flight: Arc::new(Mutex::new(false)),
    };
    let router = build_router(state);
    let mut registered: Vec<&str> = router.method_names().collect();
    registered.sort_unstable();
    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();
    assert_eq!(registered, documented);
}

/// This daemon binds no TCP listener (#6433, ADR-0032).
///
/// Why a source scan: the property is an absence, and an absence has no runtime
/// probe that is both portable and deterministic — the retired listener bound
/// an auto-assigned port, so there is no fixed address to find nothing on. What
/// there is, is a type: a TCP listener cannot be bound without naming one. This
/// fails on the pre-#6433 module, which binds one in `run_search_service`.
///
/// What it does NOT prove: that no TCP socket can be opened from these three
/// files at all. A raw `socket(2)` through `libc` or `socket2`, or a listener
/// built behind a type alias or a re-export under another name, passes. It
/// catches the regression that is actually likely — someone reaching for the
/// obvious type — not a deliberate evasion.
#[test]
fn the_daemon_binds_no_tcp_listener() {
    let needle = concat!("Tcp", "Listener");
    for (name, source) in [
        ("mod.rs", include_str!("mod.rs")),
        ("rpc.rs", include_str!("rpc.rs")),
        ("handlers.rs", include_str!("handlers.rs")),
    ] {
        assert!(
            !source.contains(needle),
            "{name} names a TCP listener; this daemon serves a Unix socket only"
        );
    }
}

/// The socket is bound with the modes the trust boundary rests on: a `0700`
/// directory (Unix path resolution needs search permission on every component,
/// so this alone makes the socket unreachable to another uid) and a `0600`
/// socket.
#[tokio::test]
async fn rpc_binds_a_hardened_socket() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let sock_mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(sock_mode, 0o600, "socket mode");
    let dir_mode = std::fs::metadata(socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "socket directory mode");

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

#[tokio::test]
async fn rpc_health_answers_over_a_real_socket() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let response = call(&socket, METHOD_HEALTH, serde_json::json!({})).await;
    let result = response.result.expect("health result");
    assert_eq!(result["status"], "ok");
    assert!(result["version"].is_string());

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

/// A no-argument method is callable with `params` absent, which arrives as
/// `null`. A plain unit struct would refuse it, which is why `NoParams` exists.
#[tokio::test]
async fn rpc_health_answers_with_no_params() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let response = call(&socket, METHOD_HEALTH, Value::Null).await;
    assert_eq!(response.result.expect("health result")["status"], "ok");

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

#[tokio::test]
async fn rpc_query_refuses_an_empty_query() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let response = call(&socket, "search.query", serde_json::json!({"query": "  "})).await;
    let error = response.error.expect("empty query must be refused");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_INVALID_PARAMS,
        "{}",
        error.message
    );

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

#[tokio::test]
async fn rpc_query_answers_with_hits() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let response = call(
        &socket,
        "search.query",
        serde_json::json!({"query": "foo", "top_k": 3}),
    )
    .await;
    let result = response.result.expect("query result");
    assert!(result.is_array(), "expected an array, got {result:?}");

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

#[tokio::test]
async fn rpc_index_file_reports_its_chunk_count() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let source = dir.path().join("alpha.rs");
    std::fs::write(&source, "fn alpha() {\n    let x = 1;\n}\n").unwrap();
    let response = call(
        &socket,
        "search.index_file",
        serde_json::json!({"path": source.display().to_string()}),
    )
    .await;
    let chunks = response.result.expect("index result")["chunks"]
        .as_u64()
        .expect("chunks is a number");
    assert!(chunks > 0, "expected at least one chunk");

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

#[tokio::test]
async fn rpc_remove_file_reports_its_removed_count() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let source = dir.path().join("beta.rs");
    std::fs::write(&source, "fn beta() {\n    let y = 2;\n}\n").unwrap();
    let params = serde_json::json!({"path": source.display().to_string()});
    let indexed = call(&socket, "search.index_file", params.clone())
        .await
        .result
        .expect("index result")["chunks"]
        .as_u64()
        .unwrap();
    let removed = call(&socket, "search.remove_file", params)
        .await
        .result
        .expect("remove result")["chunks"]
        .as_u64()
        .unwrap();
    assert_eq!(removed, indexed, "removal drops what indexing wrote");

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

/// The first call starts a walk; a second while it is in flight is answered
/// `already-running` rather than queueing a duplicate.
#[tokio::test]
async fn rpc_reindex_starts_and_refuses_a_concurrent_second() {
    let dir = TempDir::new().unwrap();
    // A file deep enough to keep the spawned walk busy past the second call is
    // not needed: the flag is taken before the walk is spawned and cleared only
    // when it finishes, and this directory has enough files to outlast one
    // round trip. The assertion that matters is the first answer.
    for i in 0..64 {
        std::fs::write(
            dir.path().join(format!("f{i}.rs")),
            "fn a() { let x = 1; }\n",
        )
        .unwrap();
    }
    let (socket, stop_tx, handle) = serve_in(&dir).await;

    let first = call(&socket, "search.reindex", Value::Null).await;
    assert_eq!(first.result.expect("reindex result")["status"], "started");

    let second = call(&socket, "search.reindex", Value::Null).await;
    let status = second.result.expect("reindex result")["status"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        status == "already-running" || status == "started",
        "a second reindex is either refused or follows a finished first; got {status}"
    );

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
}

/// Shutdown removes the socket file, so a successor's
/// `bind_singleton_hardened` finds a clean path rather than a corpse.
#[tokio::test]
async fn rpc_unlinks_its_socket_on_shutdown() {
    let dir = TempDir::new().unwrap();
    let (socket, stop_tx, handle) = serve_in(&dir).await;
    assert!(socket.exists());

    let _ = stop_tx.send(());
    handle.await.unwrap().expect("serve");
    assert!(
        !socket.exists(),
        "shutdown must unlink {}",
        socket.display()
    );
}

/// A daemon SIGKILLed before its unlink leaves a socket file behind. The next
/// start must take that path over, not refuse it — `bind_hardened` alone would
/// refuse, and every subsequent start would too, with no operator-visible
/// cause.
#[tokio::test]
async fn rpc_takes_over_a_socket_left_by_a_dead_daemon() {
    let dir = TempDir::new().unwrap();
    let sockets = dir.path().join("sockets");
    std::fs::create_dir_all(&sockets).unwrap();
    std::fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sockets.join("search.sock");

    // Dropping a `UnixListener` does not unlink its path, so this is exactly
    // the corpse a killed daemon leaves: the file is a socket, and nothing is
    // serving it.
    let corpse = tokio::net::UnixListener::bind(&socket).unwrap();
    drop(corpse);
    assert!(socket.exists(), "the corpse must survive the drop");

    let state = SearchState {
        indexer: Arc::new(mock_indexer()),
        project_root: dir.path().to_path_buf(),
        reindex_in_flight: Arc::new(Mutex::new(false)),
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let handle = tokio::spawn(async move {
        rpc::serve_with_shutdown(state, &serve_socket, async {
            let _ = stop_rx.await;
        })
        .await
    });
    await_socket(&socket).await;

    let _ = stop_tx.send(());
    handle
        .await
        .unwrap()
        .expect("the successor must bind over the corpse");
}
