//! #11256: a read-only handler is cancelled when its client hangs up; a write
//! handler is not. #12019: neither is a cancel-on-hangup one.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Notify;

/// What the handler under test did: started, finished, or was dropped.
#[derive(Default)]
struct Trace {
    started: Notify,
    completed: AtomicBool,
    dropped: AtomicBool,
}

/// Sets `dropped` when the handler future is dropped, finished or not.
struct DropFlag(Arc<Trace>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

/// Which hang-up marker, if any, the method under test carries.
#[derive(Clone, Copy)]
enum Mark {
    Unmarked,
    ReadOnly,
    CancelOnHangup,
}

/// A router with one method `work` that runs for `runs_for`, carrying `mark`.
fn router(trace: &Arc<Trace>, runs_for: Duration, mark: Mark) -> Arc<RpcRouter> {
    let trace = Arc::clone(trace);
    let router = RpcRouter::new().typed("work", move |_req: serde_json::Value| {
        let trace = Arc::clone(&trace);
        async move {
            let _flag = DropFlag(Arc::clone(&trace));
            trace.started.notify_one();
            tokio::time::sleep(runs_for).await;
            trace.completed.store(true, Ordering::SeqCst);
            Ok(json!("done"))
        }
    });
    Arc::new(match mark {
        Mark::Unmarked => router,
        Mark::ReadOnly => router.mark_read_only("work"),
        Mark::CancelOnHangup => router.mark_cancel_on_hangup("work"),
    })
}

/// Send one `work` frame and half-close, as `send_framed_request` does.
async fn send(client: &mut UnixStream) {
    let mut bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "work", "params": {}
    }))
    .expect("serialize frame");
    bytes.push(b'\n');
    client.write_all(&bytes).await.expect("write frame");
    client.shutdown().await.expect("half-close");
}

/// Serve one request, hang the client up once the handler has started, and
/// return what `handle_connection` reported.
async fn serve_then_hang_up(
    router: Arc<RpcRouter>,
    trace: &Arc<Trace>,
) -> Result<Served, RpcServerError> {
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let task = tokio::spawn(handle_connection(
        server,
        router,
        RpcServeOptions::default(),
    ));
    send(&mut client).await;
    tokio::time::timeout(Duration::from_secs(5), trace.started.notified())
        .await
        .expect("handler started");
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("handle_connection returned after the hang-up")
        .expect("connection task did not panic")
}

#[tokio::test]
async fn a_read_only_handler_is_cancelled_when_its_client_hangs_up_11256() {
    let trace = Arc::new(Trace::default());
    let served = serve_then_hang_up(
        router(&trace, Duration::from_secs(60), Mark::ReadOnly),
        &trace,
    )
    .await;

    // The cancel arm is an error, never `Answered`.
    assert!(
        matches!(&served, Err(RpcServerError::ClientGone { method }) if method == "work"),
        "expected ClientGone, got {served:?}"
    );
    // The handler future was dropped (no leaked task) before it finished, and
    // the stream it owned went with `handle_connection`'s return.
    assert!(
        trace.dropped.load(Ordering::SeqCst),
        "handler future leaked"
    );
    assert!(!trace.completed.load(Ordering::SeqCst), "handler ran on");
}

#[tokio::test]
async fn a_write_handler_runs_to_completion_after_its_client_hangs_up_11256() {
    let trace = Arc::new(Trace::default());
    let served = serve_then_hang_up(
        router(&trace, Duration::from_millis(300), Mark::Unmarked),
        &trace,
    )
    .await;

    assert!(
        trace.completed.load(Ordering::SeqCst),
        "write was cancelled"
    );
    assert!(
        !matches!(served, Err(RpcServerError::ClientGone { .. })),
        "a write must not report a cancel, got {served:?}"
    );
}

