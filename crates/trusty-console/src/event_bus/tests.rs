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
use trusty_common::uds::{SocketVerdict, connect_hardened, probe_socket_verdict};

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
    assert_eq!(received.event.id, sent_id);
}

#[tokio::test]
async fn ingest_assigns_sequential_console_seqs_starting_at_one() {
    let bus = EventBus::new(EventBusConfig { capacity: 8 });
    let mut rx = bus.subscribe();

    bus.ingest(make_event(None));
    bus.ingest(make_event(None));

    let first = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("recv 1")
        .expect("open");
    let second = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("recv 2")
        .expect("open");

    assert_eq!(
        first.event.seq, 1,
        "a fresh bus with no recovered log starts at seq 1"
    );
    assert_eq!(
        second.event.seq, 2,
        "seq is console-assigned and monotonic, overwriting the producer's own \
         (always 0 from `make_event`)"
    );
}

#[tokio::test]
async fn live_fanout_frames_are_never_marked_persisted() {
    // The write is always still in flight (or was just dropped) at the
    // moment of fan-out — see `bus`'s module docs for the full contract.
    let bus = EventBus::new(EventBusConfig { capacity: 8 });
    let mut rx = bus.subscribe();

    bus.ingest(make_event(None));

    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("recv did not time out")
        .expect("channel still open");
    assert!(
        !received.persisted,
        "a bus with no durable log configured must never claim persistence"
    );
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
    // Because it now gives up as soon as it has read MAX_LINE_BYTES (the
    // CRITICAL fix this test also covers), it can close the connection
    // before this write — or the trailing newline below — finishes; a
    // broken pipe here is the expected shape of "the listener disconnected
    // early", not a test bug, so these are best-effort rather than asserted.
    let oversized = vec![b'a'; super::ingest::MAX_LINE_BYTES + 1];
    let mut stream = dial(&socket).await;
    let _ = stream.write_all(&oversized).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.shutdown().await;

    // #6848 fix round: rather than sleep a fixed interval and assert a
    // negative (which proves nothing about *why* the count stayed at zero),
    // drive a second, independent connection and wait for ITS event to land.
    // Ingest is a single-threaded ring behind one mutex, so once the second
    // connection's event is observably in the bus, the first connection's
    // oversized frame has already been through the same code path — and
    // never bumped `ingested`.
    let event = make_event(None);
    let event_id = event.id;
    let mut line = serde_json::to_vec(&event).expect("serialize event");
    line.push(b'\n');
    let mut second_stream = dial(&socket).await;
    second_stream.write_all(&line).await.expect("write frame");
    second_stream.shutdown().await.expect("half-close");

    wait_until(Duration::from_secs(1), || bus.contains(event_id)).await;
    assert_eq!(
        bus.metrics().ingested,
        1,
        "only the second connection's well-formed frame was ever ingested"
    );
}

#[tokio::test]
async fn unterminated_line_never_grows_past_the_line_cap() {
    // A peer that streams well past the per-line budget with no newline at
    // all must never grow the read buffer past that budget — the fix this
    // proves reads with a `.take(MAX_LINE_BYTES)` budget re-applied to every
    // line, rather than an unbounded `read_until` checked only after it
    // returns (#6848 fix round, CRITICAL finding). `UnixStream::pair` gives a
    // connected socket pair with no filesystem path, so this drives
    // `read_capped_line` directly rather than through the full accept loop.
    let (mut writer, reader) = UnixStream::pair().expect("create a connected pair");

    let total = super::ingest::MAX_LINE_BYTES * 2;
    let write_task = tokio::spawn(async move {
        let chunk = vec![b'a'; 64 * 1024];
        let mut sent = 0usize;
        while sent < total {
            if writer.write_all(&chunk).await.is_err() {
                // The reader dropped its half once `read_capped_line`
                // returned; a write failing here is expected, not a bug.
                break;
            }
            sent += chunk.len();
        }
    });

    let mut buffered = tokio::io::BufReader::new(reader);
    let mut line = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(5),
        super::ingest::read_capped_line(&mut buffered, &mut line),
    )
    .await
    .expect("read_capped_line must return once its budget is exhausted, not hang")
    .expect("a budget-exhausted read is not an I/O error");

    assert!(
        line.len() <= super::ingest::MAX_LINE_BYTES,
        "the line buffer must never grow past the per-line cap, got {} bytes",
        line.len()
    );
    assert_eq!(read, line.len());
    assert!(
        !line.ends_with(b"\n"),
        "no newline was ever sent, so the read must have stopped on the budget alone"
    );

    write_task.abort();
}

