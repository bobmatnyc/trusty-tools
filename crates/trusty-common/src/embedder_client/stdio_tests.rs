//! Unit tests for the stdio embedder client.
//!
//! Why: isolated in a sibling file (declared via `#[path = "stdio_tests.rs"] mod tests;`
//! in `stdio.rs`) to keep `stdio.rs` under the 500-line cap while retaining full
//! test coverage. As a child module, `super::` reaches private items in `stdio`.
//!
//! What: exercises `decode_response`, `reader_task`, and the stall/timeout path
//! without requiring a live `trusty-embedderd` process.
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

/// Regression test for fix #763: the reader task must survive a timeout and
/// continue serving subsequent requests.
///
/// Why: the bug was a `return` in the timeout arm that permanently killed the
/// reader task. All subsequent `embed_batch` calls would then hang forever
/// because `reply_rx.await` had no consumer to deliver to.
///
/// What: drives `reader_task` directly with a controlled `tokio::io::duplex`
/// pipe. First call: delay the response past the timeout, assert the pending
/// oneshot receives `Err(timeout)`. Second call: deliver a valid response
/// immediately — if the reader task is still alive, the second oneshot
/// receives `Ok(embeddings)`. If the old `return` behavior were present, the
/// second `reply_rx.await` would stall forever and the test would time out.
///
/// Old behavior (FAIL): `reader_task` would `return` on timeout, leaving all
/// subsequent `embed_batch` callers hanging forever on `reply_rx.await`.
///
/// New behavior (PASS): `reader_task` drains pending, clears the line buffer,
/// and continues the loop. Subsequent requests still receive responses.
///
/// Test: run with `cargo test -p trusty-common
///   reader_task_survives_timeout_and_serves_next_request`.
#[tokio::test]
async fn reader_task_survives_timeout_and_serves_next_request() {
    use std::collections::VecDeque;
    use tokio::io::{AsyncWriteExt, duplex};
    use tokio::sync::oneshot;

    // Short timeout so the test completes quickly in real time.
    let short_timeout = Duration::from_millis(50);

    // Build a duplex pair: `writer` is the "sidecar stdout" we control;
    // `reader_end` is what the reader task owns.
    let (mut writer, reader_end) = duplex(4096);
    let reader = tokio::io::BufReader::new(reader_end);

    // Set up the shared pending queue.
    let pending: PendingQueue = Arc::new(Mutex::new(VecDeque::new()));
    let pending_clone = Arc::clone(&pending);

    // Spawn the reader task with the injected short timeout.
    let handle = tokio::spawn(reader_task(reader, pending_clone, short_timeout));

    // ── Request 1: push a oneshot, wait for the timeout to fire ───────────
    let (tx1, mut rx1) = oneshot::channel();
    pending.lock().await.push_back(PendingRequest {
        sent: 2,
        reply: tx1,
    });
    // Sleep 3× the timeout so the reader_task's `tokio::time::timeout`
    // fires, drains pending (sends Err to tx1), and continues the loop.
    tokio::time::sleep(short_timeout * 3).await;

    // tx1 must have received Err(Stdio) from the drain.
    let result1 = rx1.try_recv();
    assert!(
        matches!(result1, Ok(Err(EmbedderError::Stdio(_)))),
        "first request after timeout must receive Err(Stdio): got {result1:?}"
    );

    // ── Request 2: write a valid response immediately ──────────────────────
    //
    // First send the "stale" response the sidecar eventually produced for
    // request 1 (its slow ONNX call finished after the parent timed out).
    // The empty-queue guard in reader_task must discard it as a spurious frame.
    let stale =
        b"{\"jsonrpc\":\"2.0\",\"result\":{\"embeddings\":[[0.1,0.2],[0.3,0.4]]},\"id\":1}\n";
    writer.write_all(stale).await.unwrap();
    writer.flush().await.unwrap();

    // Register request 2 and write its response.
    let (tx2, rx2) = oneshot::channel();
    pending.lock().await.push_back(PendingRequest {
        sent: 2,
        reply: tx2,
    });
    let good =
        b"{\"jsonrpc\":\"2.0\",\"result\":{\"embeddings\":[[0.5,0.6],[0.7,0.8]]},\"id\":2}\n";
    writer.write_all(good).await.unwrap();
    writer.flush().await.unwrap();

    // Wait generously for the reader task to process both frames.
    let result2 = tokio::time::timeout(Duration::from_secs(2), rx2)
        .await
        .expect("rx2 timed out — reader task may have exited instead of continuing")
        .expect("rx2 channel closed unexpectedly");
    assert!(
        result2.is_ok(),
        "second request must succeed after reader task survived timeout (#763): \
         got {result2:?}"
    );

    // Clean up: drop the writer to close the pipe → EOF → reader task exits.
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
}