#[tokio::test]
async fn a_half_closed_client_still_gets_its_read_only_answer_11256() {
    let trace = Arc::new(Trace::default());
    let router = router(&trace, Duration::from_millis(300), Mark::ReadOnly);
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let task = tokio::spawn(handle_connection(
        server,
        router,
        RpcServeOptions::default(),
    ));
    send(&mut client).await;

    let mut answer = String::new();
    client
        .read_to_string(&mut answer)
        .await
        .expect("read answer");
    let served = task.await.expect("connection task did not panic");

    assert!(
        matches!(served, Ok(Served::Answered { errored: false, .. })),
        "got {served:?}"
    );
    assert!(answer.contains("\"done\""), "answer: {answer}");
    assert!(trace.completed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_read_only_handler_error_is_answered_as_an_error_frame_11256() {
    let router = Arc::new(
        RpcRouter::new()
            .typed("fail", |_req: serde_json::Value| async move {
                tokio::time::sleep(Duration::from_millis(120)).await;
                Err::<(), _>(RpcError::new(-32001, "read failed"))
            })
            .mark_read_only("fail"),
    );
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let task = tokio::spawn(handle_connection(
        server,
        router,
        RpcServeOptions::default(),
    ));
    let mut bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": 7, "method": "fail", "params": {}
    }))
    .expect("serialize frame");
    bytes.push(b'\n');
    client.write_all(&bytes).await.expect("write frame");
    client.shutdown().await.expect("half-close");

    let mut answer = String::new();
    client
        .read_to_string(&mut answer)
        .await
        .expect("read answer");
    let served = task.await.expect("connection task did not panic");

    assert!(
        matches!(served, Ok(Served::Answered { errored: true, .. })),
        "got {served:?}"
    );
    assert!(answer.contains("read failed"), "answer: {answer}");
}

#[test]
fn read_only_method_names_only_marked_methods_11256() {
    let router = RpcRouter::new().mark_read_only("recall");
    let frame = |m: &str| format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{m}"}}"#);
    assert_eq!(
        router.cancel_on_hangup_method(frame("recall").as_bytes()),
        Some("recall".to_string())
    );
    assert_eq!(
        router.cancel_on_hangup_method(frame("remember").as_bytes()),
        None
    );
    assert_eq!(router.cancel_on_hangup_method(b"not json"), None);
}

/// #12019: a method marked cancel-on-hangup is dropped when its client hangs
/// up; the same method left unmarked runs to completion.
/// Test: this function IS the test. Fails if `handle_connection` races only
/// `mark_read_only` names against a hang-up.
#[tokio::test]
async fn a_cancel_on_hangup_handler_is_cancelled_and_an_unmarked_one_is_not_12019() {
    let marked = Arc::new(Trace::default());
    let served = serve_then_hang_up(
        router(&marked, Duration::from_secs(60), Mark::CancelOnHangup),
        &marked,
    )
    .await;
    assert!(
        matches!(&served, Err(RpcServerError::ClientGone { method }) if method == "work"),
        "expected ClientGone, got {served:?}"
    );
    assert!(
        marked.dropped.load(Ordering::SeqCst),
        "handler future leaked"
    );
    assert!(!marked.completed.load(Ordering::SeqCst), "handler ran on");

    let unmarked = Arc::new(Trace::default());
    let served = serve_then_hang_up(
        router(&unmarked, Duration::from_millis(300), Mark::Unmarked),
        &unmarked,
    )
    .await;
    assert!(
        unmarked.completed.load(Ordering::SeqCst),
        "an unmarked method was cancelled"
    );
    assert!(
        !matches!(served, Err(RpcServerError::ClientGone { .. })),
        "an unmarked method must not report a cancel, got {served:?}"
    );
}

#[test]
fn cancel_on_hangup_method_names_either_marker_12019() {
    let router = RpcRouter::new()
        .mark_read_only("recall")
        .mark_cancel_on_hangup("reactivate");
    let frame = |m: &str| format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{m}"}}"#);
    assert_eq!(
        router.cancel_on_hangup_method(frame("recall").as_bytes()),
        Some("recall".to_string())
    );
    assert_eq!(
        router.cancel_on_hangup_method(frame("reactivate").as_bytes()),
        Some("reactivate".to_string())
    );
    assert_eq!(
        router.cancel_on_hangup_method(frame("remember").as_bytes()),
        None
    );
}
