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
    let (unhealthy_tx, _unhealthy_rx) = watch::channel(false);
    let timeout_tracker = Arc::new(TimeoutTracker::new());
    let handle = tokio::spawn(reader_task(
        reader,
        pending_clone,
        short_timeout,
        timeout_tracker,
        unhealthy_tx,
    ));

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

// ── #1448: reader-task death detection ──────────────────────────────────

/// A reader that panics on first poll — simulates the reader task itself
/// panicking mid-read (e.g. a frame-parsing bug) so the monitor task's
/// `JoinHandle`-based death detection (#1448) can be exercised without a
/// real child process.
struct PanicReader;

impl tokio::io::AsyncRead for PanicReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("simulated reader task panic (#1448 regression test)");
    }
}

impl tokio::io::AsyncBufRead for PanicReader {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<&[u8]>> {
        panic!("simulated reader task panic (#1448 regression test)");
    }

    fn consume(self: std::pin::Pin<&mut Self>, _amt: usize) {}
}

/// Regression test for #1448: if the reader task panics, pending
/// `embed_batch` callers must fail fast instead of hanging forever, and the
/// unhealthy signal must fire.
///
/// Why: prior to #1448 the reader task was spawned via a bare fire-and-forget
/// `tokio::spawn` with no retained `JoinHandle` and no panic detection. A
/// panic left every pending oneshot orphaned (never resolved) — callers
/// blocked forever on `reply_rx.await`, and the process-alive-but-dead-reader
/// condition was invisible to the supervisor (which only watches
/// `child.wait()`).
///
/// What: registers a pending request directly in the shared map, then drives
/// `spawn_reader_with_monitor` over a `PanicReader` (panics on first poll)
/// with a deliberately long per-call timeout (30s) so any pass here can only
/// be explained by the panic-drain path, not an individual timeout race.
/// Asserts the pending oneshot resolves to `Err` within 2s and that
/// `unhealthy_signal` observes `true`.
///
/// Test: this test.
#[tokio::test]
async fn reader_panic_drains_pending_and_signals_unhealthy() {
    use tokio::sync::oneshot;

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel();
    pending
        .lock()
        .await
        .insert(1, PendingRequest { sent: 1, reply: tx });

    // Deliberately long — proves the fast resolution below comes from the
    // panic-drain path, not a coincidental per-call timeout.
    let long_timeout = Duration::from_secs(30);
    let (unhealthy_tx, mut unhealthy_rx) =
        spawn_reader_with_monitor(PanicReader, Arc::clone(&pending), long_timeout);
    // Mirrors `_unhealthy_rx_keepalive` on the real client: keep the sender
    // alive so `send(true)` in the monitor task doesn't silently no-op
    // against a zero-receiver channel.
    let _keepalive_tx = unhealthy_tx;

    let result = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("pending request must resolve promptly after reader panic, not hang")
        .expect("oneshot must not be dropped without a value");
    assert!(
        matches!(result, Err(EmbedderError::Stdio(_))),
        "pending request must fail with EmbedderError::Stdio after reader panic: got {result:?}"
    );

    tokio::time::timeout(Duration::from_secs(2), unhealthy_rx.changed())
        .await
        .expect("unhealthy_signal must fire promptly after reader panic")
        .expect("unhealthy watch channel must not close without a value");
    assert!(
        *unhealthy_rx.borrow(),
        "unhealthy_signal must be true after reader panic"
    );
}

/// Regression test for #1448: a normal reader-task exit (EOF) must also flip
/// `unhealthy_signal`, so the supervisor's restart path is not solely
/// dependent on `child.wait()` returning around the same time.
///
/// Why: `EmbedderSupervisor` selects on both `child.wait()` and
/// `unhealthy_signal` (see `supervisor.rs`); the signal must fire on every
/// reader-task exit path, not only the panic path, so a future refactor that
/// adds another exit arm to `reader_task` cannot silently forget to signal.
///
/// What: closes the writer half of a duplex pair (EOF) and asserts
/// `unhealthy_signal` observes `true`.
///
/// Test: this test.
#[tokio::test]
async fn reader_eof_exit_also_signals_unhealthy() {
    use tokio::io::duplex;

    let (writer, reader_end) = duplex(64);
    let reader = tokio::io::BufReader::new(reader_end);
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    let (unhealthy_tx, mut unhealthy_rx) =
        spawn_reader_with_monitor(reader, Arc::clone(&pending), Duration::from_secs(30));
    let _keepalive_tx = unhealthy_tx;

    // Close the write end — the reader task sees EOF, drains (nothing
    // pending here), and returns normally.
    drop(writer);

    tokio::time::timeout(Duration::from_secs(2), unhealthy_rx.changed())
        .await
        .expect("unhealthy_signal must fire after a normal EOF reader exit")
        .expect("unhealthy watch channel must not close without a value");
    assert!(
        *unhealthy_rx.borrow(),
        "unhealthy_signal must be true after EOF reader exit"
    );
}

