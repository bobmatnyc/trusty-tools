//! Socket streaming surfaces (#6285 slice 5).
//!
//! Why: a stream can fail in two ways a one-shot body cannot, and both are
//! silent. It can deliver the wrong sequence for an operation — a missing
//! replay event, a reordered progress event — and it can deliver the right
//! sequence while leaking the task and the broadcast subscription that produced
//! it, once per abandoned dashboard. So every case here drives the REAL RPC
//! router against a shared state, and the disconnect case drives a REAL Unix
//! socket rather than the in-process dispatcher, because only a real client can
//! hang up. #9535 removed the SSE halves of the former parity cases; the daemon
//! serves no HTTP, so each case now pins the socket's own expected sequence.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;
use std::time::Duration;

use trusty_common::uds::server::{RpcOutcome, RpcRouter, RpcStreamItems};
use trusty_common::uds::{FramedStream, UdsRpcError};

use crate::core::file_events::FileEventKind;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::concurrency::ConcurrencyLimiter;
use crate::service::query_timeout::QueryTimeoutConfig;
use crate::service::reindex::{ReindexProgress, ReindexStatus};
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::rpc::streams;
use crate::service::server::{DaemonEvent, SearchAppState};

/// How long a real socket dial or a bounded poll is given before the test calls
/// it stuck. Headroom on a loaded machine, not a latency budget.
const GENEROUS: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- fixtures ---

/// One state carrying the given registered indexes, plus the RPC router built
/// on it — the planted progress records and emitted events reach the stream
/// through that one `Arc`.
async fn routers(index_ids: &[&str]) -> (Arc<SearchAppState>, RpcRouter) {
    let registry = IndexRegistry::new();
    for id in index_ids {
        let root = format!("/nonexistent/streams-{id}");
        registry.register(IndexHandle::bare(
            IndexId::new((*id).to_string()),
            Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(*id, &root))),
            root.into(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));
    let rpc = streams::register(RpcRouter::new(), &state);
    (state, rpc)
}

/// Plant a reindex-progress record for `id` holding `events`, at `status`.
///
/// `push` is the daemon's own emission point — it appends to the replay buffer
/// AND broadcasts — so a record built through it is the record a real reindex
/// would have left behind.
async fn plant_progress(
    state: &Arc<SearchAppState>,
    id: &str,
    status: ReindexStatus,
    events: &[serde_json::Value],
) -> Arc<ReindexProgress> {
    let progress = Arc::new(ReindexProgress::new());
    for event in events {
        progress.push(event.clone()).await;
    }
    progress.status.store(status);
    state
        .reindex_progress
        .insert(IndexId::new(id.to_string()), Arc::clone(&progress));
    progress
}

/// The three-event sequence a short reindex leaves in its replay buffer.
fn progress_sequence() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "event": "start", "total_files": 2 }),
        serde_json::json!({
            "event": "progress", "indexed": 1, "total": 2, "current_file": "src/a.rs",
        }),
        serde_json::json!({ "event": "complete", "indexed": 2, "elapsed_ms": 12 }),
    ]
}

// ------------------------------------------------------------- transports ---

/// Open one socket stream through the router's own streaming dispatcher.
///
/// The `"stream": true` flag is what selects the streaming table — without it
/// the same name answers one frame carrying `CODE_STREAM_REQUIRED`.
async fn open_stream(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcStreamItems {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params, "stream": true,
    }))
    .expect("encode the frame");
    match rpc.dispatch_streaming(&frame).await {
        RpcOutcome::Stream { items, .. } => items,
        other => panic!("{method} must answer a stream, got {other:?}"),
    }
}

/// Read `count` items off a stream, failing rather than hanging.
async fn take_items(items: &mut RpcStreamItems, count: usize) -> Vec<serde_json::Value> {
    let mut taken = Vec::with_capacity(count);
    for _ in 0..count {
        let item = tokio::time::timeout(GENEROUS, items.recv())
            .await
            .expect("the producer must deliver rather than hang")
            .expect("the stream must not end before it has delivered its items");
        taken.push(item.expect("this stream must not carry a terminal error"));
    }
    taken
}

/// Drain a stream that ends on its own.
async fn drain(mut items: RpcStreamItems) -> Vec<serde_json::Value> {
    let mut all = Vec::new();
    while let Some(item) = tokio::time::timeout(GENEROUS, items.recv())
        .await
        .expect("the producer must terminate rather than hang")
    {
        all.push(item.expect("this stream must not carry a terminal error"));
    }
    all
}

