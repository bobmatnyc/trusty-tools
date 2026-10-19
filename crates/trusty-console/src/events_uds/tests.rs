//! The bus client against a scripted stub daemon: one test per arm (#9076).
//!
//! Why: the real daemon (`tests/events_uds_live.rs`) cannot produce a
//! malformed frame or a stream error on demand. A stub that decodes the real
//! request frame and writes real stream frames can, so every way a
//! subscription ends is driven here and its visible state asserted.
//! What: [`Stub`] binds a socket in a tempdir and hands each subscription to
//! the test as a [`Conn`], which writes items, errors or raw bytes when told.
//! Connections that send no request line (the client's liveness probe) are
//! dropped without reaching the test.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, SubscribeItem, SubscribeRequest,
};
use trusty_common::uds::encode_frame;
use trusty_common::uds::server::{RpcError, RpcStreamFrame};

use super::client::{self, ClientConfig, Start};
use super::relay::{BusState, BusStatus, EventsRelay, RelayItem};
use super::token::{ResumeToken, ResumeTokenError};

/// How long any one step of a test may take.
const BOUND: Duration = Duration::from_secs(5);

// ─── the stub daemon ─────────────────────────────────────────────────────────

struct Stub {
    socket: PathBuf,
    conns: mpsc::UnboundedReceiver<Conn>,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// One subscription the stub accepted.
struct Conn {
    request: SubscribeRequest,
    stream: UnixStream,
}

fn bind_stub(socket: &Path) -> Stub {
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the stub socket");
    let (tx, conns) = mpsc::unbounded_channel();
    let accept = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).await.is_err() {
                continue;
            }
            // A liveness probe connects and sends nothing.
            let Ok(request) = SubscribeRequest::decode(&line) else {
                continue;
            };
            let _ = tx.send(Conn {
                request,
                stream: reader.into_inner(),
            });
        }
    });
    Stub {
        socket: socket.to_path_buf(),
        conns,
        accept,
    }
}

fn stub_in(dir: &Path) -> Stub {
    bind_stub(&dir.join("sockets").join("trusty-events.sock"))
}

impl Stub {
    async fn next_conn(&mut self) -> Conn {
        tokio::time::timeout(BOUND, self.conns.recv())
            .await
            .expect("the client subscribed within the bound")
            .expect("the stub is accepting")
    }

    /// No subscription arrives within `quiet`.
    async fn no_conn_within(&mut self, quiet: Duration) {
        if let Ok(Some(conn)) = tokio::time::timeout(quiet, self.conns.recv()).await {
            panic!("unexpected subscription: {:?}", conn.request.params);
        }
    }
}

impl Conn {
    async fn frame(&mut self, frame: RpcStreamFrame) {
        let bytes = encode_frame(&frame).expect("encode a stream frame");
        self.raw(&bytes).await;
    }

    async fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("stub write");
        self.stream.flush().await.expect("stub flush");
    }

    async fn item(&mut self, item: &SubscribeItem) {
        let value = serde_json::to_value(item).expect("encode an item");
        self.frame(RpcStreamFrame::item(json!(1), value)).await;
    }

    async fn event(&mut self, event: &HarnessEvent) {
        self.item(&SubscribeItem::Event {
            event: Box::new(event.clone()),
            persisted: false,
        })
        .await;
    }

    async fn error(&mut self, error: RpcError) {
        self.frame(RpcStreamFrame::error(json!(1), error)).await;
    }

    fn since(&self) -> (Option<u64>, Option<EventId>) {
        (self.request.params.since_seq, self.request.params.since_id)
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn event(seq: u64) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some("s".to_string()),
        seq,
        at: chrono::Utc::now(),
        payload: HarnessPayload::Ping,
        id: EventId::new(),
        parent_id: None,
    }
}

/// Short liveness checks; a backoff long enough that a test sees the
/// `Reconnecting` state before the next attempt replaces it.
fn fast(socket: &Path) -> ClientConfig {
    let mut config = ClientConfig::new(socket.to_path_buf());
    config.liveness_interval = Duration::from_millis(50);
    config.backoff_initial = Duration::from_millis(300);
    config.backoff_max = Duration::from_millis(300);
    config
}

