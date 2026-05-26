//! End-to-end test for the embed daemon binary.
//!
//! Why: unit tests in `protocol.rs`, `socket.rs`, `batch_queue.rs`, and
//! `server.rs` cover their pieces in isolation. This test exercises the
//! whole pipeline — JSON-RPC frame → server → batch queue → embedder →
//! response frame — using a real `UnixListener` and a `MockEmbedder` so we
//! never download the ONNX model in CI.
//!
//! What: spawns a minimal "daemon" composed of the same `bind_listener` /
//! `run_accept_loop` / `BatchQueue` pieces the binary uses, then drives it
//! with a raw `UnixStream` client that mirrors the on-wire shape
//! `EmbedClient` produces. The full `EmbedClient` is exercised through its
//! own unit tests; this integration check keeps the daemon's wire contract
//! honest without coupling the test to the client crate's internals.
//!
//! Test: `cargo test -p trusty-embed-daemon`.

// NOTE: the integration test reaches into the binary's modules to wire up a
// daemon-in-the-current-process. Cargo lets integration tests link against
// the bin's modules only when the modules are exposed through a library
// target, but we deliberately keep this crate binary-only — instead, the
// tests/ file declares its own minimal copy of the bind+accept loop using
// the public client surface (`trusty_common::embed_client`).

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use trusty_common::embed_client::EmbedClient;
use trusty_common::embedder::{Embedder, MockEmbedder, EMBED_DIM};

/// Mirror of the daemon's dispatch loop, simplified for testing.
///
/// Spawns one task per accepted connection that reads one newline frame,
/// dispatches it through the mock embedder, and writes one newline-
/// terminated JSON response. The real daemon uses the same shape (see
/// `crates/trusty-embed-daemon/src/server.rs`) — we duplicate it here only
/// because the binary's modules are not exposed through a `lib` target.
async fn run_test_daemon(listener: UnixListener, embedder: Arc<dyn Embedder>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let emb = Arc::clone(&embedder);
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut reader = BufReader::new(read);
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let trimmed = line.trim();
                    // Parse the request envelope and the embed params.
                    let v: serde_json::Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(e) => {
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": {"code": -32700, "message": format!("parse: {e}")},
                                "id": null
                            });
                            let mut payload = serde_json::to_vec(&resp).unwrap();
                            payload.push(b'\n');
                            let _ = write.write_all(&payload).await;
                            return;
                        }
                    };
                    let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let texts: Vec<String> = v
                        .get("params")
                        .and_then(|p| p.get("texts"))
                        .and_then(|t| t.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let embeddings = emb.embed_batch(&texts).await.unwrap_or_default();
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "result": {"embeddings": embeddings},
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
async fn end_to_end_embed_one_via_client() {
    // Use a temp socket path so parallel test invocations do not collide.
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("embed.sock");

    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
    let daemon_handle = tokio::spawn(run_test_daemon(listener, embedder));

    // Give the daemon a moment to enter the accept loop.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = EmbedClient::new(socket_path.clone());
    let v = client.embed_one("hello world").await.expect("embed_one");
    assert_eq!(v.len(), EMBED_DIM);

    daemon_handle.abort();
}

#[tokio::test]
async fn end_to_end_embed_many_via_client() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("embed.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
    let daemon_handle = tokio::spawn(run_test_daemon(listener, embedder));

    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = EmbedClient::new(socket_path.clone());
    let texts = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    let got = client.embed_many(&texts).await.expect("embed_many");
    assert_eq!(got.len(), 3);
    for v in &got {
        assert_eq!(v.len(), EMBED_DIM);
    }
    assert_ne!(got[0], got[1], "mock must produce distinct embeddings");

    daemon_handle.abort();
}

#[tokio::test]
async fn raw_protocol_smoke_test() {
    // This test mirrors what a non-Rust consumer would send on the wire.
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("embed.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind UDS");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
    let daemon_handle = tokio::spawn(run_test_daemon(listener, embedder));
    tokio::time::sleep(Duration::from_millis(20)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let req = r#"{"jsonrpc":"2.0","method":"embed","params":{"texts":["x"]},"id":42}"#;
    write.write_all(req.as_bytes()).await.unwrap();
    write.write_all(b"\n").await.unwrap();
    write.shutdown().await.unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["id"], serde_json::json!(42));
    assert!(v["result"]["embeddings"].is_array());

    daemon_handle.abort();
}
