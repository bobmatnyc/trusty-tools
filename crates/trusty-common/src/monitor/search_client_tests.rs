//! Tests for the trusty-search monitor socket client (#9214).
//!
//! Why: a sibling file keeps `search_client.rs` under the 500-SLOC cap.
//! What: the client against `crate::uds_mock`'s stand-in daemon — every
//! surface reaches its socket method, and a missing socket is an error naming
//! the path — plus the pure payload projections.
//! Test: `cargo test -p trusty-common --features monitor-tui -- search_client::tests`.

use std::sync::{Arc, Mutex};

use super::*;
use crate::uds_mock::{self, MockFuture};

/// Methods a mock daemon was called with, in order.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A handler answering each monitor method the way the daemon does, and
/// recording every call.
fn monitor_handler(calls: Calls) -> impl Fn(&str, Value) -> MockFuture + Send + Sync + 'static {
    move |method: &str, params: Value| {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((method.to_string(), params));
        let answer = match method {
            "search.health" => json!({ "version": "9.9.9", "uptime_secs": 5 }),
            "search.indexes.list" => json!({ "indexes": ["demo"] }),
            "search.index.status" => json!({ "root_path": "/tmp/demo", "chunk_count": 3 }),
            "search.graph.stats" => json!({ "node_count": 1, "edge_count": 2, "edge_kinds": {} }),
            "search.logs.tail" => json!({ "lines": ["a", "b"], "total": 2 }),
            "search.query" => json!({ "results": [{ "file": "f.rs", "start_line": 4 }] }),
            _ => json!({ "queued": true }),
        };
        let reply: MockFuture = Box::pin(async move { Ok(answer) });
        reply
    }
}

/// The method names a recorder saw.
fn methods(calls: &Calls) -> Vec<String> {
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    calls.iter().map(|(m, _)| m.clone()).collect()
}

/// #9214: `TRUSTY_SEARCH_SOCKET` steers the client to a fake daemon, and
/// `fetch_all` reads health, the list, status and graph stats over it — with
/// no `communities` call (#6382).
///
/// Sync, holding `crate::data_dir::ENV_LOCK` (the lock every
/// `TRUSTY_SEARCH_SOCKET` writer in this crate shares) across its own runtime.
#[test]
fn fetch_all_reads_every_surface_over_the_socket() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let calls = Calls::default();
    let daemon = runtime.block_on(uds_mock::spawn(monitor_handler(calls.clone())));
    // SAFETY: guarded by ENV_LOCK; removed below before asserting.
    unsafe {
        std::env::set_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV, daemon.socket());
    }
    let result = SearchClient::resolve().map(|c| runtime.block_on(c.fetch_all()));
    unsafe {
        std::env::remove_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
    }
    let data = result.expect("resolve").expect("fetch_all over the socket");
    assert_eq!(data.version, "9.9.9");
    assert_eq!(data.indexes.len(), 1);
    assert_eq!(data.indexes[0].chunk_count, 3);
    assert_eq!(data.indexes[0].edge_count, 2);
    assert_eq!(
        methods(&calls),
        [
            "search.health",
            "search.indexes.list",
            "search.index.status",
            "search.graph.stats"
        ]
    );
}

/// The env override is what [`resolve_search_socket`] returns.
#[test]
fn resolve_search_socket_honours_the_env_override() {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // SAFETY: guarded by ENV_LOCK; removed below before asserting.
    unsafe {
        std::env::set_var(
            crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV,
            "/tmp/x-9214.sock",
        );
    }
    let resolved = resolve_search_socket();
    unsafe {
        std::env::remove_var(crate::search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
    }
    assert_eq!(
        resolved.expect("resolves"),
        PathBuf::from("/tmp/x-9214.sock")
    );
}

/// #9214: no daemon on the socket is an error naming the socket path, never a
/// dial of a default TCP address.
#[tokio::test]
async fn fetch_all_names_the_socket_when_no_daemon_answers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let err = SearchClient::new(&socket)
        .fetch_all()
        .await
        .expect_err("no daemon answers");
    assert!(
        format!("{err:#}").contains(&socket.display().to_string()),
        "{err:#}"
    );
}