/// The client task, aborted when the test ends.
struct Running(tokio::task::JoinHandle<()>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn run(config: ClientConfig) -> (Arc<EventsRelay>, Running) {
    let relay = Arc::new(EventsRelay::new());
    let task = client::spawn(config, Arc::clone(&relay));
    (relay, Running(task))
}

async fn status_until(
    relay: &EventsRelay,
    what: &str,
    f: impl Fn(&BusStatus) -> bool,
) -> BusStatus {
    let mut watch = relay.watch();
    match tokio::time::timeout(BOUND, watch.wait_for(|s| f(s))).await {
        Ok(Ok(status)) => status.clone(),
        _ => panic!("status never reached {what}; last: {:?}", relay.status()),
    }
}

async fn next_item(items: &mut broadcast::Receiver<RelayItem>) -> RelayItem {
    tokio::time::timeout(BOUND, items.recv())
        .await
        .expect("a relayed item within the bound")
        .expect("the relay is open")
}

async fn next_event(items: &mut broadcast::Receiver<RelayItem>) -> (ResumeToken, HarnessEvent) {
    match next_item(items).await {
        RelayItem::Event { token, event, .. } => (token, (*event).clone()),
        other => panic!("expected an event, got {other:?}"),
    }
}

// ─── the token ───────────────────────────────────────────────────────────────

#[test]
fn token_round_trips_through_its_string() {
    let ev = event(42);
    let token = ResumeToken::of(&ev);
    let text = token.to_string();
    assert_eq!(text, format!("42:{}", ev.id));
    assert_eq!(text.parse::<ResumeToken>(), Ok(token));
}

#[test]
fn token_rejects_malformed_strings() {
    let id = EventId::new();
    let cases = [
        ("42", "no separator"),
        (&format!("x:{id}") as &str, "no seq"),
        ("42:not-a-uuid", "no id"),
        (&format!("-1:{id}") as &str, "negative seq"),
    ];
    for (raw, why) in cases {
        let parsed = raw.parse::<ResumeToken>();
        assert!(parsed.is_err(), "{why}: {raw:?} parsed as {parsed:?}");
    }
    assert!(matches!(
        "42".parse::<ResumeToken>(),
        Err(ResumeTokenError::Shape(_))
    ));
}

// ─── the arms ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_gap_is_relayed_not_swallowed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));
    let mut items = relay.subscribe();

    let mut conn = stub.next_conn().await;
    assert_eq!(conn.since(), (None, None), "a new client starts live");
    let first = event(1);
    conn.event(&first).await;
    conn.item(&SubscribeItem::Gap { skipped: 3 }).await;
    conn.event(&event(5)).await;

    let (token, _) = next_event(&mut items).await;
    assert_eq!(token, ResumeToken::of(&first));
    match next_item(&mut items).await {
        RelayItem::Gap { skipped, after } => {
            assert_eq!(skipped, 3);
            assert_eq!(after, Some(ResumeToken::of(&first)));
        }
        other => panic!("the gap was swallowed: got {other:?}"),
    }
    assert_eq!(next_event(&mut items).await.0.seq, 5);
    let status = relay.status();
    assert_eq!((status.gaps, status.skipped), (1, 3));
}

/// A subscription with nothing to say for longer than a request timeout
/// stays open: one connection, and the late event arrives on it.
#[tokio::test]
async fn a_quiet_subscription_is_not_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));
    let mut items = relay.subscribe();

    let mut conn = stub.next_conn().await;
    status_until(&relay, "connected", |s| s.state == BusState::Connected).await;
    // Many liveness intervals, and longer than any per-request timeout here.
    stub.no_conn_within(Duration::from_millis(1500)).await;
    assert_eq!(
        relay.status().state,
        BusState::Connected,
        "a quiet bus is not a dead one"
    );
    conn.event(&event(1)).await;
    assert_eq!(next_event(&mut items).await.0.seq, 1);
}

#[tokio::test]
async fn a_hangup_resumes_with_seq_and_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));
    let mut items = relay.subscribe();

    let mut conn = stub.next_conn().await;
    let held: Vec<_> = (1..=3).map(event).collect();
    for ev in &held {
        conn.event(ev).await;
    }
    for _ in &held {
        next_event(&mut items).await;
    }
    drop(conn);

    let status = status_until(&relay, "reconnecting", |s| {
        s.state == BusState::Reconnecting
    })
    .await;
    assert!(
        status.last_error.is_some(),
        "the hang-up is named: {status:?}"
    );
    let conn = stub.next_conn().await;
    assert_eq!(
        conn.since(),
        (Some(3), Some(held[2].id)),
        "resume names seq and id"
    );
    status_until(&relay, "connected again", |s| {
        s.state == BusState::Connected
    })
    .await;
}

