//! Tests for the live activity feed (#6286).
//!
//! Why: the feed replaces a poll, so the three things that can be wrong are all
//! about what happens WITHOUT one — an event that never arrives, a dead stream
//! the caller cannot detect, and a dial failure that is not reported as one.
//! Each needs a real socket, because the framing and the terminal-frame
//! contract are exactly what a hand-rolled fake would get wrong.
//! What: binds a stream-serving socket through the same
//! [`crate::uds::server`] pieces the daemon uses, and drives `ActivityFeed`
//! against it.
//! Test: this file IS the test coverage.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};

/// A socket serving `memory.activity_stream` from a caller-supplied item list.
///
/// Dropping the returned guard stops the accept loop.
struct StreamDaemon {
    socket: std::path::PathBuf,
    _dir: TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for StreamDaemon {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

/// Serve `items` on `memory.activity_stream`, one frame each, then end.
async fn stream_daemon(items: Vec<Result<Value, RpcError>>) -> StreamDaemon {
    stream_daemon_with(items, false).await
}

/// [`stream_daemon`], optionally holding the stream open after the last item.
///
/// Why: a test asserting on a LIVE feed's state has to observe it before the
/// stream ends — the close reason overwrites the last lag notice, which is the
/// right precedence in production and the wrong one to assert against.
async fn stream_daemon_with(items: Vec<Result<Value, RpcError>>, hold_open: bool) -> StreamDaemon {
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("memory.sock");
    let listener = crate::uds::bind_hardened(&socket).expect("bind");

    let items = Arc::new(items);
    let router = RpcRouter::new().typed_stream::<Value, _, _>(
        "memory.activity_stream",
        move |_params: Value| {
            let items = Arc::clone(&items);
            let hold_open = hold_open;
            async move {
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    for item in items.iter() {
                        let frame = match item {
                            Ok(v) => Ok(v.clone()),
                            Err(e) => Err(RpcError::new(e.code, e.message.clone())),
                        };
                        if tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    if hold_open {
                        // Keep the sender alive so the stream stays open. The
                        // task ends when the test's daemon guard drops.
                        std::future::pending::<()>().await;
                    }
                });
                Ok(rx)
            }
        },
    );

    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });

    StreamDaemon {
        socket,
        _dir: dir,
        stop: Some(stop),
    }
}

/// Poll `drain` until it yields something or the budget runs out.
///
/// The background task fills the channel concurrently, so a single `drain`
/// right after `open` races it. This is the condition-based wait that replaces
/// a sleep.
async fn drain_until_nonempty(feed: &mut ActivityFeed) -> Vec<MemoryEvent> {
    for _ in 0..200 {
        let batch = feed.drain();
        if !batch.is_empty() {
            return batch;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Vec::new()
}

/// Why: the whole point of the stream is that an event arrives without the
/// caller asking. This is what a poll-based test could not prove.
/// Test: itself.
#[tokio::test]
async fn feed_drains_what_the_stream_pushed() {
    let daemon = stream_daemon(vec![
        Ok(json!({ "type": "palace_created", "id": "p", "name": "alpha" })),
        Ok(json!({ "type": "palace_created", "id": "q", "name": "beta" })),
    ])
    .await;

    let mut feed = ActivityFeed::open(&daemon.socket)
        .await
        .expect("the stream opens");
    let mut seen = drain_until_nonempty(&mut feed).await;
    for _ in 0..50 {
        if seen.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        seen.extend(feed.drain());
    }

    assert_eq!(
        seen,
        vec![
            MemoryEvent::PalaceCreated {
                name: "alpha".to_string()
            },
            MemoryEvent::PalaceCreated {
                name: "beta".to_string()
            },
        ],
        "both pushed events arrive, in order"
    );
}

/// Why: a stream that ends must be DETECTABLE, because the caller's fallback to
/// polling is what stops the log going blank against a live daemon. A feed that
/// stayed `is_live` after its stream died would leave the log frozen with no
/// error and no poll.
/// Test: itself.
#[tokio::test]
async fn feed_reports_a_terminal_error_and_stops_being_live() {
    let daemon = stream_daemon(vec![Err(RpcError::internal("the daemon fell over"))]).await;

    let feed = ActivityFeed::open(&daemon.socket)
        .await
        .expect("the stream opens before the handler fails");

    for _ in 0..200 {
        if !feed.is_live() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!feed.is_live(), "a terminal error ends the feed");
    let reason = feed.take_last_error().expect("the reason is recorded");
    assert!(
        reason.contains("fell over"),
        "the daemon's own message must survive: {reason}"
    );
}

/// Why: a daemon that is not running, and one predating the method, both look
/// like a failed open — and both mean the caller polls instead. Reporting the
/// failure is what lets it make that choice rather than showing an empty log.
/// Test: itself.
#[tokio::test]
async fn feed_open_fails_against_an_absent_socket() {
    let dir = TempDir::new().expect("tempdir");
    let result = ActivityFeed::open(&dir.path().join("absent.sock")).await;
    assert!(
        result.is_err(),
        "nothing is serving that path, and the caller has to learn so"
    );
}

/// Why: a `lagged` frame is the daemon saying this reader missed events. It is
/// not an activity event and must not enter the log as one — but it must be
/// readable, because a feed with a hole in it looks continuous otherwise.
/// Test: itself.
#[tokio::test]
async fn feed_records_a_lag_without_logging_it_as_an_event() {
    // Held open: the stream's close reason overwrites the lag notice, which is
    // correct precedence in production and would make this assertion about the
    // wrong thing.
    let daemon = stream_daemon_with(
        vec![
            Ok(json!({ "type": "lagged", "lagged": 7 })),
            Ok(json!({ "type": "palace_created", "id": "p", "name": "after-the-gap" })),
        ],
        true,
    )
    .await;

    let mut feed = ActivityFeed::open(&daemon.socket)
        .await
        .expect("the stream opens");
    let seen = drain_until_nonempty(&mut feed).await;
    assert_eq!(
        seen,
        vec![MemoryEvent::PalaceCreated {
            name: "after-the-gap".to_string()
        }],
        "the lag notice is not an activity row"
    );
    let reason = feed.take_last_error().expect("the lag is recorded");
    assert!(
        reason.contains('7'),
        "the reader has to be able to say how many it missed: {reason}"
    );
    assert!(
        feed.is_live(),
        "a lag is a hole in a working feed, not the end of one"
    );
}

/// Why (#6286 review, finding 4): the caller renders each notice once, on an
/// ordinary tick. A slot that kept its value would re-render the same lag every
/// tick until the stream died; one that cleared without being read would lose
/// it. Taking is what makes "push once and clear" the caller's whole job.
/// Test: itself.
#[tokio::test]
async fn feed_take_last_error_clears_the_slot() {
    let daemon = stream_daemon_with(vec![Ok(json!({ "type": "lagged", "lagged": 3 }))], true).await;

    let feed = ActivityFeed::open(&daemon.socket)
        .await
        .expect("the stream opens");

    let mut first = None;
    for _ in 0..200 {
        first = feed.take_last_error();
        if first.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        first.is_some_and(|r| r.contains('3')),
        "the first take reports the lag"
    );
    assert!(
        feed.take_last_error().is_none(),
        "a second take on an otherwise quiet feed reports nothing — the notice \
         is rendered once, not on every tick until the stream dies"
    );
}
