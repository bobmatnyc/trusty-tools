//! Bit-identical embedding test — critical acceptance gate for issue #110 Phase 1.
//!
//! Why: the whole point of out-of-process embedding is that it produces EXACTLY
//! the same vectors as in-process embedding. If the wire format, serialisation,
//! or framing introduces any perturbation to the f32 values, the HNSW index
//! built from one set of vectors would be inconsistent with queries embedded via
//! the other path — a silent correctness regression.
//!
//! #6289: this used to compare `RemoteEmbedderClient` (HTTP) against
//! `InProcessEmbedderClient` through an inline axum router. ADR-0032 retired
//! that listener and its client, so the remote half is now `UdsEmbedderClient`
//! against the daemon's own `uds_server` — the transport that actually ships.
//!
//! What: binds the daemon's real hardened socket, serves it with the real
//! accept loop over a real `BatchQueue` holding a `FastEmbedder`, embeds 10
//! fixed strings through `UdsEmbedderClient`, embeds the same strings through
//! `InProcessEmbedderClient` (wrapping a second `FastEmbedder`), and asserts the
//! two `Vec<Vec<f32>>` outputs are bitwise-equal.
//!
//! Running locally:
//!   cargo test -p trusty-embedderd --test bit_identical -- --include-ignored --nocapture
//!
//! Note: marked `#[ignore]` because the ONNX model download (~22 MB) would make
//! CI prohibitively slow. It is the one test in this crate that needs ONNX; the
//! socket's modes, reachability and failure modes are proven without it in
//! `tests/no_tcp_listener.rs`.

/// The 10 fixed probe strings.
///
/// Why: fixed strings ensure the test is deterministic across runs. We use
/// a mix of code-like and natural-language inputs to cover the typical
/// embedding distribution.
const PROBE_TEXTS: &[&str] = &[
    "fn authenticate(token: &str) -> Result<User, AuthError>",
    "pub struct CodeChunk { pub id: String, pub content: String }",
    "how does the embedding pipeline work",
    "SELECT * FROM users WHERE id = ?",
    "import { useState, useEffect } from 'react'",
    "def parse_ast(source: str) -> Node:",
    "trusty-embedderd standalone ONNX embedding daemon",
    "BatchNormalization followed by ReLU activation",
    "git log --oneline -10 HEAD",
    "",
];

#[ignore = "requires ONNX model download (~22 MB); run with --include-ignored"]
#[tokio::test]
async fn uds_bit_identical_vs_in_process() {
    use std::sync::Arc;
    use trusty_common::embedder::{Embedder, FastEmbedder};
    use trusty_common::embedder_client::{
        EmbedderClient, InProcessEmbedderClient, UdsEmbedderClient,
    };
    use trusty_embedderd::batch_queue::{BatchConfig, BatchQueue};
    use trusty_embedderd::uds_server;

    // ── Step 1: load the FastEmbedder for the in-process path ───────────────
    let embedder = FastEmbedder::new()
        .await
        .expect("FastEmbedder::new() — requires ONNX model download");
    let in_process = InProcessEmbedderClient::from_arc(Arc::new(embedder));

    // ── Step 2: stand up the daemon's real socket over a second embedder ────
    // Two instances, as in production (parent and sidecar each load their own).
    // fastembed-rs runs a deterministic ONNX session, so identical input must
    // yield identical floats from either instance.
    let daemon_embedder = FastEmbedder::new()
        .await
        .expect("FastEmbedder::new() for daemon — requires ONNX model download");
    let daemon_embedder: Arc<dyn Embedder> = Arc::new(daemon_embedder);
    let queue = Arc::new(BatchQueue::new(daemon_embedder, BatchConfig::default()));

    let tmp = tempfile::tempdir().expect("tempdir");
    // Short path: `sockaddr_un.sun_path` holds 104 bytes on macOS.
    let sock = tmp.path().join("e.sock");
    let listener = uds_server::bind_uds_listener(&sock).expect("bind hardened socket");
    tokio::spawn(uds_server::run_uds_accept_loop(listener, queue));

    // ── Step 3: embed via the socket client ─────────────────────────────────
    let texts: Vec<String> = PROBE_TEXTS.iter().map(|s| (*s).to_string()).collect();
    let remote_vectors = UdsEmbedderClient::new(&sock)
        .embed_batch(texts.clone())
        .await
        .expect("UdsEmbedderClient::embed_batch");

    // ── Step 4: embed via the in-process client ─────────────────────────────
    let in_process_vectors = in_process
        .embed_batch(texts)
        .await
        .expect("InProcessEmbedderClient::embed_batch");

    // ── Step 5: bit-identical assertion ─────────────────────────────────────
    assert_eq!(
        remote_vectors.len(),
        in_process_vectors.len(),
        "vector count mismatch: remote={}, in-process={}",
        remote_vectors.len(),
        in_process_vectors.len()
    );

    for (i, (remote_vec, ip_vec)) in remote_vectors
        .iter()
        .zip(in_process_vectors.iter())
        .enumerate()
    {
        assert_eq!(
            remote_vec, ip_vec,
            "probe {i} ({:?}) differs between the UDS and in-process paths",
            PROBE_TEXTS[i]
        );
    }
}
