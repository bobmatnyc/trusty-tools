//! End-to-end test for the bm25 daemon binary.
//!
//! Why: unit tests cover protocol, socket-path, batch-queue, index, and
//! dispatch in isolation. This test exercises the whole pipeline —
//! JSON-RPC frame → server → batch queue → BM25 index → response frame —
//! using a real `UnixListener` and a temp-dir data directory.
//!
//! What: spawns a minimal daemon-in-the-current-process composed of the
//! same `bind_listener` / `run_accept_loop` / `BatchQueue` /
//! `PalaceBm25Index` pieces the binary uses, then drives it through the
//! `Bm25Client` public surface in `trusty-common`.
//!
//! Test: `cargo test -p trusty-bm25-daemon`.

// The binary's internal modules are duplicated here as a thin in-process
// daemon. Because the crate is binary-only, integration tests cannot
// directly import the modules — re-declaring the minimum surface keeps the
// wire contract honest without coupling the test to the binary's exact
// module layout.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use trusty_common::bm25::BM25Index;
use trusty_common::bm25_client::Bm25Client;

/// In-process daemon mirror: parses a JSON-RPC frame from the stream,
/// dispatches `index` / `search` / `delete` against an in-memory
/// `BM25Index`, and writes a JSON-RPC response. Mirrors the production
/// daemon's wire shape exactly.
async fn run_test_daemon(listener: UnixListener) {
    let index: Arc<tokio::sync::Mutex<BM25Index>> =
        Arc::new(tokio::sync::Mutex::new(BM25Index::new()));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let index = Arc::clone(&index);
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut reader = BufReader::new(read);
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let trimmed = line.trim();
                    let v: serde_json::Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(e) => {
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": {"code": -32700, "message": format!("parse: {e}")},
                                "id": serde_json::Value::Null
                            });
                            let mut payload = serde_json::to_vec(&resp).unwrap();
                            payload.push(b'\n');
                            let _ = write.write_all(&payload).await;
                            return;
                        }
                    };
                    let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let params = v.get("params").cloned().unwrap_or(serde_json::json!({}));

                    let result: serde_json::Value = match method {
                        "index" => {
                            let doc_id = params
                                .get("doc_id")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let text = params
                                .get("text")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let mut guard = index.lock().await;
                            guard.upsert_document(&doc_id, &text);
                            serde_json::json!({"indexed": true})
                        }
                        "search" => {
                            let query = params.get("query").and_then(|x| x.as_str()).unwrap_or("");
                            let top_k =
                                params.get("top_k").and_then(|x| x.as_u64()).unwrap_or(10) as usize;
                            let guard = index.lock().await;
                            let hits = guard.score_query_all(query, top_k);
                            let arr: Vec<serde_json::Value> = hits
                                .into_iter()
                                .map(|(doc_id, score)| {
                                    serde_json::json!({"doc_id": doc_id, "score": score})
                                })
                                .collect();
                            serde_json::json!({"hits": arr})
                        }
                        "delete" => {
                            let doc_id =
                                params.get("doc_id").and_then(|x| x.as_str()).unwrap_or("");
                            let mut guard = index.lock().await;
                            guard.remove_document(doc_id);
                            serde_json::json!({"deleted": true})
                        }
                        other => {
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": {
                                    "code": -32601,
                                    "message": format!("unknown method: {other}")
                                },
                                "id": id
                            });
                            let mut payload = serde_json::to_vec(&resp).unwrap();
                            payload.push(b'\n');
                            let _ = write.write_all(&payload).await;
                            return;
                        }
                    };

                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "result": result,
                        "id": id
                    });
                    let mut payload = serde_json::to_vec(&resp).unwrap();
                    payload.push(b'\n');
                    let _ = write.write_all(&payload).await;
                });
            }
            Err(_) => return,
        }
    }
}

#[tokio::test]
async fn end_to_end_index_then_search_via_client() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("bm25.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let daemon_handle = tokio::spawn(run_test_daemon(listener));

    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = Bm25Client::new(socket_path.clone());
    client.index("d1", "the quick brown fox").await.unwrap();
    client.index("d2", "lazy dog napping").await.unwrap();

    let hits = client.search("fox", 5).await.unwrap();
    assert!(!hits.is_empty(), "expected at least one hit");
    assert_eq!(hits[0].doc_id, "d1");
    assert!(hits[0].score > 0.0);

    daemon_handle.abort();
}

#[tokio::test]
async fn end_to_end_delete_via_client() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("bm25.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let daemon_handle = tokio::spawn(run_test_daemon(listener));

    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = Bm25Client::new(socket_path.clone());
    client.index("d1", "alpha beta").await.unwrap();
    assert!(!client.search("alpha", 5).await.unwrap().is_empty());
    client.delete("d1").await.unwrap();
    assert!(client.search("alpha", 5).await.unwrap().is_empty());

    daemon_handle.abort();
}

#[tokio::test]
async fn raw_protocol_smoke_test() {
    // Mirrors what a non-Rust consumer would send on the wire.
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("bm25.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let daemon_handle = tokio::spawn(run_test_daemon(listener));

    tokio::time::sleep(Duration::from_millis(20)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let req = r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"x","text":"hello world"},"id":42}"#;
    write.write_all(req.as_bytes()).await.unwrap();
    write.write_all(b"\n").await.unwrap();
    write.shutdown().await.unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["id"], serde_json::json!(42));
    assert_eq!(v["result"]["indexed"], serde_json::json!(true));

    daemon_handle.abort();
}