#[tokio::test]
async fn stale_socket_file_is_reclaimed_on_bind() {
    // Why: `bind_ingest` now binds through `bind_singleton_hardened` (#6848
    // fix round, HIGH finding) specifically so a console that exits
    // uncleanly (SIGKILL, crash) does not wedge every future bind on the
    // corpse it left behind — `bind_hardened` alone refuses an occupied path
    // forever. See `trusty_common::uds::tests::bind_singleton_takes_over_a_stale_socket_file`
    // for the same contract proven at the trusty-common layer.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-console.sock");

    let dead = bind_ingest(&socket).await.expect("first bind");
    drop(dead); // tokio does not unlink on drop, so the file survives.
    assert!(socket.exists(), "the corpse must still be on disk");

    // Wait for the kernel to actually finish tearing the dropped listener
    // down before asserting the takeover — a condition wait on the socket's
    // own provable state, not a fixed sleep. On macOS a connect can land in
    // the teardown window and spuriously succeed; this loop is the same one
    // `bind_singleton_takes_over_a_stale_socket_file` uses for that reason.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while probe_socket_verdict(&socket, Duration::from_millis(50)).await
        != SocketVerdict::NotServing
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dropped listener never stopped answering connects"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let listener = bind_ingest(&socket)
        .await
        .expect("a stale socket file must be reclaimed, not refused forever");
    drop(listener);
}

#[tokio::test]
async fn idle_connection_is_dropped_after_the_read_timeout() {
    // #6848 fix round, security finding: a connection that never completes a
    // line must not be held open forever — it would hold a concurrency slot
    // and a `BufReader` for no reason. `UnixStream::pair` avoids waiting the
    // real production timeout: `handle_connection_with_timeout` takes it
    // explicitly for exactly this reason.
    let (writer, reader) = UnixStream::pair().expect("create a connected pair");
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));

    let handle = tokio::spawn(super::ingest::handle_connection_with_timeout(
        reader,
        Arc::clone(&bus),
        Duration::from_millis(50),
    ));

    // Send nothing at all. The task must return on its own once the idle
    // timeout elapses — awaited here with a generous outer bound, not slept
    // for — rather than being held open indefinitely.
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("handle_connection_with_timeout must return once idle")
        .expect("the connection task must not panic");

    drop(writer);
    assert_eq!(
        bus.metrics().ingested,
        0,
        "an idle connection that never sent anything ingests nothing"
    );
}

