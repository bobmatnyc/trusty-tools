//! Concurrent embedding integration tests for `trusty-embedderd`.
//!
//! Why: the whole point of `BatchQueue` is to coalesce concurrent requests into
//! fewer ONNX calls, and the socket is the transport those requests arrive on.
//! These tests verify that many concurrent UDS callers all get correct
//! responses through one shared queue.
//!
//! #6289: the HTTP halves of this file — `concurrent_http_requests_all_succeed`
//! and `mixed_http_uds_concurrent_all_succeed`, plus the inline axum router and
//! the hand-rolled mock UDS server they shared — are gone with the `--http`
//! listener ADR-0032 retired. What replaced them is stronger, not just smaller:
//! the crate became a library in #1318, so these tests now drive the REAL
//! `BatchQueue` and the REAL `uds_server::run_uds_accept_loop` instead of
//! re-implementing both.
//!
//! The tests use `MockEmbedder`, so no ONNX model download is required in CI.
//!
//! Test: `cargo test -p trusty-embedderd --test concurrent_embed`.

use std::sync::Arc;

use trusty_common::embedder::{Embedder, MockEmbedder, EMBED_DIM};
use trusty_common::embedder_client::{EmbedderClient, UdsEmbedderClient};
use trusty_embedderd::batch_queue::{BatchConfig, BatchQueue};
use trusty_embedderd::uds_server;

/// How many callers pile onto one queue at once.
///
/// Why: comfortably above the default batch size so the coalescing window has
/// something to coalesce, and below any per-process fd ceiling.
const CONCURRENCY: usize = 50;

/// Bind the daemon's real hardened socket in `dir` and serve it.
///
/// Why: exercising `bind_uds_listener` + `run_uds_accept_loop` is the point —
/// a test-local re-implementation would prove nothing about the daemon.
/// What: returns the socket path; the accept loop runs on a detached task for
/// the lifetime of the test.
/// Test: used by both tests below.
fn serve(dir: &std::path::Path, queue: Arc<BatchQueue>) -> std::path::PathBuf {
    // Short path: `sockaddr_un.sun_path` holds 104 bytes on macOS.
    let sock = dir.join("e.sock");
    let listener = uds_server::bind_uds_listener(&sock).expect("bind hardened socket");
    tokio::spawn(uds_server::run_uds_accept_loop(listener, queue));
    sock
}

/// A real `BatchQueue` over `MockEmbedder` — the production type, no ONNX.
fn mock_queue() -> Arc<BatchQueue> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
    Arc::new(BatchQueue::new(embedder, BatchConfig::default()))
}

#[tokio::test]
async fn concurrent_uds_requests_all_succeed() {
    // Why: 50 concurrent callers on one socket must all receive a correct
    //      embedding — a dropped or crossed reply would corrupt an index
    //      silently rather than failing loudly.
    // What: fire `CONCURRENCY` single-text batches at once through
    //      `UdsEmbedderClient` and assert every response is one vector of
    //      `EMBED_DIM` floats.
    // Test: this test.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = serve(tmp.path(), mock_queue());

    let mut tasks = Vec::with_capacity(CONCURRENCY);
    for i in 0..CONCURRENCY {
        let client = UdsEmbedderClient::new(&sock);
        tasks.push(tokio::spawn(async move {
            client.embed_batch(vec![format!("uds-text-{i}")]).await
        }));
    }

    for (i, task) in tasks.into_iter().enumerate() {
        let vectors = task
            .await
            .expect("embed task did not panic")
            .unwrap_or_else(|e| panic!("request {i} failed: {e}"));
        assert_eq!(vectors.len(), 1, "request {i}: one text in, one vector out");
        assert_eq!(vectors[0].len(), EMBED_DIM, "request {i}: wrong dimension");
    }
}

#[tokio::test]
async fn one_connection_carries_many_sequential_batches() {
    // Why: `handle_connection` reads newline-framed requests in a loop, so a
    //      caller that reuses a socket must get one reply per request in order.
    //      `UdsEmbedderClient` opens a connection per call, so this drives the
    //      accept loop repeatedly against one live server instead.
    // What: ten sequential batches of growing size through one server.
    // Test: this test.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = serve(tmp.path(), mock_queue());
    let client = UdsEmbedderClient::new(&sock);

    for n in 1..=10usize {
        let texts: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
        let vectors = client
            .embed_batch(texts)
            .await
            .unwrap_or_else(|e| panic!("batch of {n} failed: {e}"));
        assert_eq!(vectors.len(), n, "batch of {n}: vector count");
    }
}