#[tokio::test]
async fn a_refused_resume_resyncs_from_zero_and_drops_duplicates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let mut config = fast(&stub.socket);
    // A resync must not wait out a backoff this long.
    config.backoff_initial = Duration::from_secs(3);
    config.backoff_max = Duration::from_secs(3);
    let (relay, _client) = run(config);
    let mut items = relay.subscribe();

    let mut conn = stub.next_conn().await;
    let (one, two) = (event(1), event(2));
    conn.event(&one).await;
    conn.event(&two).await;
    next_event(&mut items).await;
    next_event(&mut items).await;
    drop(conn);

    let mut conn = stub.next_conn().await;
    assert_eq!(conn.since(), (Some(2), Some(two.id)));
    conn.error(RpcError::invalid_params(
        "since_id is not the event at since_seq 2; resubscribe with since_seq 0",
    ))
    .await;

    let mut conn = tokio::time::timeout(Duration::from_secs(1), stub.next_conn())
        .await
        .expect("the resync is immediate, not backed off");
    assert_eq!(conn.since(), (Some(0), None), "resync replays from 0");
    let fresh = event(1);
    conn.item(&SubscribeItem::Event {
        event: Box::new(one.clone()),
        persisted: true,
    })
    .await;
    conn.item(&SubscribeItem::Event {
        event: Box::new(two.clone()),
        persisted: true,
    })
    .await;
    conn.event(&fresh).await;

    let (token, ev) = next_event(&mut items).await;
    assert_eq!(ev.id, fresh.id, "duplicates were relayed again");
    assert_eq!(token, ResumeToken::of(&fresh));
    let status = relay.status();
    assert_eq!(
        (status.resyncs, status.duplicates, status.delivered),
        (1, 2, 3)
    );
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("since_id")),
        "the refusal is visible: {status:?}"
    );
}

/// An event this build cannot decode (a newer daemon's payload, DOC-73 §3.3)
/// is passed over as a one-event gap, and the cursor moves past it, so a
/// replay that carries it again does not stall the client (#9076 review).
#[tokio::test]
async fn an_undecodable_event_is_a_gap_and_the_client_moves_past_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let mut config = fast(&stub.socket);
    config.start = Start::Beginning;
    let (relay, _client) = run(config);
    let mut items = relay.subscribe();

    let (one, three) = (event(1), event(3));
    let future_id = EventId::new();
    let future = json!({
        "type": "event",
        "persisted": true,
        "event": {
            "source": "mpm", "seq": 2, "at": "2026-10-19T00:00:00Z",
            "payload": { "domain": "from_the_future" }, "id": future_id,
        },
    });
    let mut conn = stub.next_conn().await;
    conn.event(&one).await;
    conn.frame(RpcStreamFrame::item(json!(1), future.clone()))
        .await;
    // An item kind this build does not know is not an event: nothing is lost.
    conn.frame(RpcStreamFrame::item(
        json!(1),
        json!({ "type": "from_the_future" }),
    ))
    .await;
    conn.event(&three).await;

    assert_eq!(next_event(&mut items).await.0, ResumeToken::of(&one));
    match next_item(&mut items).await {
        RelayItem::Gap { skipped, after } => {
            assert_eq!((skipped, after), (1, Some(ResumeToken::of(&one))));
        }
        other => panic!("the undecodable event stalled or vanished: {other:?}"),
    }
    assert_eq!(next_event(&mut items).await.0, ResumeToken::of(&three));
    drop(conn);

    // The replay after a hang-up starts past both, not at the bad event.
    let conn = stub.next_conn().await;
    assert_eq!(conn.since(), (Some(3), Some(three.id)));
    let status = relay.status();
    assert_eq!((status.gaps, status.skipped), (1, 1), "{status:?}");
}

#[tokio::test]
async fn a_stream_error_that_is_not_a_refusal_backs_off_and_resumes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));

    let mut conn = stub.next_conn().await;
    let one = event(1);
    conn.event(&one).await;
    conn.error(RpcError::internal(
        "every subscriber slot is in use; retry later",
    ))
    .await;

    let status = status_until(&relay, "reconnecting", |s| {
        s.state == BusState::Reconnecting
    })
    .await;
    assert!(
        status.retry_in_ms.is_some_and(|ms| ms > 0),
        "it backs off: {status:?}"
    );
    assert_eq!(status.resyncs, 0, "only a refused resume resyncs");
    let conn = stub.next_conn().await;
    assert_eq!(conn.since(), (Some(1), Some(one.id)));
}

