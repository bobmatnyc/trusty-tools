//! The `trusty-events` daemon on its one socket: producer ingest and
//! `events.subscribe`, end to end over the real binary (#9075, ADR-0065).

mod common;

use std::collections::HashSet;

use common::{
    event, large_event, next_event, next_frame, next_full_event, next_item, open, push, start,
    subscribe,
};
use trusty_common::control_bus::{
    HarnessEvent, HarnessPayload, HarnessSource, MessageChannel, MessageEvent, SubscribeFilter,
    SubscribeItem, SubscribeParams,
};
use trusty_common::uds::UdsRpcError;
use trusty_common::uds::server::CODE_INVALID_PARAMS;

fn session(s: &str) -> SubscribeFilter {
    SubscribeFilter {
        session: Some(s.to_string()),
        ..SubscribeFilter::default()
    }
}

/// A pushed event reaches a subscriber that asks for it by `since_seq` and a
/// session filter, replayed first and then live, on the producers' socket.
#[tokio::test(flavor = "multi_thread")]
async fn a_pushed_event_reaches_a_filtered_subscriber_from_since_seq() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &[]).await;

    push(&daemon.socket, vec![event("s1"), event("s2"), event("s1")]).await;

    let mut stream = subscribe(&daemon.socket, Some(0), session("s1")).await;
    let (seq, sess, _) = next_event(&mut stream).await;
    assert_eq!(
        (seq, sess.as_str()),
        (1, "s1"),
        "replay starts after since_seq"
    );
    let (seq, sess, _) = next_event(&mut stream).await;
    assert_eq!(
        (seq, sess.as_str()),
        (3, "s1"),
        "seq 2 belongs to s2 and is filtered out, without a gap"
    );

    // Live: the subscription stays open while a producer pushes on the same
    // socket.
    push(&daemon.socket, vec![event("s2"), event("s1")]).await;
    let (seq, sess, persisted) = next_event(&mut stream).await;
    assert_eq!((seq, sess.as_str()), (5, "s1"));
    assert!(!persisted, "a live event is not marked persisted");
}

/// Several producers and several subscribers on the one socket at once: every
/// subscriber sees every event, once, in `seq` order.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_and_subscribe_share_one_socket_concurrently() {
    const PRODUCERS: usize = 4;
    const PER_PRODUCER: usize = 50;
    const TOTAL: u64 = (PRODUCERS * PER_PRODUCER) as u64;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &[]).await;

    let mut readers = Vec::new();
    for _ in 0..3 {
        // Every subscription opens at the same head, seq 0, before any push.
        readers.push(subscribe(&daemon.socket, Some(0), SubscribeFilter::default()).await);
    }

    let mut producers = Vec::new();
    for p in 0..PRODUCERS {
        let socket = daemon.socket.clone();
        producers.push(tokio::spawn(async move {
            for _ in 0..PER_PRODUCER {
                push(&socket, vec![event(&format!("p{p}"))]).await;
            }
        }));
    }

    let mut checks = Vec::new();
    for mut stream in readers {
        checks.push(tokio::spawn(async move {
            let mut expected = 1;
            while expected <= TOTAL {
                let (seq, _, _) = next_event(&mut stream).await;
                assert_eq!(seq, expected, "every event once, in seq order, no gap");
                expected += 1;
            }
        }));
    }
    for producer in producers {
        producer.await.expect("producer task");
    }
    for check in checks {
        check.await.expect("subscriber task");
    }
}

/// A subscriber that stops reading falls behind the ring. It is told so with a
/// `Gap`, and every pushed event is either delivered or counted in a gap.
#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_that_falls_behind_the_ring_gets_a_gap() {
    // 32 KiB events fill the kernel socket buffers (about 1 MiB a side) in
    // tens of events, so 400 overrun a 4-event ring many times over.
    const EVENTS: u64 = 400;
    const BATCH: usize = 100;
    const BYTES: usize = 32 * 1024;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &["--ring-capacity", "4"]).await;
    let mut stream = subscribe(&daemon.socket, Some(0), SubscribeFilter::default()).await;
    // A marker read back proves the subscription is attached and live before
    // the reader stalls; the bulk events then carry seqs 2..=EVENTS + 1.
    push(&daemon.socket, vec![event("marker")]).await;
    let (marker, _, _) = next_event(&mut stream).await;
    assert_eq!(marker, 1);

    for _ in 0..(EVENTS as usize / BATCH) {
        push(
            &daemon.socket,
            (0..BATCH).map(|_| large_event("bulk", BYTES)).collect(),
        )
        .await;
    }

    let mut delivered = 0u64;
    let mut skipped = 0u64;
    let mut gaps = 0u64;
    // The seq the next event must carry: one past the last event, moved on by
    // every gap reported since it.
    let mut next = 2u64;
    let mut last = 1u64;
    while last < EVENTS + 1 {
        match next_item(&mut stream).await {
            SubscribeItem::Event { event, .. } => {
                assert_eq!(
                    event.seq, next,
                    "an event follows the previous one or the gap that ends just before it"
                );
                last = event.seq;
                next = last + 1;
                delivered += 1;
            }
            SubscribeItem::Gap { skipped: n } => {
                assert!(n > 0, "a gap names at least one event");
                skipped += n;
                next += n;
                gaps += 1;
            }
            other => panic!("unexpected item {other:?}"),
        }
    }
    assert!(
        gaps > 0,
        "a 4-event ring cannot hold a stalled reader's backlog"
    );
    assert_eq!(
        delivered + skipped,
        EVENTS,
        "every event is delivered or counted in a gap, none silently dropped"
    );
}