/// `rpc` served on a real Unix socket until [`Served::stop`].
struct Served {
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    stop_tx: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Served {
    /// Stop the server and wait, bounded, for it to drain.
    async fn stop(self) {
        let _ = self.stop_tx.send(());
        let _ = tokio::time::timeout(GENEROUS, self.task).await;
    }
}

/// Bind a fresh socket, serve `rpc` on it, and return once it answers.
async fn serve_on_socket(rpc: Arc<RpcRouter>) -> Served {
    use trusty_common::uds::server::{serve_until, RpcServeOptions};

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("streams.sock");
    let listener = trusty_common::uds::bind_singleton_hardened(&socket)
        .await
        .expect("bind a fresh socket path");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve_until(&listener, rpc, RpcServeOptions::default(), async {
            let _ = stop_rx.await;
        })
        .await;
    });
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Served {
        _dir: dir,
        socket,
        stop_tx,
        task,
    }
}

/// One frame off a real-socket stream. The client's per-frame budget is
/// `GENEROUS`, so a stuck stream reads as `Some(Err(Timeout))` before this bound.
async fn next_frame(
    stream: &mut FramedStream<serde_json::Value>,
) -> Option<Result<serde_json::Value, UdsRpcError>> {
    tokio::time::timeout(GENEROUS * 2, stream.next_frame())
        .await
        .expect("the client's per-frame read budget answers first")
}

// ------------------------------------------------------- reindex streams ---

/// Why: the replay buffer is the whole reason a late subscriber sees the `start`
/// event at all. This drains the stream against ONE planted record and compares
/// the sequence whole, so a dropped, reordered, or re-encoded event fails here
/// rather than in a dashboard.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn reindex_progress_events_replay_the_planted_sequence_frame_for_frame() {
    let (state, rpc) = routers(&["rs"]).await;
    let expected = progress_sequence();
    plant_progress(&state, "rs", ReindexStatus::Complete, &expected).await;

    let over_socket = drain(
        open_stream(
            &rpc,
            streams::METHOD_INDEX_REINDEX_STREAM,
            serde_json::json!({ "index_id": "rs" }),
        )
        .await,
    )
    .await;

    assert_eq!(
        over_socket, expected,
        "the socket stream must replay the planted sequence"
    );
}

/// Why: a finished reindex never sends again, so a producer that subscribed and
/// then waited would hold the connection open forever after the replay. This
/// proves the socket ends the stream — the terminal frame arrives, rather than
/// the client timing out and reporting a truncated stream.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_finished_reindex_stream_ends_after_its_replay() {
    let (state, rpc) = routers(&["rs"]).await;
    plant_progress(&state, "rs", ReindexStatus::Complete, &progress_sequence()).await;

    let items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;
    assert_eq!(
        drain(items).await.len(),
        3,
        "the stream must end on its own once the replay is delivered"
    );
}

/// Why: the replay arm proves the buffered half. This proves the LIVE half — an
/// event emitted after the subscription reaches the socket, in order, without
/// waiting for the reindex to finish. A producer that collected before sending
/// would pass every test above and fail this one.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_live_reindex_stream_delivers_events_in_order_as_they_are_emitted() {
    let (state, rpc) = routers(&["rs"]).await;
    let start = serde_json::json!({ "event": "start", "total_files": 3 });
    let progress = plant_progress(
        &state,
        "rs",
        ReindexStatus::Running,
        std::slice::from_ref(&start),
    )
    .await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;
    assert_eq!(
        take_items(&mut items, 1).await,
        vec![start],
        "the buffered start event must arrive before any live one"
    );

    let live: Vec<serde_json::Value> = (1..=3)
        .map(|n| serde_json::json!({ "event": "progress", "indexed": n, "total": 3 }))
        .collect();
    for event in &live {
        progress.push(event.clone()).await;
    }
    assert_eq!(
        take_items(&mut items, live.len()).await,
        live,
        "live events must arrive in emission order"
    );
}