#[tokio::test]
async fn a_malformed_frame_is_reported_and_the_client_reconnects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));
    let mut items = relay.subscribe();

    let mut conn = stub.next_conn().await;
    let one = event(1);
    conn.event(&one).await;
    next_event(&mut items).await;
    // Not a frame at all: there is no seq to move past, so the client
    // reconnects from the last good event. An item that is a frame but holds
    // an event this build cannot read is the test above.
    conn.raw(b"this is not a stream frame\n").await;

    let status = status_until(&relay, "reconnecting", |s| {
        s.state == BusState::Reconnecting
    })
    .await;
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("malformed")),
        "the bad frame is named: {status:?}"
    );
    let conn = stub.next_conn().await;
    assert_eq!(
        conn.since(),
        (Some(1), Some(one.id)),
        "it resumes past the last good event"
    );
}

#[tokio::test]
async fn a_missing_socket_is_reported_and_retried_with_backoff() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-events.sock");
    let mut config = fast(&socket);
    config.backoff_initial = Duration::from_millis(100);
    config.backoff_max = Duration::from_millis(400);
    let (relay, _client) = run(config);

    let first = status_until(&relay, "a failure", |s| s.failures >= 1).await;
    assert_eq!(first.state, BusState::Reconnecting);
    assert!(
        first
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("trusty-events.sock") && e.contains("not running")),
        "the error names the absent socket: {first:?}"
    );
    let later = status_until(&relay, "four failures", |s| s.failures >= 4).await;
    let (a, b) = (
        first.retry_in_ms.unwrap_or(0),
        later.retry_in_ms.unwrap_or(0),
    );
    assert!(b > a, "the delay grows: {a} then {b}");
    assert!(b <= 400, "the delay is capped at backoff_max: {b}");

    let mut stub = bind_stub(&socket);
    stub.next_conn().await;
    let status = status_until(&relay, "connected", |s| s.state == BusState::Connected).await;
    assert_eq!(status.retry_in_ms, None);
}

#[tokio::test]
async fn a_socket_that_vanishes_under_a_quiet_subscription_is_detected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));

    let _conn = stub.next_conn().await;
    status_until(&relay, "connected", |s| s.state == BusState::Connected).await;
    std::fs::remove_file(&stub.socket).expect("remove the socket file");

    let status = status_until(&relay, "reconnecting", |s| {
        s.state == BusState::Reconnecting
    })
    .await;
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("no longer served")),
        "the lost socket is named: {status:?}"
    );
}

#[tokio::test]
async fn a_clean_end_of_stream_is_not_read_as_healthy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let (relay, _client) = run(fast(&stub.socket));

    let mut conn = stub.next_conn().await;
    conn.frame(RpcStreamFrame::end(json!(1))).await;
    let status = status_until(&relay, "reconnecting", |s| {
        s.state == BusState::Reconnecting
    })
    .await;
    assert!(status.last_error.is_some(), "{status:?}");
    stub.next_conn().await;
}

#[tokio::test]
async fn a_client_started_after_a_token_resumes_from_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut stub = stub_in(tmp.path());
    let held = event(7);
    let mut config = fast(&stub.socket);
    config.start = Start::After(ResumeToken::of(&held));
    let (_relay, _client) = run(config);
    let conn = stub.next_conn().await;
    assert_eq!(conn.since(), (Some(7), Some(held.id)));
}

#[tokio::test]
async fn an_unresolvable_socket_is_reported_unavailable() {
    let relay = Arc::new(EventsRelay::new());
    let spawned = super::start_with(Err(anyhow::anyhow!("no data dir")), Arc::clone(&relay));
    assert!(spawned.is_none());
    let status = relay.status();
    assert_eq!(status.state, BusState::Unavailable);
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("no data dir"))
    );
}

#[test]
fn backoff_doubles_and_stops_at_its_ceiling() {
    let (initial, max) = (Duration::from_millis(100), Duration::from_secs(2));
    let delays: Vec<_> = (1..=8).map(|n| client::backoff(n, initial, max)).collect();
    assert_eq!(delays[0], initial);
    assert_eq!(delays[1], Duration::from_millis(200));
    assert!(delays.windows(2).all(|w| w[1] >= w[0]));
    assert_eq!(*delays.last().expect("eight delays"), max);
    assert_eq!(client::backoff(u32::MAX, initial, max), max);
}
