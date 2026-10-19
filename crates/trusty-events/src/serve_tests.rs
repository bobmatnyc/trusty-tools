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

/// A resume at `seq` naming `id` as the event held there (#9075).
fn resume_at(seq: u64, id: Option<EventId>) -> SubscribeParams {
    SubscribeParams {
        since_seq: Some(seq),
        since_id: id,
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
    bus.ingest(event());
    let shared = Arc::new(Connection::new(bus, limits(1, 3)));
    let mut first = subscribe(&shared, since(0)).await;
    // An item on the first stream proves it holds the only slot.
    assert!(matches!(
        item(&mut first).await,
        SubscribeItem::Event { .. }
    ));
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
    let first = event();
    let first_id = first.id;
    bus.ingest(first);
    for _ in 0..4 {
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
    // Seq 1 has left the 2-event ring, so its id is checked against the log.
    let mut stream = subscribe(&shared, resume_at(1, Some(first_id))).await;
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

// ─── #9075 (HIGH-1): a resume names the event it holds ─────────────────────

/// A bus holding seqs 1-3 in its ring; returns the bus and seq 2's id.
fn three_events() -> (Arc<EventBus>, EventId) {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    bus.ingest(event());
    let second = event();
    let id = second.id;
    bus.ingest(second);
    bus.ingest(event());
    (bus, id)
}

#[tokio::test]
async fn a_resume_whose_since_id_names_another_event_is_refused() {
    let (bus, _) = three_events();
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, resume_at(2, Some(EventId::new()))).await;
    let message = error_message(&mut stream).await;
    assert!(message.contains("since_id"), "got {message:?}");
}

#[tokio::test]
async fn a_resume_without_since_id_is_refused() {
    let (bus, _) = three_events();
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, since(2)).await;
    let message = error_message(&mut stream).await;
    assert!(message.contains("since_id"), "got {message:?}");
}

#[tokio::test]
async fn a_resume_whose_since_id_matches_continues_after_it() {
    let (bus, id) = three_events();
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, resume_at(2, Some(id))).await;
    match item(&mut stream).await {
        SubscribeItem::Event { event, .. } => assert_eq!(event.seq, 3),
        other => panic!("expected seq 3, got {other:?}"),
    }
}

#[tokio::test]
async fn a_resume_from_a_seq_no_tier_holds_is_refused() {
    // No log and a 2-event ring: seq 1 cannot be checked, so it is refused
    // rather than trusted.
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 2 }));
    let first = event();
    let id = first.id;
    bus.ingest(first);
    for _ in 0..4 {
        bus.ingest(event());
    }
    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let mut stream = subscribe(&shared, resume_at(1, Some(id))).await;
    let message = error_message(&mut stream).await;
    assert!(message.contains("since_id"), "got {message:?}");
}

// ─── #10071: opt-in heartbeats ───────────────────────────────────────────────

/// Live-only params that ask for a heartbeat every `ms`.
fn beating(ms: u64) -> SubscribeParams {
    SubscribeParams {
        heartbeat_ms: Some(ms),
        ..SubscribeParams::default()
    }
}

/// The next item that is not a heartbeat.
async fn next_non_heartbeat(reader: &mut BufReader<UnixStream>) -> SubscribeItem {
    loop {
        match item(reader).await {
            SubscribeItem::Heartbeat { .. } => {}
            other => return other,
        }
    }
}

/// A subscription that asks gets a heartbeat before anything else, even
/// before its replay, then one per interval; `head` is the bus's last seq.
#[tokio::test]
async fn a_subscription_that_asks_gets_a_heartbeat_first_then_on_its_interval() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    bus.ingest(event());
    bus.ingest(event());
    let shared = Arc::new(Connection::new(Arc::clone(&bus), limits(4, 3)));
    let params = SubscribeParams {
        since_seq: Some(0),
        heartbeat_ms: Some(100),
        ..SubscribeParams::default()
    };
    let mut stream = subscribe(&shared, params).await;
    assert_eq!(
        item(&mut stream).await,
        SubscribeItem::Heartbeat { head: 2 }
    );
    for want in 1..=2 {
        match item(&mut stream).await {
            SubscribeItem::Event { event, .. } => assert_eq!(event.seq, want),
            other => panic!("expected seq {want}, got {other:?}"),
        }
    }
    assert_eq!(
        item(&mut stream).await,
        SubscribeItem::Heartbeat { head: 2 }
    );
    bus.ingest(event());
    match next_non_heartbeat(&mut stream).await {
        SubscribeItem::Event { event, .. } => assert_eq!(event.seq, 3),
        other => panic!("expected seq 3, got {other:?}"),
    }
    assert_eq!(
        item(&mut stream).await,
        SubscribeItem::Heartbeat { head: 3 }
    );
}

