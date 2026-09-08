//! Tests for the console event-bus core (issue #6848).
//!
//! `bus_*` tests drive [`super::bus::EventBus`] directly; `ingest_*` and
//! `malformed_*`/`oversized_*` tests drive [`super::ingest`] over a real
//! Unix socket in a tempdir, matching this workspace's convention for UDS
//! listener tests (see `search_uds`, `memory_uds`, `webhook`).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use trusty_common::control_bus::{EventId, HarnessEvent, HarnessPayload, HarnessSource};
use trusty_common::uds::connect_hardened;

use super::bus::{EventBus, EventBusConfig, IngestOutcome};
use super::ingest::{bind_ingest, serve_ingest};

/// Build a minimal `HarnessEvent` fixture. `id` defaults to a fresh
/// [`EventId`] when `None`, so tests that need a specific duplicate id can
/// pass one in.
fn make_event(id: Option<EventId>) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: None,
        seq: 0,
        at: Utc::now(),
        payload: HarnessPayload::Ping,
        id: id.unwrap_or_default(),
        parent_id: None,
    }
}

// ─── EventBus, driven directly ─────────────────────────────────────────────

#[test]
fn zero_capacity_is_raised_to_one() {
    // Would panic inside `tokio::sync::broadcast::channel(0)` otherwise.
    let bus = EventBus::new(EventBusConfig { capacity: 0 });
    assert_eq!(bus.ingest(make_event(None)), IngestOutcome::Ingested);
    assert_eq!(bus.len(), 1);
}

#[test]
fn duplicate_id_is_deduped() {
    let bus = EventBus::new(EventBusConfig { capacity: 8 });
    let id = EventId::new();
    assert_eq!(bus.ingest(make_event(Some(id))), IngestOutcome::Ingested);
    assert_eq!(bus.ingest(make_event(Some(id))), IngestOutcome::Deduped);
    assert_eq!(bus.len(), 1);
    let metrics = bus.metrics();
    assert_eq!(metrics.ingested, 1);
    assert_eq!(metrics.deduped, 1);
    assert_eq!(metrics.evicted, 0);
}

#[test]
fn eviction_at_capacity_drops_the_oldest() {
    let bus = EventBus::new(EventBusConfig { capacity: 2 });
    let first = make_event(None);
    let first_id = first.id;
    let second = make_event(None);
    let third = make_event(None);

    assert_eq!(bus.ingest(first), IngestOutcome::Ingested);
    assert_eq!(bus.ingest(second), IngestOutcome::Ingested);
    assert!(bus.contains(first_id));

    assert_eq!(bus.ingest(third), IngestOutcome::Ingested);
    assert_eq!(bus.len(), 2, "ring never exceeds its configured capacity");
    assert!(
        !bus.contains(first_id),
        "the oldest event is evicted to make room for the newest"
    );

    let metrics = bus.metrics();
    assert_eq!(metrics.ingested, 3);
    assert_eq!(metrics.evicted, 1);
}

#[tokio::test]
async fn a_subscriber_receives_ingested_events() {
    let bus = EventBus::new(EventBusConfig { capacity: 8 });
    let mut rx = bus.subscribe();
    let event = make_event(None);
    let sent_id = event.id;

    bus.ingest(event);

    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("recv did not time out")
        .expect("channel still open");
    assert_eq!(received.id, sent_id);
}

// ─── the UDS ingest listener, over a real socket ───────────────────────────

/// Bind a fresh ingest listener under `tmp` and start serving it in the
/// background. Returns the socket path and the bus it feeds.
async fn spawn_test_listener(tmp: &std::path::Path) -> (std::path::PathBuf, Arc<EventBus>) {
    let socket = tmp.join("sockets").join("trusty-console.sock");
    let listener = bind_ingest(&socket).await.expect("bind ingest socket");
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let served_bus = Arc::clone(&bus);
    tokio::spawn(async move {
        serve_ingest(listener, served_bus, std::future::pending()).await;
    });
    (socket, bus)
}

async fn dial(socket: &std::path::Path) -> UnixStream {
    connect_hardened(socket)
        .await
        .expect("connect to ingest socket")
}

#[tokio::test]
async fn ingest_over_uds_socket_reaches_the_bus() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, bus) = spawn_test_listener(tmp.path()).await;

    let event = make_event(None);
    let sent_id = event.id;
    let mut line = serde_json::to_vec(&event).expect("serialize event");
    line.push(b'\n');

    let mut stream = dial(&socket).await;
    stream.write_all(&line).await.expect("write frame");
    stream.shutdown().await.expect("half-close");

    wait_until(Duration::from_secs(1), || bus.metrics().ingested >= 1).await;
    assert!(bus.contains(sent_id));
    assert_eq!(bus.metrics().deduped, 0);
}

#[tokio::test]
async fn malformed_line_does_not_kill_the_listener() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, bus) = spawn_test_listener(tmp.path()).await;

    let mut stream = dial(&socket).await;
    stream
        .write_all(b"not valid json at all\n")
        .await
        .expect("write malformed line");

    let good = make_event(None);
    let good_id = good.id;
    let mut line = serde_json::to_vec(&good).expect("serialize event");
    line.push(b'\n');
    stream.write_all(&line).await.expect("write valid frame");
    stream.shutdown().await.expect("half-close");

    wait_until(Duration::from_secs(1), || bus.metrics().ingested >= 1).await;
    assert!(
        bus.contains(good_id),
        "the valid frame after a malformed one is still ingested"
    );

    // The listener itself is unaffected: a second, unrelated connection still
    // gets served.
    let other = make_event(None);
    let other_id = other.id;
    let mut other_line = serde_json::to_vec(&other).expect("serialize event");
    other_line.push(b'\n');
    let mut second_stream = dial(&socket).await;
    second_stream
        .write_all(&other_line)
        .await
        .expect("write second connection's frame");
    second_stream.shutdown().await.expect("half-close");

    wait_until(Duration::from_secs(1), || bus.contains(other_id)).await;
}

#[tokio::test]
async fn oversized_line_ends_only_its_own_connection() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, bus) = spawn_test_listener(tmp.path()).await;

    // One byte over the listener's per-line budget, with no newline — the
    // listener must give up on this connection rather than buffer forever.
    let oversized = vec![b'a'; super::ingest::MAX_LINE_BYTES + 1];
    let mut stream = dial(&socket).await;
    stream
        .write_all(&oversized)
        .await
        .expect("write oversized payload");
    stream.write_all(b"\n").await.expect("terminate");
    stream.shutdown().await.expect("half-close");

    // The connection is dropped without being ingested or crashing anything.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bus.metrics().ingested, 0);

    // A fresh connection still works.
    let event = make_event(None);
    let event_id = event.id;
    let mut line = serde_json::to_vec(&event).expect("serialize event");
    line.push(b'\n');
    let mut second_stream = dial(&socket).await;
    second_stream.write_all(&line).await.expect("write frame");
    second_stream.shutdown().await.expect("half-close");

    wait_until(Duration::from_secs(1), || bus.contains(event_id)).await;
}

/// Poll `predicate` until it is true or `budget` elapses.
///
/// Why: ingest happens on a spawned task, so a test writing to the socket
/// must not assert immediately after `write_all` returns — that only proves
/// the bytes reached the kernel buffer, not that `handle_connection` has run.
async fn wait_until(budget: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if predicate() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition did not become true within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