/// `seq` survives a restart: the log is read back and numbering continues, so
/// a replay across the restart has no repeat and no hole.
#[tokio::test(flavor = "multi_thread")]
async fn seq_continues_after_a_restart() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut first = start(tmp.path(), &[]).await;
    push(&first.socket, vec![event("a"), event("a"), event("a")]).await;
    // Read all three back first, so SIGTERM cannot arrive before they are
    // ingested.
    let mut seen = subscribe(&first.socket, Some(0), SubscribeFilter::default()).await;
    for expected in 1..=3u64 {
        assert_eq!(next_event(&mut seen).await.0, expected);
    }
    drop(seen);
    let status = first.terminate().await;
    assert!(status.success(), "SIGTERM is a clean exit, got {status}");
    drop(first);

    let second = start(tmp.path(), &[]).await;
    push(&second.socket, vec![event("b")]).await;

    let mut stream = subscribe(&second.socket, Some(0), SubscribeFilter::default()).await;
    let mut seen = HashSet::new();
    for expected in 1..=4u64 {
        let (seq, sess, _) = next_event(&mut stream).await;
        assert_eq!(seq, expected, "no repeat and no hole across the restart");
        assert!(seen.insert(seq));
        let want = if expected <= 3 { "a" } else { "b" };
        assert_eq!(sess, want);
    }
}

// ─── #10071: opt-in heartbeats over the real socket ──────────────────────────

/// Two live subscriptions on one daemon: the one that set `heartbeat_ms`
/// gets a heartbeat at once and then on its interval; the one that did not
/// gets exactly the events, as before #10071.
#[tokio::test(flavor = "multi_thread")]
async fn heartbeats_reach_only_the_subscription_that_asked() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &[]).await;
    let asked = SubscribeParams {
        heartbeat_ms: Some(100),
        ..SubscribeParams::default()
    };
    let mut beating = open(&daemon.socket, asked).await;
    assert_eq!(
        next_item(&mut beating).await,
        SubscribeItem::Heartbeat { head: 0 }
    );
    let mut plain = subscribe(&daemon.socket, None, SubscribeFilter::default()).await;

    // Several intervals pass with nothing to say.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    push(&daemon.socket, vec![event("s1")]).await;
    let (seq, ..) = next_event(&mut plain).await;
    assert_eq!(seq, 1, "the first frame without heartbeat_ms is the event");

    let mut beats = 0;
    loop {
        match next_item(&mut beating).await {
            SubscribeItem::Heartbeat { head } => {
                // A beat due just after the push may already carry seq 1.
                assert!(head <= 1, "a heartbeat before the event: head {head}");
                beats += 1;
            }
            SubscribeItem::Event { event, .. } => {
                assert_eq!(event.seq, 1);
                break;
            }
            other => panic!("unexpected item {other:?}"),
        }
    }
    assert!(
        beats >= 2,
        "a heartbeat per interval, got {beats} in 500 ms"
    );
    assert_eq!(
        next_item(&mut beating).await,
        SubscribeItem::Heartbeat { head: 1 }
    );
}

/// The daemon refuses an out-of-range `heartbeat_ms` with `invalid_params`.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_refuses_a_zero_heartbeat_ms() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &[]).await;
    let zero = SubscribeParams {
        heartbeat_ms: Some(0),
        ..SubscribeParams::default()
    };
    let mut stream = open(&daemon.socket, zero).await;
    match next_frame(&mut stream).await {
        Some(Err(UdsRpcError::Stream { error, .. })) => {
            assert_eq!(error.code, CODE_INVALID_PARAMS, "{error}");
            assert!(error.message.contains("heartbeat_ms"), "{error}");
        }
        other => panic!("expected an invalid_params refusal, got {other:?}"),
    }
}

/// A `message` event (#9167) crosses the daemon intact: ingested on the
/// producers' socket, kept, and delivered to a subscriber that asks for the
/// `message` domain, with every field as the producer wrote it.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_event_round_trips_through_the_daemon_9167() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), &[]).await;
    let message = MessageEvent {
        at: chrono::Utc::now(),
        channel: MessageChannel::Tmux,
        from: "s1".to_string(),
        to: "=dev-pm:".to_string(),
        relay: Some("[Ops Architect 08:0xZ]".to_string()),
        bytes: 31,
        sha256: "0f".repeat(32),
        body: None,
    };
    let sent = HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some("s1".to_string()),
        seq: 0,
        at: message.at,
        payload: HarnessPayload::Message(message),
        id: Default::default(),
        parent_id: Some(Default::default()),
    };
    push(&daemon.socket, vec![event("s1"), sent.clone()]).await;

    let only_messages = SubscribeFilter {
        domains: Some(vec!["message".to_string()]),
        ..SubscribeFilter::default()
    };
    let mut stream = subscribe(&daemon.socket, Some(0), only_messages).await;
    let got = next_full_event(&mut stream).await;
    assert_eq!(got.seq, 2, "the ping at seq 1 is filtered out");
    assert_eq!(got.payload, sent.payload);
    assert_eq!((got.id, got.parent_id), (sent.id, sent.parent_id));
}
