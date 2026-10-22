//! Tests for [`super::EngineState::pump_session_events`]' reconnect (#6637).
//!
//! Why: `trusty_common::uds`'s stream contract says a stream ends on a
//! terminal FRAME and never on EOF, so a socket that closes mid-tail reports
//! `UdsRpcError::NoResponse` rather than a clean end. A client that treated
//! that as a finished turn would render a truncated answer as a complete one —
//! the exact Fail-Open branch the contract exists to close. This asserts the
//! pump reconnects instead, and re-requests from the `seq` it last forwarded.
//! What: a hand-rolled stub daemon on a real socket that answers the first
//! `session.events` with one frame and then hangs up mid-stream, and the second
//! with the terminal `SessionDone`. The second request's `after_seq` is
//! captured and asserted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::timeout;
use trusty_code_tui::ReplEvent;
use trusty_common::uds::test_support::bind_hardened_stub;

use super::EngineState;
use crate::tui_client::uds_rpc::UdsRpcClient;

/// Every `after_seq` the stub observed, in request order.
type SeenCursors = Arc<Mutex<Vec<Option<u64>>>>;

/// One `session.events` frame, as the daemon writes it.
fn item_frame(id: &Value, seq: u64, event: Value) -> String {
    let envelope = json!({
        "session_id": "sess-1",
        "seq": seq,
        "at": "2026-09-08T00:00:00Z",
        "kind": event["type"],
        "event": event,
    });
    format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": id, "stream": "item", "result": envelope})
    )
}

/// A stub daemon that truncates its first stream and completes its second.
///
/// Why hand-rolled rather than the real daemon: the property under test is a
/// mid-stream hang-up, which the real server never does on purpose. Writing the
/// frames directly is the only way to produce one deterministically.
async fn spawn_truncating_daemon(dir: &Path, seen: SeenCursors) -> PathBuf {
    // `connect_hardened` verifies the bits before it writes a byte, so a stub
    // that skipped them would be refused by the client rather than exercised.
    // #7215: the shared stub bind sets them.
    let (listener, socket) = bind_hardened_stub(dir).expect("bind the stub socket");
    tokio::spawn(async move {
        let mut answered = 0u32;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            let attempt = answered;
            answered += 1;
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let request: Value = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => return,
                };
                let id = request["id"].clone();
                if request["method"] != "session.events" {
                    // `task.run` and anything else: one ordinary response.
                    let body = format!("{}\n", json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                    let _ = reader.get_mut().write_all(body.as_bytes()).await;
                    let _ = reader.get_mut().flush().await;
                    return;
                }

                seen.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(request["params"]["after_seq"].as_u64());

                if attempt == 0 {
                    // One frame, then hang up with no terminal frame.
                    let frame = item_frame(
                        &id,
                        7,
                        json!({
                            "type": "session_input",
                            "session_id": "sess-1",
                            "input": "partial",
                        }),
                    );
                    let _ = reader.get_mut().write_all(frame.as_bytes()).await;
                    let _ = reader.get_mut().flush().await;
                    return;
                }

                let frame = item_frame(
                    &id,
                    8,
                    json!({"type": "session_done", "session_id": "sess-1", "status": "finished"}),
                );
                let _ = reader.get_mut().write_all(frame.as_bytes()).await;
                let _ = reader.get_mut().flush().await;
                // The client returns on the terminal session event, so the
                // stream's own `end` frame is never read; writing it anyway
                // keeps the stub honest about the wire contract.
                let end = format!("{}\n", json!({"jsonrpc": "2.0", "id": id, "stream": "end"}));
                let _ = reader.get_mut().write_all(end.as_bytes()).await;
                let _ = reader.get_mut().flush().await;
            });
        }
    });
    socket
}

