//! Unit tests for the stdio embedder client.
//!
//! Why: isolated in a sibling file (declared via `#[path = "stdio_tests.rs"] mod tests;`
//! in `stdio.rs`) to keep `stdio.rs` under the 500-line cap while retaining full
//! test coverage. As a child module, `super::` reaches private items in `stdio`.
//!
//! What: exercises `decode_response`, `reader_task`, the stall/timeout path,
//! and the stale-frame misattribution-prevention guarantee.
//!
//! Test: `cargo test -p trusty-common --features embedder-client,embedder-bundled-ort`

use super::*;

// ── Wire format tests (no live process needed) ────────────────────────

#[test]
fn request_serialises_correctly() {
    // Why: guard against accidental rename of JSON-RPC fields; the daemon
    //      parses these names literally.
    // What: serialise a sample request and check required wire fields.
    // Test: this test.
    let texts = vec!["hello".to_string(), "world".to_string()];
    let req = RpcRequest {
        jsonrpc: JSONRPC_VERSION,
        method: METHOD_EMBED,
        params: EmbedParams { texts: &texts },
        id: 1,
    };
    let s = serde_json::to_string(&req).unwrap();
    assert!(s.contains("\"jsonrpc\":\"2.0\""), "must have jsonrpc 2.0");
    assert!(s.contains("\"method\":\"embed\""), "must have embed method");
    assert!(
        s.contains("\"texts\":[\"hello\",\"world\"]"),
        "must include texts"
    );
    assert!(s.contains("\"id\":1"), "must have id");
}

#[test]
fn error_response_maps_to_model_error() {
    // Why: daemon RPC errors must surface as EmbedderError::ModelError so
    //      callers can distinguish them from transport failures.
    // What: decode a synthetic error-response frame and check the variant.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"ort failed"},"id":1}"#;
    let result = decode_response(json, 1);
    assert!(
        matches!(result, Err(EmbedderError::ModelError(_))),
        "got: {result:?}"
    );
}

#[test]
fn success_response_decoded() {
    // Why: verify the happy-path decode path works end-to-end without a
    //      live child process.
    // What: synthesise a success response and deserialise the embeddings.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[[0.1,0.2],[0.3,0.4]]},"id":1}"#;
    let result = decode_response(json, 2).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0][0], 0.1_f32);
}

#[test]
fn count_mismatch_returns_dimension_error() {
    // Why: a count mismatch between sent and received vectors must surface
    //      as DimensionMismatch, not a silent truncation.
    // What: send `sent=3` but the mock response has 2 embeddings.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[[0.1],[0.2]]},"id":1}"#;
    let result = decode_response(json, 3);
    assert!(
        matches!(
            result,
            Err(EmbedderError::DimensionMismatch { sent: 3, got: 2 })
        ),
        "got: {result:?}"
    );
}

// ── extract_response_id unit tests ────────────────────────────────────

#[test]
fn extract_response_id_numeric() {
    // Why: the id-keyed dispatch path depends on correct id extraction.
    // What: numeric id must round-trip as u64.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[]},"id":42}"#;
    assert_eq!(extract_response_id(json), Some(42));
}

#[test]
fn extract_response_id_null_returns_none() {
    // Why: a null id (parse-error fallback from sidecar) must not cause a
    //      lookup panic — it must produce None and be discarded by the caller.
    // What: `id: null` → None.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"parse error"},"id":null}"#;
    assert_eq!(extract_response_id(json), None);
}

#[test]
fn extract_response_id_string_returns_none() {
    // Why: the client always sends numeric ids; a string id from an unexpected
    //      source must produce None rather than a spurious u64.
    // What: `id: "abc"` → None.
    // Test: this test.
    let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[]},"id":"abc"}"#;
    assert_eq!(extract_response_id(json), None);
}

/// Verify that a stalled/silent sidecar reader produces a timeout error
/// rather than blocking indefinitely.
///
/// Why: the root cause of the reindex-stall failure mode is a read blocking
/// forever when the sidecar stops writing. This test proves that
/// `tokio::time::timeout` on a never-yielding `read_line` call returns an
/// `Elapsed` error rather than hanging.
///
/// What: creates a `tokio::io::duplex` reader whose write end is held but
/// never written to. Calls `read_line` with a 1 s deadline and asserts the
/// result is `Err(Elapsed)`. Identical to a stalled sidecar.
///
/// Test: this test (`embed_call_stalled_reader_times_out`).
#[tokio::test]
async fn embed_call_stalled_reader_times_out() {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::duplex;

    let (_tx, rx) = duplex(1024);
    let mut buf = String::new();
    let mut reader = tokio::io::BufReader::new(rx);

    let result = tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut buf)).await;

    assert!(
        result.is_err(),
        "a read_line on a never-writing reader must time out under a 1 s deadline; \
         got: {result:?}"
    );
}