// ── #1450: wedged-but-alive sidecar detection ───────────────────────────

/// Pure unit tests for `TimeoutTracker` — deterministic, no async/timing.
///
/// Why: the async, real-timeout-driven test below
/// (`wedge_threshold_fires_after_consecutive_timeouts`) proves `reader_task`
/// wires `TimeoutTracker` correctly, but threshold-boundary and reset
/// semantics are best proven directly against the counter, without any
/// scheduler timing involved.
/// Test: this test.
#[test]
fn timeout_tracker_fires_exactly_at_threshold() {
    let tracker = TimeoutTracker::new();
    for i in 0..WEDGED_TIMEOUT_THRESHOLD - 1 {
        assert!(
            !tracker.record_timeout(),
            "must not fire before the threshold (call {i})"
        );
    }
    assert!(
        tracker.record_timeout(),
        "must fire exactly on the call that reaches WEDGED_TIMEOUT_THRESHOLD"
    );
}

/// `TimeoutTracker::record_success` resets the consecutive-timeout count.
///
/// Test: this test.
#[test]
fn timeout_tracker_record_success_resets_count() {
    let tracker = TimeoutTracker::new();
    for _ in 0..WEDGED_TIMEOUT_THRESHOLD - 1 {
        assert!(!tracker.record_timeout());
    }
    tracker.record_success();
    // After a reset, it must again take the full threshold to fire.
    for i in 0..WEDGED_TIMEOUT_THRESHOLD - 1 {
        assert!(
            !tracker.record_timeout(),
            "must not fire before the threshold post-reset (call {i})"
        );
    }
    assert!(
        tracker.record_timeout(),
        "must fire after a fresh threshold run"
    );
}

/// Regression test for #1450: `WEDGED_TIMEOUT_THRESHOLD` consecutive real
/// (non-idle) reader-task call timeouts — with the sidecar never replying at
/// all — must flip `unhealthy_signal` to `true`, and the reader task must
/// stay alive throughout (fix #763 invariant: a mere timeout must never kill
/// the reader task; only the supervisor acts on the signal).
///
/// Why: a wedged-but-alive sidecar (process running, but not responding —
/// the CUDA BFCArena stall from #1428, or the CoreML/ANE scheduling stall
/// reported on #1450) never exits, so `EmbedderSupervisor`'s process-exit-
/// based restart never fires. This is the independent signal that lets the
/// supervisor force a restart anyway.
///
/// What: registers `WEDGED_TIMEOUT_THRESHOLD` pending requests up front
/// against a duplex pipe whose write end is held open but never written to.
/// `reader_task` always evicts the oldest (min-id) pending entry per timeout
/// window, so this produces exactly `WEDGED_TIMEOUT_THRESHOLD` consecutive
/// real timeouts. Asserts `unhealthy_signal` observes `true` and the reader
/// task is still running afterward.
///
/// Test: this test.
#[tokio::test]
async fn wedge_threshold_fires_after_consecutive_timeouts() {
    use tokio::io::duplex;
    use tokio::sync::oneshot;

    let short_timeout = Duration::from_millis(30);
    // Write end held (not dropped) so the reader never sees EOF — it must
    // see only per-call timeouts, exercising the wedge path specifically.
    let (_writer, reader_end) = duplex(64);
    let reader = tokio::io::BufReader::new(reader_end);

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut guard = pending.lock().await;
        for i in 0..WEDGED_TIMEOUT_THRESHOLD {
            let (tx, _rx) = oneshot::channel();
            guard.insert(u64::from(i) + 1, PendingRequest { sent: 1, reply: tx });
        }
    }

    let (unhealthy_tx, mut unhealthy_rx) = watch::channel(false);
    let timeout_tracker = Arc::new(TimeoutTracker::new());
    let handle = tokio::spawn(reader_task(
        reader,
        Arc::clone(&pending),
        short_timeout,
        Arc::clone(&timeout_tracker),
        unhealthy_tx,
    ));

    tokio::time::timeout(Duration::from_secs(5), unhealthy_rx.changed())
        .await
        .expect("unhealthy_signal must fire once the wedge threshold is crossed")
        .expect("unhealthy watch channel must not close without a value");
    assert!(
        *unhealthy_rx.borrow(),
        "unhealthy_signal must be true after WEDGED_TIMEOUT_THRESHOLD consecutive timeouts"
    );
    assert!(
        !handle.is_finished(),
        "reader task must stay alive on wedge detection (fix #763 invariant) — \
         only the supervisor acts on unhealthy_signal"
    );
    handle.abort();
}