/// #9214: the log tail reads `search.logs.tail`, and a missing socket is an
/// error rather than an empty tail.
#[tokio::test]
async fn logs_tail_reads_the_socket_and_fails_when_it_is_absent() {
    let calls = Calls::default();
    let daemon = uds_mock::spawn(monitor_handler(calls.clone())).await;
    let lines = SearchClient::new(daemon.socket())
        .logs_tail(50)
        .await
        .expect("tail");
    assert_eq!(lines, ["a", "b"]);
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner())[0].1,
        json!({ "n": 50 })
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let err = SearchClient::new(&socket)
        .logs_tail(50)
        .await
        .expect_err("absent socket");
    assert!(
        format!("{err:#}").contains(&socket.display().to_string()),
        "{err:#}"
    );
}

/// #9214: `reindex` and `search` call their socket methods with the index
/// envelope the daemon decodes.
#[tokio::test]
async fn reindex_and_search_call_their_socket_methods() {
    let calls = Calls::default();
    let daemon = uds_mock::spawn(monitor_handler(calls.clone())).await;
    let client = SearchClient::new(daemon.socket());
    client.reindex("demo").await.expect("reindex");
    let hits = client.search("demo", "q", 5).await.expect("search");
    assert_eq!(hits.len(), 1);
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        calls[0],
        ("search.index.reindex".into(), json!({ "index_id": "demo" }))
    );
    assert_eq!(
        calls[1],
        (
            "search.query".into(),
            json!({ "index_id": "demo", "body": { "text": "q", "top_k": 5 } })
        )
    );
}

/// #9214: a reindex against a missing socket ends in one `Failed` event that
/// names the socket.
#[tokio::test]
async fn reindex_stream_reports_a_missing_socket_as_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    SearchClient::new(&socket).reindex_stream("demo", tx).await;
    match rx.recv().await {
        Some(ReindexEvent::Failed(msg)) => {
            assert!(msg.contains(&socket.display().to_string()), "{msg}");
        }
        other => panic!("expected a Failed event, got {other:?}"),
    }
}

#[test]
fn parse_search_hits_projects_fields() {
    let raw = json!({
        "results": [
            {
                "file": "src/lib.rs",
                "start_line": 42,
                "compact_snippet": "fn embed() {\n  ...\n}",
                "content": "ignored when compact present",
            },
            { "file": "src/main.rs", "start_line": 7, "content": "  fn main() {}\nmore" },
        ],
    });
    let hits = parse_search_hits(&raw);
    assert_eq!(hits.len(), 2);
    assert_eq!((hits[0].file.as_str(), hits[0].line), ("src/lib.rs", 42));
    assert_eq!(hits[0].snippet, "fn embed() {");
    assert_eq!(hits[1].snippet, "fn main() {}");
    assert!(parse_search_hits(&json!({})).is_empty());
}

#[test]
fn parse_reindex_event_maps_event_field() {
    let started = parse_reindex_event(&json!({ "event": "start", "total_files": 1200 }));
    assert_eq!(started, ReindexEvent::Started { total_files: 1200 });
    let progress =
        parse_reindex_event(&json!({ "event": "batch", "indexed": 500, "total_files": 1200 }));
    assert_eq!(
        progress,
        ReindexEvent::Progress {
            indexed: 500,
            total_files: 1200
        }
    );
    let complete = parse_reindex_event(
        &json!({ "event": "complete", "total_chunks": 19012, "status": "complete" }),
    );
    assert_eq!(
        complete,
        ReindexEvent::Complete {
            total_chunks: 19012,
            status: "complete".into()
        }
    );
    let failed = parse_reindex_event(&json!({ "event": "error", "message": "denied" }));
    assert_eq!(failed, ReindexEvent::Failed("denied".into()));
}