/// Regression test for fix #763: the reader task must survive a timeout AND
/// must not misattribute a stale frame to a new request.
///
/// # Why
///
/// **Bug 1 (task exit):** the original `return` in the timeout arm permanently
/// killed the reader task. All subsequent `embed_batch` calls would then hang
/// forever because `reply_rx.await` had no consumer.
///
/// **Bug 2 (stale-frame misattribution):** even after the task-exit was fixed
/// with a `continue`, the FIFO queue meant that a stale late-arriving response
/// for request A (whose timeout already errored the caller) would be popped
/// and dispatched to request B — the next request to arrive. This silently
/// injects wrong embeddings into the HNSW index, which is worse than an error
/// because it is undetectable (valid-looking but wrong vectors; the zero-vector
/// guard does not catch it).
///
/// # What this test proves
///
/// 1. The reader task stays alive after a timeout.
/// 2. A stale response for a timed-out request is DISCARDED, not dispatched
///    to a subsequent request.
/// 3. The subsequent request eventually receives its own correct response.
///
/// Scenario:
/// - Enqueue request A (id=1) with `sent=2`. The sidecar is silent; the
///   50 ms timeout fires. The reader removes A from the pending map and sends
///   `Err(timeout)` to A's oneshot.
/// - Now the "sidecar" delivers A's stale response (id=1, with A's embeddings).
/// - Enqueue request B (id=2) with `sent=2`. The "sidecar" delivers B's real
///   response (id=2, with B's embeddings).
/// - Assert: B's oneshot receives B's embeddings (not A's). The stale frame
///   for id=1 was discarded because id=1 is no longer in the pending map.
///
/// With FIFO (old): the stale frame for A would be popped and dispatched to B,
/// so B would receive A's embeddings `[[0.1,0.2],[0.3,0.4]]` — silent
/// corruption. This test would FAIL on the old FIFO implementation.
///
/// With id-keyed map (new): the stale frame id=1 is not in the map (it was
/// removed on timeout), so it is discarded; B receives its own correct
/// embeddings `[[0.5,0.6],[0.7,0.8]]`. This test PASSES.
///
/// Test: run with `cargo test -p trusty-common
///   reader_task_survives_timeout_and_serves_next_request`.
#[tokio::test]
async fn reader_task_survives_timeout_and_serves_next_request() {
    use tokio::io::{AsyncWriteExt, duplex};
    use tokio::sync::oneshot;

    // Short timeout so the test completes quickly in real time.
    let short_timeout = Duration::from_millis(50);

    // Build a duplex pair: `writer` is the "sidecar stdout" we control;
    // `reader_end` is what the reader task owns.
    let (mut writer, reader_end) = duplex(4096);
    let reader = tokio::io::BufReader::new(reader_end);

    // Set up the shared pending map.
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_clone = Arc::clone(&pending);

    // Spawn the reader task with the injected short timeout.
    let handle = tokio::spawn(reader_task(reader, pending_clone, short_timeout));

    // ── Request A (id=1): push a oneshot, wait for the timeout to fire ────
    //
    // We manually set id=1 here to match the stale response we'll inject
    // below. In production the client uses AtomicU64, but for this test we
    // drive reader_task directly and control the ids ourselves.
    let (tx_a, mut rx_a) = oneshot::channel();
    pending.lock().await.insert(
        1,
        PendingRequest {
            sent: 2,
            reply: tx_a,
        },
    );
    // Sleep 3× the timeout so the reader_task's `tokio::time::timeout`
    // fires, removes id=1 from the map, sends Err to tx_a, and continues.
    tokio::time::sleep(short_timeout * 3).await;

    // tx_a must have received Err(Stdio) from the timeout drain.
    let result_a = rx_a.try_recv();
    assert!(
        matches!(result_a, Ok(Err(EmbedderError::Stdio(_)))),
        "request A after timeout must receive Err(Stdio): got {result_a:?}"
    );

    // ── Inject stale response for request A (id=1) ────────────────────────
    //
    // This simulates the sidecar eventually finishing its slow ONNX call and
    // emitting A's response AFTER A's timeout already fired. With the id-keyed
    // map, id=1 is no longer in the map so this frame must be DISCARDED.
    // With FIFO (the old implementation) this frame would have been dispatched
    // to request B instead — the misattribution bug.
    let stale_a =
        b"{\"jsonrpc\":\"2.0\",\"result\":{\"embeddings\":[[0.1,0.2],[0.3,0.4]]},\"id\":1}\n";
    writer.write_all(stale_a).await.unwrap();
    writer.flush().await.unwrap();

    // ── Request B (id=2): register and deliver its correct response ────────
    let (tx_b, rx_b) = oneshot::channel();
    pending.lock().await.insert(
        2,
        PendingRequest {
            sent: 2,
            reply: tx_b,
        },
    );
    // B's real response — different embeddings from A's stale frame.
    let real_b =
        b"{\"jsonrpc\":\"2.0\",\"result\":{\"embeddings\":[[0.5,0.6],[0.7,0.8]]},\"id\":2}\n";
    writer.write_all(real_b).await.unwrap();
    writer.flush().await.unwrap();

    // Wait generously for the reader task to process both frames.
    let result_b = tokio::time::timeout(Duration::from_secs(2), rx_b)
        .await
        .expect("rx_b timed out — reader task may have exited instead of continuing")
        .expect("rx_b channel closed unexpectedly");

    assert!(
        result_b.is_ok(),
        "request B must succeed after reader task survived timeout (#763): \
         got {result_b:?}"
    );

    // KEY ASSERTION: B's embeddings must be B's own correct data, NOT A's
    // stale data. With the old FIFO implementation this would fail because
    // the stale frame for A would have been dispatched to B.
    let embeddings_b = result_b.unwrap();
    assert_eq!(
        embeddings_b.len(),
        2,
        "request B must return 2 embedding vectors"
    );
    assert!(
        (embeddings_b[0][0] - 0.5_f32).abs() < 1e-6,
        "request B must receive its OWN embeddings (0.5…), not A's stale \
         embeddings (0.1…) — misattribution bug would put 0.1 here. \
         Got: {:?}",
        embeddings_b[0]
    );
    assert!(
        (embeddings_b[1][0] - 0.7_f32).abs() < 1e-6,
        "request B second vector must be B's own data (0.7…), not A's (0.3…). \
         Got: {:?}",
        embeddings_b[1]
    );

    // Clean up: drop the writer to close the pipe → EOF → reader task exits.
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
}