/// A subscription that does not ask is sent exactly what it was before
/// #10071: no heartbeat, however long it waits, and the event first.
#[tokio::test]
async fn a_subscription_that_does_not_ask_gets_no_heartbeat() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let shared = Arc::new(Connection::new(Arc::clone(&bus), limits(4, 3)));
    let mut stream = subscribe(&shared, SubscribeParams::default()).await;
    // Several of the shortest interval a subscription could ask for.
    tokio::time::sleep(Duration::from_millis(400)).await;
    bus.ingest(event());
    match item(&mut stream).await {
        SubscribeItem::Event { event, .. } => assert_eq!(event.seq, 1),
        other => panic!("a subscription that did not ask got {other:?}"),
    }
}

/// `heartbeat_ms` outside its bounds is refused with `invalid_params`, before
/// any item; the bounds themselves and a probe's 500 ms are accepted.
#[tokio::test]
async fn a_heartbeat_ms_outside_its_bounds_is_refused() {
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let shared = Arc::new(Connection::new(bus, limits(16, 3)));
    for refused in [0, 99, 3_600_001, u64::MAX] {
        let mut stream = subscribe(&shared, beating(refused)).await;
        let frame = frame(&mut stream).await;
        assert_eq!(frame["stream"], "error", "{refused}: {frame}");
        assert_eq!(
            frame["error"]["code"],
            trusty_common::uds::server::CODE_INVALID_PARAMS,
            "{refused}: {frame}"
        );
        let message = frame["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("heartbeat_ms"), "{refused}: {message:?}");
    }
    for accepted in [100, 500, 3_600_000] {
        let mut stream = subscribe(&shared, beating(accepted)).await;
        assert_eq!(
            item(&mut stream).await,
            SubscribeItem::Heartbeat { head: 0 },
            "{accepted}"
        );
    }
}

/// A subscriber that stops reading fills its own buffer; ingest goes on and
/// another subscriber keeps getting its events and its heartbeats.
#[tokio::test]
async fn a_reader_that_stops_reading_does_not_stop_ingest_or_other_heartbeats() {
    const EVENTS: u64 = 2_000;
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let shared = Arc::new(Connection::new(Arc::clone(&bus), limits(4, 3)));
    let _stalled = subscribe(&shared, beating(100)).await;
    let mut reading = subscribe(&shared, beating(100)).await;
    assert_eq!(
        item(&mut reading).await,
        SubscribeItem::Heartbeat { head: 0 }
    );
    for _ in 0..EVENTS {
        bus.ingest(event());
    }
    assert_eq!(
        bus.metrics().ingested,
        EVENTS,
        "ingest never waits on a reader"
    );
    let mut last = 0;
    while last < EVENTS {
        match next_non_heartbeat(&mut reading).await {
            SubscribeItem::Event { event, .. } => last = event.seq,
            other => panic!("expected an event, got {other:?}"),
        }
    }
    assert_eq!(
        item(&mut reading).await,
        SubscribeItem::Heartbeat { head: EVENTS }
    );
}

// ─── #10071 review: a slow log read neither silences nor outlives its reader ─

/// A day file that is a FIFO: reading it blocks until a writer opens it.
/// Released on drop, so no blocking read outlives the test's runtime.
struct Fifo(std::path::PathBuf);

impl Fifo {
    fn make(path: std::path::PathBuf) -> Self {
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo {}", path.display());
        Self(path)
    }

    /// Let a reader blocked on this FIFO through: it reads an empty file.
    fn release(&self) {
        let path = self.0.clone();
        std::thread::spawn(move || {
            let _ = std::fs::OpenOptions::new().write(true).open(path);
        });
    }

    /// Set the returned flag once anything opens this FIFO to read it.
    fn watch_for_a_reader(&self) -> Arc<std::sync::atomic::AtomicBool> {
        let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&opened);
        let path = self.0.clone();
        std::thread::spawn(move || {
            if std::fs::OpenOptions::new().write(true).open(path).is_ok() {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        opened
    }
}

impl Drop for Fifo {
    fn drop(&mut self) {
        self.release();
    }
}

/// The day file `days_ago` days back in `dir`.
fn day_file(dir: &std::path::Path, days_ago: i64) -> std::path::PathBuf {
    let date = chrono::Utc::now().date_naive() - chrono::Duration::days(days_ago);
    dir.join(format!("{}.ndjson", date.format("%Y-%m-%d")))
}

/// Write events with seqs `seqs` to `path` as the log writer would; returns them.
fn write_day_file(
    path: &std::path::Path,
    seqs: std::ops::RangeInclusive<u64>,
) -> Vec<HarnessEvent> {
    let mut text = Vec::new();
    let mut events = Vec::new();
    for seq in seqs {
        let mut ev = event();
        ev.seq = seq;
        text.extend(serde_json::to_vec(&ev).expect("encode"));
        text.push(b'\n');
        events.push(ev);
    }
    std::fs::write(path, text).expect("write a day file");
    events
}

/// A bus on a log already holding the day files the caller wrote in `dir`.
async fn bus_on(dir: &std::path::Path) -> Arc<EventBus> {
    let (log, recovered) = super::log::DurableLog::open(super::log::LogConfig {
        dir: dir.to_path_buf(),
        retain_days: 7,
    })
    .await
    .expect("open log");
    Arc::new(EventBus::with_log(
        EventBusConfig::default(),
        Some(log),
        recovered.next_seq,
    ))
}

/// Replay reads the log a day file at a time and beats while a read is slow:
/// the first file's events arrive, then heartbeats keep coming while the
/// next file blocks.
#[tokio::test]
async fn a_slow_replay_read_still_beats() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().join("event_log");
    std::fs::create_dir_all(&dir).expect("log dir");
    write_day_file(&day_file(&dir, 2), 1..=3);
    let bus = bus_on(&dir).await;
    let _slow = Fifo::make(day_file(&dir, 1));

    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let params = SubscribeParams {
        since_seq: Some(0),
        heartbeat_ms: Some(100),
        ..SubscribeParams::default()
    };
    let mut stream = subscribe(&shared, params).await;
    assert_eq!(
        item(&mut stream).await,
        SubscribeItem::Heartbeat { head: 3 }
    );
    for want in 1..=3 {
        match item(&mut stream).await {
            SubscribeItem::Event { event, persisted } => {
                assert_eq!((event.seq, persisted), (want, true));
            }
            other => panic!("expected logged seq {want}, got {other:?}"),
        }
    }
    for _ in 0..3 {
        assert_eq!(
            item(&mut stream).await,
            SubscribeItem::Heartbeat { head: 3 }
        );
    }
}

/// The `since_id` check reads the log too, and beats while it does.
#[tokio::test]
async fn a_slow_since_id_check_still_beats() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().join("event_log");
    std::fs::create_dir_all(&dir).expect("log dir");
    let held = write_day_file(&day_file(&dir, 1), 1..=3);
    let bus = bus_on(&dir).await;
    // An older day file, read first, that blocks.
    let _slow = Fifo::make(day_file(&dir, 2));

    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let params = SubscribeParams {
        since_seq: Some(2),
        since_id: Some(held[1].id),
        heartbeat_ms: Some(100),
        ..SubscribeParams::default()
    };
    let mut stream = subscribe(&shared, params).await;
    for _ in 0..3 {
        assert_eq!(
            item(&mut stream).await,
            SubscribeItem::Heartbeat { head: 3 }
        );
    }
}

/// A pump whose reader has gone stops reading the log: once the blocked
/// read is let through, the next day file is never opened.
#[tokio::test]
async fn a_pump_whose_reader_left_stops_reading_the_log() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().join("event_log");
    std::fs::create_dir_all(&dir).expect("log dir");
    write_day_file(&day_file(&dir, 3), 1..=3);
    let bus = bus_on(&dir).await;
    let first = Fifo::make(day_file(&dir, 2));
    let second = Fifo::make(day_file(&dir, 1));
    let second_read = second.watch_for_a_reader();

    let shared = Arc::new(Connection::new(bus, limits(4, 3)));
    let stream = subscribe(&shared, since(0)).await;
    // The pump is now blocked on `first`. The reader leaves.
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stream);
    // Longer than the stream's peer-departure poll.
    tokio::time::sleep(Duration::from_secs(1)).await;
    first.release();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !second_read.load(std::sync::atomic::Ordering::SeqCst),
        "a pump with no reader went on to read the next day file"
    );
    // `second` has a writer thread waiting; let it go.
    let _reader = std::fs::File::open(&second.0);
}