#[tokio::test]
async fn connections_beyond_the_limit_wait_for_a_free_slot() {
    // #6848 fix round, security finding: bound concurrent connections so a
    // burst cannot hand out an unbounded number of tasks and `BufReader`s.
    // `serve_ingest_with_limit` takes the cap explicitly so this test can
    // prove the gate at a limit of 1 instead of opening 257 real sockets.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-console.sock");
    let listener = bind_ingest(&socket).await.expect("bind ingest socket");
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let served_bus = Arc::clone(&bus);
    tokio::spawn(async move {
        super::ingest::serve_ingest_with_limit(listener, served_bus, 1, std::future::pending())
            .await;
    });

    // Occupy the only slot: connect and never send a line, so this
    // connection's handler never returns and its permit is never released.
    let occupant = dial(&socket).await;

    // A second connection is accepted at the kernel level, but with the sole
    // slot held, its handler must not run until the slot frees. Prove that
    // with a bounded, repeatedly-polled negative check — not a single blind
    // sleep — followed by the definite positive once the slot is freed.
    let event = make_event(None);
    let event_id = event.id;
    let mut line = serde_json::to_vec(&event).expect("serialize event");
    line.push(b'\n');
    let mut second = dial(&socket).await;
    second.write_all(&line).await.expect("write frame");
    second.shutdown().await.expect("half-close");

    assert_stays_false(Duration::from_millis(300), || bus.contains(event_id)).await;

    // Free the slot: the occupant's connection closes, its permit releases,
    // and the second connection's handler can finally run.
    drop(occupant);
    wait_until(Duration::from_secs(2), || bus.contains(event_id)).await;
}

#[tokio::test]
async fn shutdown_is_observed_while_the_connection_pool_is_saturated() {
    // #6848 fix round 2, HIGH finding: the permit acquire in
    // `serve_ingest_with_limit` used to sit *outside* the `tokio::select!`
    // that races `shutdown` against `accept_sized`, as a plain
    // `.acquire_owned().await` after the select returned. With every permit
    // held, that acquire never resolves, so the loop never got back around to
    // re-polling `shutdown` — a shutdown signal sent in that state was never
    // observed and the serve loop ran forever. Saturate the capacity-1 pool
    // the same way `connections_beyond_the_limit_wait_for_a_free_slot` does,
    // so a second connection is stuck on the acquire, then fire a real
    // shutdown and prove the loop still returns.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-console.sock");
    let listener = bind_ingest(&socket).await.expect("bind ingest socket");
    let bus = Arc::new(EventBus::new(EventBusConfig::default()));
    let served_bus = Arc::clone(&bus);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let serve_handle = tokio::spawn(async move {
        super::ingest::serve_ingest_with_limit(listener, served_bus, 1, async move {
            let _ = shutdown_rx.await;
        })
        .await;
    });

    // Occupy the only slot: connect and never send a line, so its handler
    // never returns and the permit is never released.
    let occupant = dial(&socket).await;

    // A second connection is accepted at the kernel level, but with the sole
    // slot held, the serve loop is now stuck on `acquire_owned` for it — the
    // exact state where the round-2 bug loses `shutdown`. Confirm it stays
    // un-ingested for a bounded window (which also gives the accept loop
    // time to actually reach that stuck acquire) before firing shutdown.
    let event = make_event(None);
    let event_id = event.id;
    let mut line = serde_json::to_vec(&event).expect("serialize event");
    line.push(b'\n');
    let mut second = dial(&socket).await;
    second.write_all(&line).await.expect("write frame");
    second.shutdown().await.expect("half-close");
    assert_stays_false(Duration::from_millis(300), || bus.contains(event_id)).await;

    shutdown_tx
        .send(())
        .expect("serve loop is still awaiting shutdown");

    tokio::time::timeout(Duration::from_secs(2), serve_handle)
        .await
        .expect(
            "serve_ingest_with_limit must return once shutdown resolves, even with \
             every permit held and a connection stuck on acquire",
        )
        .expect("the serve task must not panic");

    drop(occupant);
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

/// Poll `predicate` for the whole of `budget` and fail the moment it becomes
/// true, rather than checking once after a blind sleep.
///
/// Why not a single `sleep(budget)` then one assertion: this fails FAST the
/// instant the forbidden state appears (a broken gate is caught well inside
/// `budget`, not only after it), and it still bounds the check to a fixed
/// window because a negative can only ever be proven "not yet", never
/// "never" — `budget` is that concession, kept short and paired with the
/// definite positive check that follows it in the caller.
async fn assert_stays_false(budget: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        assert!(!predicate(), "condition became true within {budget:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