/// Verify that `timeout_stall_hint` returns provider-appropriate text (issue #857).
///
/// Why: a regression guard ensures that the CUDA hint is never emitted for
/// non-CUDA providers and that each branch returns a non-empty, provider-specific
/// string — so future changes to the hint text cannot silently collapse all
/// variants to the same message or to an empty string.
/// What: calls `timeout_stall_hint` for every `ExecutionProvider` variant and
/// asserts the expected substring is present.
/// Test: this test.
#[test]
fn timeout_stall_hint_is_provider_aware() {
    use crate::embedder::ExecutionProvider;

    let cuda = super::timeout_stall_hint(ExecutionProvider::Cuda);
    assert!(
        cuda.contains("CUDA"),
        "CUDA provider must mention CUDA; got: {cuda:?}"
    );
    assert!(
        !cuda.contains("CoreML"),
        "CUDA hint must not mention CoreML; got: {cuda:?}"
    );

    let coreml = super::timeout_stall_hint(ExecutionProvider::CoreML);
    assert!(
        coreml.contains("CoreML"),
        "CoreML provider must mention CoreML; got: {coreml:?}"
    );
    assert!(
        !coreml.contains("CUDA"),
        "CoreML hint must not mention CUDA; got: {coreml:?}"
    );

    let coreml_ane = super::timeout_stall_hint(ExecutionProvider::CoreMLAne);
    assert!(
        coreml_ane.contains("CoreML"),
        "CoreMLAne provider must mention CoreML; got: {coreml_ane:?}"
    );
    assert!(
        !coreml_ane.contains("CUDA"),
        "CoreMLAne hint must not mention CUDA; got: {coreml_ane:?}"
    );

    let cpu = super::timeout_stall_hint(ExecutionProvider::Cpu);
    assert!(
        !cpu.contains("CUDA"),
        "CPU hint must not mention CUDA; got: {cpu:?}"
    );
    assert!(
        !cpu.contains("CoreML"),
        "CPU hint must not mention CoreML; got: {cpu:?}"
    );
    assert!(!cpu.is_empty(), "CPU hint must not be empty");
}
