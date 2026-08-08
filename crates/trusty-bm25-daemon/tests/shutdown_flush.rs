//! Shutdown durability: writes still inside the open coalescing window must
//! reach disk before the daemon exits.
//!
//! Why: the batch worker writes the snapshot at the end of a write window, so
//! everything indexed inside an open window lives in memory only. Its own
//! final flush is gated on the op channel closing, which cannot happen while
//! the accept loop holds a sender — so before `lib.rs`'s shutdown flush, a
//! daemon that stopped mid-window silently lost that window. Survivable while
//! the only thing that stopped a daemon was a deliberate shutdown; not
//! survivable now that the trusty-memory supervisor reaps daemons routinely to
//! hold its process cap.
//!
//! What: drives `run_until` in-process with an explicit shutdown trigger
//! rather than a real signal. That is what makes this test CI-runnable and
//! deterministic: the write window is set to 10 minutes, so the indexed
//! document is guaranteed to be un-flushed when shutdown fires, and reverting
//! the flush block fails the test every time rather than only when a reap
//! happens to land inside a 50 ms race.
//!
//! Test: this *is* the test file.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use trusty_bm25_daemon::{run_until, DaemonConfig};

/// Send one JSON-RPC frame and return the decoded response.
///
/// Why: this crate has no client dependency by design, and the test needs
/// exactly one request/response round trip.
/// What: connects, writes the frame plus newline, reads one line back.
/// Test: used by the test below.
async fn rpc(socket: &Path, frame: &str) -> serde_json::Value {
    let stream = UnixStream::connect(socket).await.expect("connect");
    let (read_half, mut write_half) = stream.into_split();
    write_half
        .write_all(format!("{frame}\n").as_bytes())
        .await
        .expect("write frame");
    write_half.shutdown().await.expect("half-close");
    let mut line = String::new();
    BufReader::new(read_half)
        .read_line(&mut line)
        .await
        .expect("read response");
    serde_json::from_str(&line).expect("decode response")
}

/// Poll until the daemon's socket accepts a connection.
async fn await_socket(socket: &Path) {
    for _ in 0..300 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never bound {}", socket.display());
}

/// Why: this is the regression the shutdown flush exists for. Reverting the
/// `timeout(SHUTDOWN_FLUSH_TIMEOUT, shutdown_queue.flush_now())` block in
/// `lib.rs` makes the snapshot assertion below fail deterministically — the
/// file is never created, because a 10-minute write window guarantees the
/// worker has not flushed on its own.
/// What: starts the daemon in-process, indexes one document, asserts nothing
/// is on disk yet, fires shutdown, and asserts the document survived.
/// Test: this test itself.
#[tokio::test]
async fn shutdown_flushes_the_open_write_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("flush.sock");
    let data_dir = dir.path().join("data");
    let snapshot = data_dir.join("bm25_index.json");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(run_until(
        DaemonConfig {
            palace: "flush-window".to_string(),
            data_dir: data_dir.clone(),
            socket: Some(socket.clone()),
            // Ten minutes: the worker cannot flush on its own before the test
            // finishes, so a passing assertion can only come from the
            // shutdown flush.
            write_window_ms: 600_000,
            max_batch_size: 10_000,
        },
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    await_socket(&socket).await;

    let resp = rpc(
        &socket,
        r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"d1","text":"phoenix rising"},"id":1}"#,
    )
    .await;
    assert!(resp.get("error").is_none_or(|e| e.is_null()), "{resp}");

    assert!(
        !snapshot.exists(),
        "precondition: the write must still be inside the open window, \
         un-flushed — otherwise this test proves nothing about shutdown"
    );

    shutdown_tx.send(()).expect("fire shutdown");
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("daemon must exit promptly after shutdown")
        .expect("daemon task must not panic")
        .expect("daemon must exit cleanly");

    let raw = std::fs::read_to_string(&snapshot).unwrap_or_else(|e| {
        panic!(
            "shutdown must flush the open write window to {}: {e}",
            snapshot.display()
        )
    });
    assert!(
        raw.contains("phoenix rising"),
        "the snapshot must hold the document indexed inside the window: {raw}"
    );
}

/// Why: the flush must not be narrowed to the reap path. `run_until` races
/// shutdown against the accept loop, and a caller that closes the listener
/// takes the other arm — that arm owes the same durability.
/// What: same setup, but the shutdown future never resolves; the daemon exits
/// because its socket is removed and the test drops it. Rather than depend on
/// that, this asserts the simpler invariant: a second shutdown-driven run
/// against the SAME data dir loads what the first flushed.
/// Test: this test itself.
#[tokio::test]
async fn a_flushed_snapshot_is_loaded_by_the_next_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");

    for (socket_name, doc, text) in [
        ("run-a.sock", "d1", "first window"),
        ("run-b.sock", "d2", "second window"),
    ] {
        let socket = dir.path().join(socket_name);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let daemon = tokio::spawn(run_until(
            DaemonConfig {
                palace: "reload".to_string(),
                data_dir: data_dir.clone(),
                socket: Some(socket.clone()),
                write_window_ms: 600_000,
                max_batch_size: 10_000,
            },
            async move {
                let _ = rx.await;
            },
        ));
        await_socket(&socket).await;
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"index","params":{{"doc_id":"{doc}","text":"{text}"}},"id":1}}"#
        );
        rpc(&socket, &frame).await;
        tx.send(()).expect("fire shutdown");
        tokio::time::timeout(Duration::from_secs(10), daemon)
            .await
            .expect("daemon exits")
            .expect("no panic")
            .expect("clean exit");
    }

    let raw = std::fs::read_to_string(data_dir.join("bm25_index.json")).expect("snapshot");
    assert!(raw.contains("first window"), "got: {raw}");
    assert!(raw.contains("second window"), "got: {raw}");
}