/// A truncated tail must reconnect and resume above the last forwarded `seq`,
/// never be reported as a finished turn.
#[tokio::test]
async fn stream_client_reconnects_after_a_truncated_tail() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    let seen: SeenCursors = Arc::new(Mutex::new(Vec::new()));
    let socket = spawn_truncating_daemon(dir.path(), seen.clone()).await;

    let state = EngineState::new(UdsRpcClient::new(socket), None, false);
    let (tx, mut rx) = unbounded_channel();
    timeout(
        Duration::from_secs(30),
        state.pump_session_events("sess-1", &tx),
    )
    .await
    .expect("the pump must finish within the budget")
    .expect("the reconnect must carry the turn to its terminal event");

    let cursors = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        cursors,
        vec![None, Some(7)],
        "the first request opens bare and the reconnect resumes above the forwarded seq"
    );

    // The truncation is visible to the user, not swallowed.
    let mut saw_connection_lost = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, ReplEvent::ConnectionLost { .. }) {
            saw_connection_lost = true;
        }
    }
    assert!(
        saw_connection_lost,
        "a truncated tail must render as ConnectionLost, not silently"
    );
}

/// A daemon that accepts the connection and then never writes a frame must not
/// hang the pump forever — #3494's property, restated for this transport.
///
/// The per-frame budget is shortened for the test rather than waited out:
/// `STREAM_FRAME_TIMEOUT` is fifteen minutes, and a paused clock cannot help
/// because the reader is parked on a socket read rather than on a timer. What
/// is asserted is the SHAPE — the budget expires, the reconnects exhaust, the
/// pump returns an error and the TUI sees it — which does not depend on the
/// budget's size.
#[tokio::test]
async fn session_stream_silence_is_bounded_not_infinite() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    let (listener, socket) = bind_hardened_stub(dir.path()).expect("bind the silent socket");

    let accept_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            // Held open, never written to: the wedged-daemon shape.
            std::mem::forget(stream);
        }
    });

    let state = EngineState::new(
        UdsRpcClient::new(socket).with_stream_frame_timeout(Duration::from_millis(200)),
        None,
        false,
    );
    let (tx, mut rx) = unbounded_channel();
    let result = timeout(
        Duration::from_secs(120),
        state.pump_session_events("sess-1", &tx),
    )
    .await
    .expect("the pump must return rather than hang");
    assert!(
        result.is_err(),
        "exhausting the reconnect budget against a silent daemon must be an error"
    );

    let mut saw_connection_lost = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, ReplEvent::ConnectionLost { .. }) {
            saw_connection_lost = true;
        }
    }
    assert!(
        saw_connection_lost,
        "a wedged daemon must surface as ConnectionLost rather than a silent hang"
    );

    accept_task.abort();
}

/// #7216: opening a stream is bounded by the unary call budget, not by the
/// per-frame budget. The frame budget is an hour here, so only a separate open
/// budget can end a write the daemon never drains. The paused clock waits out
/// the budget; the socket write itself stays real.
#[tokio::test(start_paused = true)]
async fn stream_open_is_bounded_by_the_call_timeout_not_the_frame_budget() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    // Bound and never accepted: a request larger than the socket buffers
    // blocks its write, the wedged-daemon shape for the open step.
    let (_listener, socket) = bind_hardened_stub(dir.path()).expect("bind the unread stub");
    let client = UdsRpcClient::new(socket).with_stream_frame_timeout(Duration::from_secs(3600));
    let pad = "x".repeat(32 * 1024 * 1024);

    let opened = timeout(
        Duration::from_secs(60),
        client.open_stream::<Value>("session.events", json!({ "pad": pad })),
    )
    .await
    .expect("the open must be bounded by the call budget, not the hour-long frame budget");

    match opened {
        Err(crate::tui_client::error::EngineError::Transport { source, .. }) => assert!(
            matches!(
                *source,
                trusty_common::uds::UdsRpcError::Timeout { timeout, .. }
                    if timeout == crate::tui_client::uds_rpc::DEFAULT_CALL_TIMEOUT
            ),
            "expected a Timeout at DEFAULT_CALL_TIMEOUT, got {source:?}"
        ),
        other => panic!("expected a transport Timeout, got {other:?}"),
    }
}