/// Regression test for #1450: a successful reply between timeouts resets the
/// consecutive-timeout counter, so a sidecar that is merely occasionally slow
/// (not wedged) never spuriously triggers a restart.
///
/// Why: without a reset, individually-timing-out-but-eventually-recovering
/// requests would accumulate across the sidecar's whole lifetime and
/// eventually cross the threshold even though the sidecar is healthy.
///
/// What: times out `WEDGED_TIMEOUT_THRESHOLD - 1` requests (one below the
/// threshold — condition-polled to completion rather than slept for a fixed
/// duration), delivers one real successful response, then times out
/// `WEDGED_TIMEOUT_THRESHOLD - 1` more. Asserts `unhealthy_signal` never
/// fires because the intervening success reset the counter.
///
/// Test: this test.
#[tokio::test]
async fn wedge_threshold_resets_on_success() {
    use tokio::io::{AsyncWriteExt, duplex};
    use tokio::sync::oneshot;

    // Compile-time guard (not a runtime assert! — clippy flags asserting a
    // constant expression): this test assumes a threshold >= 2 so "one below
    // threshold" is itself a real timeout count.
    const _: () = assert!(WEDGED_TIMEOUT_THRESHOLD >= 2);
    let below = WEDGED_TIMEOUT_THRESHOLD - 1;

    let short_timeout = Duration::from_millis(30);
    let (mut writer, reader_end) = duplex(4096);
    let reader = tokio::io::BufReader::new(reader_end);

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut guard = pending.lock().await;
        for i in 0..below {
            let (tx, _rx) = oneshot::channel();
            guard.insert(u64::from(i) + 1, PendingRequest { sent: 1, reply: tx });
        }
    }

    let (unhealthy_tx, unhealthy_rx) = watch::channel(false);
    let timeout_tracker = Arc::new(TimeoutTracker::new());
    let handle = tokio::spawn(reader_task(
        reader,
        Arc::clone(&pending),
        short_timeout,
        Arc::clone(&timeout_tracker),
        unhealthy_tx,
    ));

    // Condition-poll until the first `below` entries have all timed out and
    // been evicted, rather than sleeping a guessed fixed duration.
    wait_until_pending_empty(&pending, short_timeout * 20).await;
    assert!(
        !*unhealthy_rx.borrow(),
        "must not be unhealthy yet — only {below} of {WEDGED_TIMEOUT_THRESHOLD} \
         consecutive timeouts have occurred"
    );

    // Deliver ONE real successful response — resets the tracker.
    let success_id = 1_000_u64;
    let (tx_ok, rx_ok) = oneshot::channel();
    pending.lock().await.insert(
        success_id,
        PendingRequest {
            sent: 1,
            reply: tx_ok,
        },
    );
    let frame = format!(
        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"embeddings\":[[0.1]]}},\"id\":{success_id}}}\n"
    );
    writer.write_all(frame.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx_ok)
        .await
        .expect("success response must be delivered promptly")
        .expect("oneshot must not close without a value")
        .expect("synthetic success response must decode without error");

    // `below` more real timeouts — still under the threshold because the
    // intervening success reset the counter.
    {
        let mut guard = pending.lock().await;
        for i in 0..below {
            let (tx, _rx) = oneshot::channel();
            guard.insert(2_000 + u64::from(i), PendingRequest { sent: 1, reply: tx });
        }
    }
    wait_until_pending_empty(&pending, short_timeout * 20).await;

    assert!(
        !*unhealthy_rx.borrow(),
        "unhealthy_signal must not fire: the intervening success reset the \
         consecutive-timeout counter, so only {below} timeouts have \
         accumulated since — one below WEDGED_TIMEOUT_THRESHOLD"
    );
    handle.abort();
}