/// Why: `ReindexProgress` keeps its broadcast sender for replay, so a live
/// stream never saw `Closed` and stayed open after `complete`. The CLI then hung
/// up mid-stream and the daemon logged `uds rpc connection failed` once per
/// reindex (#9933).
/// What: over a real socket, a stream opened while the run is live delivers the
/// terminal event and then reads `None` — the client's reading of the server's
/// `end` frame. A server whose write failed would leave no terminal frame, and
/// the read would report `NoResponse` instead. The producer must also drop its
/// broadcast subscription on its own, with the client still connected.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_live_reindex_stream_ends_with_an_end_frame_after_its_terminal_event() {
    let (state, rpc) = routers(&["rs"]).await;
    let start = serde_json::json!({ "event": "start", "total_files": 1 });
    let progress = plant_progress(
        &state,
        "rs",
        ReindexStatus::Running,
        std::slice::from_ref(&start),
    )
    .await;
    let served = serve_on_socket(Arc::new(rpc)).await;

    let mut stream: FramedStream<serde_json::Value> =
        trusty_common::uds::send_framed_stream_request(
            &served.socket,
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 9933,
                "method": streams::METHOD_INDEX_REINDEX_STREAM,
                "params": { "index_id": "rs" }, "stream": true,
            }),
            GENEROUS,
        )
        .await
        .expect("open the reindex stream over a real socket");
    let first = next_frame(&mut stream)
        .await
        .expect("the replayed start event must arrive");
    assert_eq!(first.expect("not an error frame"), start);

    let complete = serde_json::json!({ "event": "complete", "indexed": 1, "elapsed_ms": 5 });
    progress
        .push_terminal(ReindexStatus::Complete, complete.clone())
        .await;
    let terminal = next_frame(&mut stream)
        .await
        .expect("the terminal event must arrive");
    assert_eq!(terminal.expect("not an error frame"), complete);

    let after = next_frame(&mut stream).await;
    assert!(
        after.is_none(),
        "the stream must end with an `end` frame after its terminal event, got {after:?}"
    );

    let mut unsubscribed = false;
    for _ in 0..400 {
        if progress.sender.receiver_count() == 0 {
            unsubscribed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        unsubscribed,
        "the producer must drop its subscription once the run is terminal"
    );

    drop(stream);
    served.stop().await;
}

/// Why: `reindex_stream` opens through `ReindexProgress::subscribe_with_replay`,
/// which takes the replay buffer and the live subscription under one lock hold
/// that `push` also takes across its append and its broadcast — so no event can
/// straddle an open (#6386). This pins the two orderings a consumer depends on:
/// an event pushed strictly BEFORE the open arrives once, from the replay; one
/// pushed strictly AFTER arrives once, live. A rewrite that replayed twice or
/// dropped the replay changes one of those two counts.
///
/// The concurrent case — an event pushed WHILE the stream opens — is
/// `a_stream_opening_against_live_pushes_loses_and_repeats_nothing` below.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn an_event_either_side_of_the_stream_opening_is_delivered_exactly_once() {
    let (state, rpc) = routers(&["rs"]).await;
    let before = serde_json::json!({ "event": "start", "total_files": 2 });
    let progress = plant_progress(
        &state,
        "rs",
        ReindexStatus::Running,
        std::slice::from_ref(&before),
    )
    .await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;

    let after = serde_json::json!({ "event": "progress", "indexed": 1, "total": 2 });
    progress.push(after.clone()).await;

    // Two items, in that order: a third would mean the replayed event was also
    // delivered live, and a first item of `after` would mean the replay was lost.
    assert_eq!(
        take_items(&mut items, 2).await,
        vec![before, after],
        "each event either side of the open must arrive exactly once, in order"
    );

    // The next item is the next event and nothing left over from the open.
    let latest = serde_json::json!({ "event": "complete", "indexed": 2, "elapsed_ms": 3 });
    progress.push(latest.clone()).await;
    assert_eq!(take_items(&mut items, 1).await, vec![latest]);
}

/// Why: #6386 end to end, on the transport a dashboard actually reads. The unit
/// coverage in `reindex::progress_race_tests` proves `subscribe_with_replay` is
/// exactly-once; this proves the socket producer built on it delivers that
/// sequence whole — replay first, live after, nothing dropped between them — while
/// a reindex is emitting. Before the fix an event pushed inside the open reached
/// neither path, and a dashboard's progress simply skipped a batch with no error
/// anywhere.
///
/// What: opens the real stream while eight writers push a known set through the
/// real `ReindexProgress::push`, then reads exactly as many items as were pushed
/// and checks the delivered `seq` values are that set — a lost event shows up as a
/// missing one, a repeated event as a missing one plus a duplicate.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_stream_opening_against_live_pushes_loses_and_repeats_nothing() {
    /// Below the 256-slot broadcast channel and the 500-event replay cap, so a
    /// lag or a truncation here would be a broken fixture rather than a finding.
    const EVENTS: usize = 120;
    const WRITERS: usize = 8;

    let (state, rpc) = routers(&["rs"]).await;
    let progress = plant_progress(&state, "rs", ReindexStatus::Running, &[]).await;
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let writers: Vec<_> = (0..WRITERS)
        .map(|_| {
            let progress = Arc::clone(&progress);
            let next = Arc::clone(&next);
            tokio::spawn(async move {
                loop {
                    let seq = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if seq >= EVENTS {
                        return;
                    }
                    progress.push(serde_json::json!({ "seq": seq })).await;
                }
            })
        })
        .collect();

    // Opened while the writers are mid-run, which is the whole point.
    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;

    let delivered = take_items(&mut items, EVENTS).await;
    for writer in writers {
        writer.await.expect("no writer may panic");
    }

    let seen: std::collections::BTreeSet<u64> = delivered
        .iter()
        .map(|item| {
            item["seq"]
                .as_u64()
                .unwrap_or_else(|| panic!("every item must be a planted event, got {item}"))
        })
        .collect();
    let expected: std::collections::BTreeSet<u64> = (0..EVENTS as u64).collect();
    let missing: Vec<u64> = expected.difference(&seen).copied().collect();
    assert!(
        missing.is_empty(),
        "{count} of the first {EVENTS} items were not the {EVENTS} pushed events — seq \
         {missing:?} never arrived, so the open either lost them or repeated something else",
        count = missing.len(),
    );
}

