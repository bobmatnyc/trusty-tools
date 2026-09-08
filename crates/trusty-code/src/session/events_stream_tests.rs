//! Tests for [`crate::session::events_stream`] (#6637).
//!
//! Why: `session.events` is the replacement for the HTTP SSE route the TUI
//! used to tail, so its two load-bearing properties — replay-then-live with no
//! gap, and a resume that skips what the client already has — are asserted
//! directly against the registry rather than through a transport.
//! What: mirrors `registry_tests::attach_replay_then_live_seq_is_contiguous`
//! for the replay/live seam, adds the `after_seq` resume HTTP has no
//! equivalent of, and pins the unknown-session refusal.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::time::timeout;

use super::{SessionEventsParams, open};
use crate::session::SessionRegistry;

/// Read the next frame's `seq`, failing the test rather than hanging.
async fn next_seq(rx: &mut trusty_common::uds::server::RpcStreamItems) -> u64 {
    let frame = timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a frame must arrive within the budget")
        .expect("the stream must still be open")
        .expect("the frame must not be a terminal error");
    frame
        .get("seq")
        .and_then(Value::as_u64)
        .expect("every session event envelope carries a seq")
}

/// The ring buffer must arrive first, then the live bus, with no gap and no
/// duplicate across the seam — the property
/// `registry_tests::attach_replay_then_live_seq_is_contiguous` pins for
/// `session.attach`, restated for the socket transport.
#[tokio::test]
async fn session_events_replays_ring_buffer_then_live() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry.send(&session.id, "before-open").unwrap();

    let mut rx = open(
        registry.clone(),
        SessionEventsParams {
            session_id: session.id.clone(),
            after_seq: None,
        },
    )
    .await
    .expect("open must succeed for a live session");

    let replayed = next_seq(&mut rx).await;
    assert_eq!(replayed, 1, "the ring buffer replays from seq 1");

    registry.send(&session.id, "after-open").unwrap();
    let live = next_seq(&mut rx).await;
    assert_eq!(
        live,
        replayed + 1,
        "no gap and no duplicate between replay and live"
    );
}

/// `after_seq` must drop exactly the replayed events at or below it, so a
/// reconnecting client resumes rather than re-reading its whole tail.
#[tokio::test]
async fn session_events_after_seq_skips_replayed_events() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    registry.send(&session.id, "first").unwrap();
    registry.send(&session.id, "second").unwrap();

    let mut rx = open(
        registry.clone(),
        SessionEventsParams {
            session_id: session.id.clone(),
            after_seq: Some(1),
        },
    )
    .await
    .expect("open must succeed for a live session");

    let frame = timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a frame must arrive within the budget")
        .expect("the stream must still be open")
        .expect("the frame must not be a terminal error");
    assert_eq!(
        frame.get("seq").and_then(Value::as_u64),
        Some(2),
        "seq 1 was already confirmed and must not be replayed"
    );
    assert!(
        !frame.to_string().contains("first"),
        "nothing at or below the confirmed seq may be replayed, got: {frame}"
    );
}

/// An unknown session id must be refused when the stream opens, carrying the
/// same `session_not_found` code every other transport reports.
#[tokio::test]
async fn session_events_unknown_session_is_refused() {
    let registry = Arc::new(SessionRegistry::new());
    let err = open(
        registry,
        SessionEventsParams {
            session_id: "no-such-session".to_string(),
            after_seq: None,
        },
    )
    .await
    .expect_err("an unknown session must not open a stream");
    assert_eq!(
        err.code,
        i64::from(crate::jsonrpc::RpcError::session_not_found("x").code)
    );
}