/// Condition-poll until `pending` is empty or `budget` elapses (panics on
/// timeout). Prefer this over a fixed `sleep` when waiting for the reader
/// task to finish draining timed-out entries — see `condition-based-waiting`
/// conventions.
async fn wait_until_pending_empty(pending: &PendingMap, budget: Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if pending.lock().await.is_empty() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("pending map did not drain within {budget:?}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ── #1448 CRITICAL follow-up: post-death registration window ───────────

/// Regression test for the CRITICAL gap: a request registered AFTER the
/// client is already unhealthy (mirrors one issued during the kill/respawn
/// window, when the OS process may still be alive but no reader task exists
/// to ever answer it) must error promptly instead of hanging forever.
///
/// Why: `spawn_reader_with_monitor`'s one-time drain only clears the pending
/// map as it existed at the moment the reader died. A request that registers
/// itself afterward is invisible to that drain — without racing against the
/// unhealthy signal, `reply_rx.await` would hang forever (no reader left,
/// and the caller holds its own `Arc<dyn EmbedderClient>`, so nothing ever
/// drops the sender either).
///
/// What: registers a pending entry directly (as `embed_batch` would), flips
/// `unhealthy` to `true` BEFORE calling `await_reply_or_unhealthy`, then
/// asserts the call resolves to an error within 2s and that the entry was
/// removed from `pending` (not leaked).
///
/// Test: this test.
#[tokio::test]
async fn request_after_reader_death_errors_promptly() {
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (unhealthy_tx, unhealthy_rx) = watch::channel(false);
    let (reply_tx, reply_rx) = oneshot::channel();
    let id = 42_u64;
    pending.lock().await.insert(
        id,
        PendingRequest {
            sent: 1,
            reply: reply_tx,
        },
    );

    // The client is already unhealthy before we start waiting — mirrors a
    // request that registered itself during the kill/respawn window.
    let _ = unhealthy_tx.send(true);

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        await_reply_or_unhealthy(id, &pending, reply_rx, unhealthy_rx),
    )
    .await
    .expect("must error promptly instead of hanging when the client is already unhealthy");

    assert!(
        matches!(result, Err(EmbedderError::Stdio(_))),
        "got: {result:?}"
    );
    assert!(
        !pending.lock().await.contains_key(&id),
        "the stranded pending entry must be removed, not leaked"
    );
}

/// Companion to `request_after_reader_death_errors_promptly`: the unhealthy
/// signal fires WHILE the request is already waiting (not before), proving
/// the `tokio::select!` race in `await_reply_or_unhealthy` — not just the
/// pre-registration state — actually wakes up a stranded waiter.
///
/// Test: this test.
#[tokio::test]
async fn unhealthy_signal_during_wait_errors_promptly() {
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (unhealthy_tx, unhealthy_rx) = watch::channel(false);
    let (reply_tx, reply_rx) = oneshot::channel();
    let id = 7_u64;
    pending.lock().await.insert(
        id,
        PendingRequest {
            sent: 1,
            reply: reply_tx,
        },
    );

    // Flip unhealthy shortly after the wait has already begun.
    let flipper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = unhealthy_tx.send(true);
    });

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        await_reply_or_unhealthy(id, &pending, reply_rx, unhealthy_rx),
    )
    .await
    .expect("must not hang once the unhealthy signal fires mid-wait");

    assert!(
        matches!(result, Err(EmbedderError::Stdio(_))),
        "got: {result:?}"
    );
    flipper.await.expect("flipper task must not panic");
}

/// Guard against the `biased` select breaking the happy path: a real reply
/// that is already available must still win over an unhealthy signal that
/// never fires.
///
/// Test: this test.
#[tokio::test]
async fn await_reply_or_unhealthy_returns_ok_on_real_reply() {
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (_unhealthy_tx, unhealthy_rx) = watch::channel(false);
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = reply_tx.send(Ok(vec![vec![0.1_f32, 0.2_f32]]));

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        await_reply_or_unhealthy(1, &pending, reply_rx, unhealthy_rx),
    )
    .await
    .expect("must resolve promptly")
    .expect("must succeed when a real reply is already available");

    assert_eq!(result, vec![vec![0.1_f32, 0.2_f32]]);
}
