//! Tests for the bus socket's accept loop, first-line routing and
//! `events.subscribe` (#9075).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, SubscribeItem, SubscribeParams,
    SubscribeRequest,
};

use super::bus::{EventBus, EventBusConfig};
use super::serve::{Accept, Connection, ServeError, ServeLimits, handle_connection, serve_with};

const BOUND: Duration = Duration::from_secs(5);

fn event() -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Code,
        session: None,
        seq: 0,
        at: chrono::Utc::now(),
        payload: HarnessPayload::Ping,
        id: EventId::new(),
        parent_id: None,
    }
}

fn limits(max_subscribers: usize, accept_error_budget: u32) -> ServeLimits {
    ServeLimits {
        max_subscribers,
        accept_error_budget,
        ..ServeLimits::default()
    }
}

/// One step of a scripted accept source.
enum Step {
    Fail,
    Conn(UnixStream),
}

/// An accept source that plays `steps`, then waits forever.
struct Scripted(Mutex<VecDeque<Step>>);

impl Accept for Scripted {
    async fn accept(&self) -> std::io::Result<UnixStream> {
        let next = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        match next {
            Some(Step::Fail) => Err(std::io::Error::other("injected accept failure")),
            Some(Step::Conn(stream)) => Ok(stream),
            None => std::future::pending().await,
        }
    }
}

/// An accept source that only fails.
struct AlwaysFails;

impl Accept for AlwaysFails {
    async fn accept(&self) -> std::io::Result<UnixStream> {
        // Yield as a real accept does, so a loop that never gives up spins
        // without starving the test's own timeout.
        tokio::task::yield_now().await;
        Err(std::io::Error::other("injected accept failure"))
    }
}

#[tokio::test]
async fn the_accept_loop_gives_up_after_its_error_budget() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let served = tokio::time::timeout(
        BOUND,
        serve_with(&AlwaysFails, bus, limits(4, 3), std::future::pending()),
    )
    .await
    .expect("a listener that only fails must end the serve loop");
    match served {
        Err(ServeError::AcceptFailed { consecutive, .. }) => assert_eq!(consecutive, 3),
        other => panic!("expected AcceptFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn accept_errors_below_the_budget_do_not_stop_the_loop() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let mut producers = Vec::new();
    let mut steps = VecDeque::new();
    for _ in 0..2 {
        // Two failures, then a success that resets the count: four failures
        // in all never reach a budget of three in a row.
        steps.push_back(Step::Fail);
        steps.push_back(Step::Fail);
        let (producer, served) = UnixStream::pair().expect("socket pair");
        steps.push_back(Step::Conn(served));
        producers.push(producer);
    }
    let acceptor = Scripted(Mutex::new(steps));
    let served_bus = Arc::clone(&bus);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(async move {
        serve_with(&acceptor, served_bus, limits(4, 3), async {
            let _ = stopped.await;
        })
        .await
    });

    for mut producer in producers {
        let mut line = serde_json::to_vec(&event()).expect("encode");
        line.push(b'\n');
        producer.write_all(&line).await.expect("write frame");
        producer.shutdown().await.expect("half-close");
    }
    let deadline = tokio::time::Instant::now() + BOUND;
    while bus.metrics().ingested < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "both producers ingested"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = stop.send(());
    let result = tokio::time::timeout(BOUND, serve)
        .await
        .expect("serve stops on shutdown")
        .expect("serve task");
    assert!(result.is_ok(), "shutdown is a clean return, got {result:?}");
}

/// Open a subscription over a socket pair served by `handle_connection`.
async fn subscribe(shared: &Arc<Connection>, params: SubscribeParams) -> BufReader<UnixStream> {
    let (mut client, served) = UnixStream::pair().expect("socket pair");
    let shared = Arc::clone(shared);
    tokio::spawn(async move { handle_connection(served, &shared).await });
    let mut line = serde_json::to_vec(&SubscribeRequest::new(7, params)).expect("encode request");
    line.push(b'\n');
    client.write_all(&line).await.expect("write request");
    BufReader::new(client)
}

/// The next stream frame, as JSON.
async fn frame(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = String::new();
    let read = tokio::time::timeout(BOUND, reader.read_line(&mut line))
        .await
        .expect("a frame within the bound")
        .expect("read frame");
    assert!(read > 0, "the stream ended without a frame");
    serde_json::from_str(&line).expect("frame is JSON")
}

/// The next frame, which must be an item; its `result` as a `SubscribeItem`.
async fn item(reader: &mut BufReader<UnixStream>) -> SubscribeItem {
    let frame = frame(reader).await;
    assert_eq!(frame["stream"], "item", "expected an item frame: {frame}");
    serde_json::from_value(frame["result"].clone()).expect("result is a SubscribeItem")
}

/// The terminal error frame's message.
async fn error_message(reader: &mut BufReader<UnixStream>) -> String {
    let frame = frame(reader).await;
    assert_eq!(frame["stream"], "error", "expected an error frame: {frame}");
    frame["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn since(seq: u64) -> SubscribeParams {
    SubscribeParams {
        since_seq: Some(seq),
        ..SubscribeParams::default()
    }
}

#[tokio::test]
async fn a_first_line_with_jsonrpc_is_routed_to_subscribe() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let shared = Arc::new(Connection::new(Arc::clone(&bus), limits(4, 3)));
    let mut stream = subscribe(&shared, since(0)).await;
    bus.ingest(event());
    match item(&mut stream).await {
        SubscribeItem::Event { event, persisted } => {
            assert_eq!(event.seq, 1);
            assert!(!persisted, "an event from the ring is not marked persisted");
        }
        other => panic!("expected the event, got {other:?}"),
    }
}

#[tokio::test]
async fn a_subscriber_beyond_the_limit_is_refused() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let shared = Arc::new(Connection::new(bus, limits(1, 3)));
    let _first = subscribe(&shared, SubscribeParams::default()).await;
    // Give the first subscription time to take the only slot.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut second = subscribe(&shared, SubscribeParams::default()).await;
    let message = error_message(&mut second).await;
    assert!(message.contains("subscriber slot"), "got {message:?}");
}

