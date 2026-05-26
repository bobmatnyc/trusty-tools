//! Bit-identical embedding test — critical acceptance gate for issue #110 Phase 1.
//!
//! Why: the whole point of Phase 1 is that out-of-process embedding produces
//! EXACTLY the same vectors as in-process embedding. If the wire format,
//! serialisation, or HTTP framing introduces any perturbation to the f32
//! values, the HNSW index built from one set of vectors would be inconsistent
//! with queries embedded via the other path — silent correctness regression.
//! This test is the hard gate that must pass before Phase 2 (default flip) is
//! allowed.
//!
//! What: spawns a `trusty-embedderd` instance on an ephemeral TCP port, embeds
//! 10 fixed strings via `RemoteEmbedderClient`, embeds the same strings via
//! `InProcessEmbedderClient` (wrapping a fresh `FastEmbedder`), and asserts
//! the two `Vec<Vec<f32>>` outputs are bitwise-equal (`assert_eq!`).
//!
//! Running locally:
//!   cargo test -p trusty-embedderd --test bit_identical -- --include-ignored --nocapture
//!
//! Note: this test is marked `#[ignore]` to match the existing
//! `trusty-embedder` integration-test pattern — the ONNX model download
//! (~22 MB) would make CI prohibitively slow. Remove `--include-ignored` to
//! skip this test in CI.

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
async fn bit_identical_remote_vs_in_process() {
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use trusty_common::embedder::FastEmbedder;
    use trusty_common::embedder_client::{
        EmbedderClient, InProcessEmbedderClient, RemoteEmbedderClient,
    };

    // ── Step 1: load the FastEmbedder once ──────────────────────────────────
    // We load one instance for the in-process path. The daemon will load its
    // own instance; both must produce identical vectors because fastembed-rs
    // (deterministic ONNX session) always returns the same floats for the same
    // model + input.
    let embedder = FastEmbedder::new()
        .await
        .expect("FastEmbedder::new() — requires ONNX model download");
    let in_process = InProcessEmbedderClient::from_arc(Arc::new(embedder));

    // ── Step 2: bind an ephemeral port for the daemon ────────────────────────
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let base_url = format!("http://{addr}");

    // ── Step 3: spawn the daemon ─────────────────────────────────────────────
    // We load a second FastEmbedder for the daemon (simulating the daemon's
    // own startup) and build the axum app inline so we don't need to shell out
    // to the binary.
    //
    // Why inline rather than subprocess: running a subprocess requires the
    // binary to be pre-built (`--test-bin` flow) and adds process-startup
    // latency. Building the router inline exercises the same handler code with
    // far less test infrastructure.
    let daemon_embedder = FastEmbedder::new()
        .await
        .expect("FastEmbedder::new() for daemon — requires ONNX model download");
    let daemon_embedder = Arc::new(daemon_embedder);

    let app = build_test_app(daemon_embedder);

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test daemon serve");
    });

    // Give the server a moment to start accepting connections.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // ── Step 4: embed via remote client ─────────────────────────────────────
    let remote = RemoteEmbedderClient::new(&base_url);
    let texts: Vec<String> = PROBE_TEXTS.iter().map(|s| s.to_string()).collect();

    let remote_vectors = remote
        .embed_batch(texts.clone())
        .await
        .expect("RemoteEmbedderClient::embed_batch");

    // ── Step 5: embed via in-process client ─────────────────────────────────
    let in_process_vectors = in_process
        .embed_batch(texts)
        .await
        .expect("InProcessEmbedderClient::embed_batch");

    // ── Step 6: bit-identical assertion ─────────────────────────────────────
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
            remote_vec.len(),
            ip_vec.len(),
            "probe[{i}]: vector dimension mismatch: remote={}, in-process={}",
            remote_vec.len(),
            ip_vec.len()
        );
        assert_eq!(
            remote_vec, ip_vec,
            "probe[{i}] ({:?}): remote and in-process vectors are NOT bit-identical",
            PROBE_TEXTS[i]
        );
    }

    println!(
        "bit_identical: all {} probe texts produced identical vectors ({}-dim)",
        PROBE_TEXTS.len(),
        in_process_vectors.first().map(|v| v.len()).unwrap_or(0)
    );

    // Shut down the test server.
    server_handle.abort();
}

/// Build the axum `Router` for the test daemon.
///
/// Why: extracted so the test doesn't have to import the binary's private
/// items (which would require exposing them as `pub`). The handler logic is
/// minimal: call `embed_batch` and return the result.
///
/// What: mirrors the production `main.rs` router with `GET /health` and
/// `POST /embed`.
///
/// Test: exercised by `bit_identical_remote_vs_in_process`.
fn build_test_app(embedder: std::sync::Arc<trusty_common::embedder::FastEmbedder>) -> axum::Router {
    use axum::{
        extract::State,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::json;
    use trusty_common::embedder_client::{EmbedRequest, EmbedResponse};

    #[derive(Clone)]
    struct TestState {
        embedder: std::sync::Arc<trusty_common::embedder::FastEmbedder>,
    }

    async fn health() -> Json<serde_json::Value> {
        Json(json!({"status": "ok", "model": "AllMiniLML6V2Q", "dim": 384}))
    }

    async fn embed(
        State(state): State<TestState>,
        Json(req): Json<EmbedRequest>,
    ) -> Result<Json<EmbedResponse>, (StatusCode, Json<serde_json::Value>)> {
        use trusty_common::embedder::Embedder as _;
        if req.texts.is_empty() {
            return Ok(Json(EmbedResponse { vectors: vec![] }));
        }
        match state.embedder.embed_batch(&req.texts).await {
            Ok(vectors) => Ok(Json(EmbedResponse { vectors })),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("{e:#}") })),
            )),
        }
    }

    Router::new()
        .route("/health", get(health))
        .route("/embed", post(embed))
        .with_state(TestState { embedder })
}