/// Why: a stream that opened and then ended empty would read to a consumer as
/// "the reindex produced nothing" rather than "there is no reindex". This pins
/// the refusal for an index with no recorded reindex as a terminal error frame
/// carrying the read surface's not-found code.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn an_unknown_index_is_refused_before_the_reindex_stream_opens() {
    let (_state, rpc) = routers(&["rs"]).await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "never-reindexed-6285" }),
    )
    .await;
    let refusal = tokio::time::timeout(GENEROUS, items.recv())
        .await
        .expect("the refusal must arrive rather than hang")
        .expect("a refused stream carries one item")
        .expect_err("that item must be the refusal, not a progress event");
    assert_eq!(
        refusal.code, CODE_NOT_FOUND,
        "the socket must report an index with no recorded reindex as not found: {refusal:?}"
    );
}

// -------------------------------------------------------- status streams ---

/// Why: `{"type":"connected"}` is what tells a dashboard its subscription is
/// live before any event exists. A socket stream that omitted it would leave a
/// consumer unable to distinguish "subscribed, nothing happening" from "not
/// subscribed yet".
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn the_status_stream_opens_with_the_connected_frame() {
    let (_state, rpc) = routers(&[]).await;

    let mut items = open_stream(&rpc, streams::METHOD_STATUS_STREAM, serde_json::Value::Null).await;
    let over_socket = take_items(&mut items, 1).await;

    assert_eq!(
        over_socket,
        vec![serde_json::json!({ "type": "connected" })],
        "the socket stream must open with the connected frame"
    );
}

/// Why: the connected frame is written by the producer itself, so it would match
/// even if the socket never reached the shared broadcaster. This emits a REAL
/// `DaemonEvent` and asserts the stream carries its document — which it can only
/// do by reading `state.events`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_daemon_event_reaches_the_socket_stream() {
    let (state, rpc) = routers(&[]).await;

    // The subscription must exist before the event is emitted: a broadcast
    // delivers only to receivers already attached, and the producer subscribes
    // before it writes the connected frame.
    let mut items = open_stream(&rpc, streams::METHOD_STATUS_STREAM, serde_json::Value::Null).await;
    assert_eq!(take_items(&mut items, 1).await.len(), 1, "connected frame");

    let emitted = DaemonEvent::IndexRegistered {
        id: "streams-6285".to_string(),
    };
    state
        .events
        .send(emitted.clone())
        .expect("the socket subscriber is attached");

    let over_socket = take_items(&mut items, 1).await;
    assert_eq!(
        over_socket,
        vec![serde_json::to_value(&emitted).expect("a DaemonEvent serialises")],
        "the socket must carry the event's own JSON"
    );
}

// ------------------------------------------------------ early disconnect ---