#[tokio::test]
async fn a_since_seq_ahead_of_the_bus_is_refused() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    bus.ingest(event());
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, since(5)).await;
    let message = error_message(&mut stream).await;
    assert!(message.contains("past the bus"), "got {message:?}");
}

#[tokio::test]
async fn a_replay_over_events_no_tier_holds_starts_with_a_gap() {
    // No log, and a 2-event ring: seqs 1-3 are gone from every tier.
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 2 }));
    for _ in 0..5 {
        bus.ingest(event());
    }
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, since(0)).await;
    assert_eq!(item(&mut stream).await, SubscribeItem::Gap { skipped: 3 });
    for want in 4..=5 {
        match item(&mut stream).await {
            SubscribeItem::Event { event, .. } => assert_eq!(event.seq, want),
            other => panic!("expected seq {want}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn replay_reads_the_log_up_to_the_ring_then_the_ring() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (log, recovered) = super::log::DurableLog::open(super::log::LogConfig {
        dir: tmp.path().join("event_log"),
        retain_days: 7,
    })
    .await
    .expect("open log");
    let bus = Arc::new(EventBus::with_log(
        EventBusConfig { capacity: 2 },
        Some(log),
        recovered.next_seq,
    ));
    for _ in 0..5 {
        bus.ingest(event());
    }
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        let written = bus.replay_since(0).await.expect("log").expect("replay");
        if written.len() == 5 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the writer caught up"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, since(1)).await;
    for (want, from_log) in [(2, true), (3, true), (4, false), (5, false)] {
        match item(&mut stream).await {
            SubscribeItem::Event { event, persisted } => {
                assert_eq!(event.seq, want, "no repeat and no hole at the boundary");
                assert_eq!(persisted, from_log, "seq {want} comes from the right tier");
            }
            other => panic!("expected seq {want}, got {other:?}"),
        }
    }
}