/// Why: this is the failure a purely in-process test cannot reach — only a real
/// client can hang up. A dashboard that closes its tab leaves a producer task, a
/// broadcast subscription and a connection behind, and a producer that ignored
/// its `send` error would keep all three for the daemon's lifetime. The
/// subscriber count on the daemon's own broadcaster is the observable: it rises
/// when the stream opens and must fall back once the write fails.
///
/// **Since #7217 the disconnect clears without an event.** `write_stream` reads
/// the closed socket, drops the receiver, and the producer's `Sender::closed()`
/// arm ends the task. The emit below is kept because a live daemon's 2 s status
/// ticker produces one anyway; it is no longer what clears the subscription.
///
/// The limiter is checked on both sides of the disconnect: a stream that took a
/// permit would leak one per abandoned dashboard, which no later event clears.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_client_that_drops_mid_stream_leaves_no_producer_subscribed() {
    // One permit, so a stream that took one would make the admissions below
    // fail outright rather than merely queue behind something.
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_query_guards(
        ConcurrencyLimiter::with_limits(1, 0),
        QueryTimeoutConfig::from_duration(Duration::from_secs(30)),
    ));
    let served = serve_on_socket(Arc::new(streams::register(RpcRouter::new(), &state))).await;

    let before = state.events.receiver_count();
    let mut stream: FramedStream<serde_json::Value> =
        trusty_common::uds::send_framed_stream_request(
            &served.socket,
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 7,
                "method": streams::METHOD_STATUS_STREAM,
                "params": null, "stream": true,
            }),
            GENEROUS,
        )
        .await
        .expect("open the status stream over a real socket");
    let first = tokio::time::timeout(GENEROUS, stream.next_frame())
        .await
        .expect("the first frame must arrive rather than hang")
        .expect("the stream must not end before its connected frame")
        .expect("the connected frame must not be an error");
    assert_eq!(first, serde_json::json!({ "type": "connected" }));
    assert!(
        state.events.receiver_count() > before,
        "the producer must be subscribed while the client is reading"
    );

    // The lane assertion, taken WHILE the stream is open: a permit held for the
    // stream's lifetime would already be visible here.
    let held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("a stream must not hold the daemon's only admission permit");
    drop(held);

    // Hang up mid-stream. Everything after this is the server noticing.
    drop(stream);

    let mut settled = false;
    for _ in 0..400 {
        if state.events.receiver_count() == before {
            settled = true;
            break;
        }
        // The event the write fails on — the ticker's job on a live daemon.
        // `send` errors once the only subscriber is gone, which is the state
        // this loop is waiting for.
        let _ = state.events.send(DaemonEvent::IndexRegistered {
            id: "tick-6285".to_string(),
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        settled,
        "the producer must drop its subscription once its write fails, not hold it \
         for the daemon's lifetime (subscribers: {} before, {} after)",
        before,
        state.events.receiver_count()
    );

    let held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("no permit may be leaked by an abandoned stream");
    drop(held);

    served.stop().await;
}

// ------------------------------------------------- #6524 file-change feed ---

/// `search.index.file_events` replays the ring, then streams live events.
///
/// Why: this is the contract the console proxy and the UI's file-change list
/// are written against — a consumer opening cold must see recent history, and
/// one already open must see the next change. A stream that only did one of the
/// two would look correct in whichever half a reviewer happened to try.
/// What: records one event before opening and one after, then asserts the first
/// arrives as replay and the second arrives live, in that order, with the
/// documented `{path, kind, at_unix_ms}` shape.
/// Test: this test. It does not compile on `origin/main`: neither the method
/// nor `IndexHandle::file_events` exists there.
#[tokio::test]
#[serial_test::parallel]
async fn file_events_replays_the_ring_then_streams_live() {
    let (state, rpc) = routers(&["feed-6524"]).await;
    let handle = state
        .registry
        .get(&IndexId::new("feed-6524"))
        .expect("the fixture index is registered");

    handle
        .file_events
        .record(FileEventKind::Modified, "src/before.rs")
        .await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_FILE_EVENTS,
        serde_json::json!({ "index_id": "feed-6524" }),
    )
    .await;

    handle
        .file_events
        .record(FileEventKind::Removed, "src/after.rs")
        .await;

    let delivered = take_items(&mut items, 2).await;
    assert_eq!(delivered[0]["path"], "src/before.rs");
    assert_eq!(delivered[0]["kind"], "modified");
    assert!(
        delivered[0]["at_unix_ms"].as_u64().unwrap_or(0) > 0,
        "each row carries a real timestamp"
    );
    assert_eq!(delivered[1]["path"], "src/after.rs");
    assert_eq!(delivered[1]["kind"], "removed");
}

/// An unknown index is refused before the file-events stream opens.
///
/// Why: a stream that opened empty for a typo'd id would look like "this repo
/// has had no changes" — the wrong answer, indistinguishable from the right one.
/// What: opens the stream against an unregistered id and asserts the one item
/// is the not-found refusal.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn an_unknown_index_is_refused_before_the_file_events_stream_opens() {
    let (_state, rpc) = routers(&["feed-known-6524"]).await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_FILE_EVENTS,
        serde_json::json!({ "index_id": "no-such-index-6524" }),
    )
    .await;
    let refusal = tokio::time::timeout(GENEROUS, items.recv())
        .await
        .expect("the refusal must arrive rather than hang")
        .expect("a refused stream carries one item")
        .expect_err("that item must be the refusal, not an event");
    assert_eq!(refusal.code, CODE_NOT_FOUND);
}
