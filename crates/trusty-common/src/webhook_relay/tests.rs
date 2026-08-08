//! Tests for the relay's receive half (#5182).
//!
//! The three that carry the change: `dispatch_acks_only_after_the_sink_has_taken_ownership`
//! (the ordering), `dispatch_refuses_when_the_sink_cannot_take_ownership` (the
//! failure that must not ack), and `dispatch_rejects_a_method_that_is_not_webhook_deliver`.
//! Everything else exists to keep those three honest.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::inbox::{INBOX_DIR_MODE, INBOX_FILE_MODE, Inbox};
use super::serve::{
    CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_NOT_DURABLE,
    CODE_PARSE_ERROR, DeliverySink, LISTENER_SHUTDOWN_FLUSH, ServeOptions, SinkRejection,
    dispatch_frame, serve_until,
};
use super::{
    ANALYZE_SOCKET_FILE, JSONRPC_VERSION, Provenance, RELAY_METHOD, REVIEW_SOCKET_FILE,
    RelayDelivery, RelayFrame, RelayResponse, analyze_socket_path, review_socket_path,
    socket_path_for,
};

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn provenance() -> Provenance {
    Provenance {
        algorithm: "hmac-sha256".to_string(),
        key_id: "GITHUB_WEBHOOK_SECRET".to_string(),
        verified: true,
    }
}

fn delivery(id: &str) -> RelayDelivery {
    RelayDelivery {
        delivery_id: id.to_string(),
        source: "review".to_string(),
        event: "pull_request".to_string(),
        headers: BTreeMap::from([("x-github-event".to_string(), "pull_request".to_string())]),
        body_b64: "eyJhY3Rpb24iOiJyZXZpZXdfcmVxdWVzdGVkIn0=".to_string(),
        provenance: provenance(),
        received_at_unix_ms: 1_700_000_000_000,
        attempts: 0,
    }
}

/// One well-formed `webhook.deliver` frame, as the sender writes it.
fn frame_bytes(delivery: &RelayDelivery) -> Vec<u8> {
    let frame = RelayFrame::new(
        &delivery.delivery_id,
        &delivery.source,
        &delivery.event,
        &delivery.headers,
        &delivery.body_b64,
        &delivery.provenance,
        delivery.received_at_unix_ms,
        delivery.attempts,
    );
    serde_json::to_vec(&frame).expect("serialize frame")
}

/// A sink that records what it was asked to own, and can be told to fail.
///
/// Why: the ack ordering is only assertable against a sink whose success is
/// under the test's control — a real `Inbox` succeeds, so it can never show
/// what happens when durability is unavailable.
struct StubSink {
    accept: bool,
    seen: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl StubSink {
    fn accepting() -> Arc<Self> {
        Arc::new(Self {
            accept: true,
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn refusing() -> Arc<Self> {
        Arc::new(Self {
            accept: false,
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("stub sink lock").clone()
    }
}

impl DeliverySink for StubSink {
    fn take_ownership(&self, delivery: &RelayDelivery) -> Result<(), SinkRejection> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("stub sink lock")
            .push(delivery.delivery_id.clone());
        if self.accept {
            Ok(())
        } else {
            Err(SinkRejection::not_durable("disk is full"))
        }
    }
}

// ─── The ack ordering ────────────────────────────────────────────────────────

#[test]
fn dispatch_acks_only_after_the_sink_has_taken_ownership() {
    // 🔴 The property the whole change exists for. An ack licenses the sender to
    // delete its spool entry — the only remaining copy, because GitHub does not
    // re-send an acknowledged delivery. So the ack must be downstream of durable
    // ownership, and the sink must have been consulted exactly once.
    let sink = StubSink::accepting();
    let d = delivery("ack-order-1");

    let response = dispatch_frame(&frame_bytes(&d), sink.as_ref());

    assert!(response.is_ack(), "a durably-owned delivery must be acked");
    assert_eq!(sink.calls(), 1, "the ack must go through the sink, once");
    assert_eq!(sink.seen(), vec!["ack-order-1".to_string()]);
}

#[test]
fn dispatch_refuses_when_the_sink_cannot_take_ownership() {
    // 🔴 The failure that must never ack. Against the pre-fix tree there is no
    // counterpart: no listener existed, so console's relay could only report
    // `Unreachable`. This pins the arm that replaces it — a receiver that CAN be
    // reached but cannot own the work refuses, and the sender keeps its copy.
    let sink = StubSink::refusing();
    let d = delivery("ack-order-2");

    let response = dispatch_frame(&frame_bytes(&d), sink.as_ref());

    assert!(
        !response.is_ack(),
        "a delivery that is not durably held must NOT be acked"
    );
    let err = response.error.expect("a refusal carries an error");
    assert_eq!(err.code, CODE_NOT_DURABLE);
    assert!(
        err.message.contains("disk is full"),
        "the sender's durable record must carry the target's own words, got {:?}",
        err.message
    );
    assert_eq!(sink.calls(), 1, "the sink is still consulted, and refuses");
}

#[test]
fn dispatch_rejects_a_method_that_is_not_webhook_deliver() {
    // The sink must never be reached: an unknown method is refused before
    // anything takes responsibility for the payload.
    let sink = StubSink::accepting();
    let mut frame: serde_json::Value =
        serde_json::from_slice(&frame_bytes(&delivery("method-1"))).expect("parse");
    frame["method"] = serde_json::Value::String("webhook.deliverr".to_string());

    let response = dispatch_frame(
        &serde_json::to_vec(&frame).expect("re-encode"),
        sink.as_ref(),
    );

    assert!(!response.is_ack());
    let err = response.error.expect("a refusal carries an error");
    assert_eq!(err.code, CODE_METHOD_NOT_FOUND);
    assert!(
        err.message.contains("webhook.deliverr") && err.message.contains(RELAY_METHOD),
        "the refusal must name both what arrived and what is served, got {:?}",
        err.message
    );
    assert_eq!(
        sink.calls(),
        0,
        "an unknown method must not reach the sink at all"
    );
}

#[test]
fn dispatch_rejects_an_unparseable_frame() {
    let sink = StubSink::accepting();
    let response = dispatch_frame(b"{not json", sink.as_ref());
    assert!(!response.is_ack());
    assert_eq!(
        response.error.expect("error").code,
        CODE_PARSE_ERROR,
        "garbage must be refused, not silently owned"
    );
    assert_eq!(sink.calls(), 0);
}

#[test]
fn dispatch_rejects_an_unsupported_jsonrpc_version() {
    let sink = StubSink::accepting();
    let mut frame: serde_json::Value =
        serde_json::from_slice(&frame_bytes(&delivery("ver-1"))).expect("parse");
    frame["jsonrpc"] = serde_json::Value::String("1.0".to_string());

    let response = dispatch_frame(
        &serde_json::to_vec(&frame).expect("re-encode"),
        sink.as_ref(),
    );

    assert!(!response.is_ack());
    assert_eq!(response.error.expect("error").code, CODE_INVALID_REQUEST);
    assert_eq!(sink.calls(), 0);
}

#[test]
fn dispatch_rejects_an_unverified_provenance() {
    // ADR-0034 §3: the sender refuses an unverified delivery before it reaches
    // the spool, so a frame claiming otherwise is a broken sender — never
    // something to take responsibility for.
    let sink = StubSink::accepting();
    let mut d = delivery("prov-1");
    d.provenance.verified = false;

    let response = dispatch_frame(&frame_bytes(&d), sink.as_ref());

    assert!(!response.is_ack());
    assert_eq!(response.error.expect("error").code, CODE_INVALID_PARAMS);
    assert_eq!(sink.calls(), 0);
}

// ─── The inbox ───────────────────────────────────────────────────────────────

#[test]
fn inbox_open_creates_an_owner_only_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("inbox");
    let inbox = Inbox::open(&root).expect("open inbox");

    let mode = std::fs::metadata(inbox.root())
        .expect("stat inbox")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, INBOX_DIR_MODE, "inbox must be {INBOX_DIR_MODE:04o}");
}

#[test]
fn inbox_persist_is_durable_before_it_returns() {
    // "Durable" is checked the only way a test can: the file exists, is
    // owner-only, and decodes back to the delivery byte-for-byte — all true the
    // instant `take_ownership` returns, which is the instant the ack is allowed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let d = delivery("durable-1");

    let owned = inbox.take_ownership(&d).expect("take ownership");

    assert!(!owned.already_owned);
    assert!(owned.path.is_file(), "the entry must be on disk");
    let mode = std::fs::metadata(&owned.path)
        .expect("stat entry")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, INBOX_FILE_MODE);

    let stored: RelayDelivery =
        serde_json::from_slice(&std::fs::read(&owned.path).expect("read entry")).expect("decode");
    assert_eq!(
        stored, d,
        "the stored copy must be the frame we were handed"
    );
}

#[test]
fn inbox_redelivery_of_a_held_id_is_already_owned() {
    // Relay is at-least-once: a lost ack means the same delivery arrives again.
    // Refusing it would wedge the sender's spool forever, so a delivery we
    // already hold is still an ack — and must not produce a second copy.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let mut d = delivery("redelivered-1");

    let first = inbox.take_ownership(&d).expect("first");
    d.attempts = 3;
    let second = inbox.take_ownership(&d).expect("second");

    assert!(!first.already_owned);
    assert!(second.already_owned, "a held id must report already_owned");
    assert_eq!(first.path, second.path);
    assert_eq!(inbox.list().expect("list").len(), 1, "no second copy");
}

#[test]
fn inbox_entry_path_sanitises_a_hostile_delivery_id() {
    // The `X-GitHub-Delivery` header is outside the HMAC, so it is
    // attacker-influenced input reaching `Path::join`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("inbox");
    let inbox = Inbox::open(&root).expect("open inbox");

    let path = inbox.entry_path("../../etc/passwd");
    assert_eq!(
        path.parent(),
        Some(root.as_path()),
        "a traversing id must not escape the inbox: {path:?}"
    );

    let long = "x".repeat(4096);
    let name = inbox.entry_path(&long);
    let base = name.file_name().expect("filename").to_string_lossy();
    assert!(
        base.len() < 128,
        "filename must stay short, got {}",
        base.len()
    );
}

#[test]
fn inbox_entry_path_separates_ids_that_sanitise_alike() {
    // Sanitising throws away distinctions; the hash of the RAW id restores them,
    // so two hostile ids cannot alias onto one file and lose a delivery.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    assert_ne!(inbox.entry_path("a/b"), inbox.entry_path("a:b"));
}

#[test]
fn inbox_persist_fails_when_the_root_is_not_a_directory() {
    // Every failure of the write must surface as an error, because an error is
    // what stops the ack.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    std::fs::remove_dir_all(inbox.root()).expect("remove inbox dir");
    std::fs::write(inbox.root(), b"not a directory").expect("write file at inbox path");

    let err = inbox
        .take_ownership(&delivery("no-dir-1"))
        .expect_err("a missing inbox must not report ownership");
    assert!(
        format!("{err}").contains("webhook inbox"),
        "error must name the inbox, got {err}"
    );
}

#[test]
fn inbox_lists_what_it_holds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let mut older = delivery("older");
    older.received_at_unix_ms = 1;
    let mut newer = delivery("newer");
    newer.received_at_unix_ms = 2;

    inbox.take_ownership(&newer).expect("newer");
    inbox.take_ownership(&older).expect("older");

    let held: Vec<String> = inbox
        .list()
        .expect("list")
        .into_iter()
        .map(|(_, d)| d.delivery_id)
        .collect();
    assert_eq!(held, vec!["older".to_string(), "newer".to_string()]);
}

/// The `Inbox` implementation of the sink is the production ack seam, so its
/// success and failure arms are asserted through the trait, not only directly.
#[test]
fn inbox_as_a_sink_refuses_when_it_cannot_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let d = delivery("sink-1");

    assert!(DeliverySink::take_ownership(&inbox, &d).is_ok());

    std::fs::remove_dir_all(inbox.root()).expect("remove inbox dir");
    let rejection = DeliverySink::take_ownership(&inbox, &d)
        .expect_err("a vanished inbox must refuse, not ack");
    assert_eq!(rejection.code, CODE_NOT_DURABLE);
}

// ─── Socket paths ────────────────────────────────────────────────────────────

#[test]
fn socket_paths_live_in_the_hardened_scratch_dir() {
    let dir = crate::uds::scratch_socket_dir();
    assert_eq!(review_socket_path(), dir.join(REVIEW_SOCKET_FILE));
    assert_eq!(analyze_socket_path(), dir.join(ANALYZE_SOCKET_FILE));
}

#[test]
fn socket_path_for_resolves_both_targets_and_refuses_others() {
    assert_eq!(socket_path_for("review"), Some(review_socket_path()));
    assert_eq!(socket_path_for("analyze"), Some(analyze_socket_path()));
    assert_eq!(socket_path_for("Review"), None);
    assert_eq!(socket_path_for("mpm"), None);
}

#[test]
fn serve_options_read_timeout_fits_the_declared_flush_budget() {
    // Console declares `LISTENER_SHUTDOWN_FLUSH` as this child's flush budget
    // and sets its SIGTERM patience above it. If a connection could outlast the
    // budget, the SIGKILL would land on an in-flight delivery.
    assert!(
        ServeOptions::default().read_timeout <= LISTENER_SHUTDOWN_FLUSH,
        "a connection must settle inside the declared flush budget"
    );
}

// ─── Over a real socket ──────────────────────────────────────────────────────

/// Bind a hardened listener under `dir` and serve it until `stop` fires.
fn spawn_listener(
    dir: &Path,
    sink: Arc<dyn DeliverySink>,
) -> (std::path::PathBuf, tokio::sync::oneshot::Sender<()>) {
    let sock = dir.join("sockets").join("recv.sock");
    let listener = crate::uds::bind_hardened(&sock).expect("bind listener");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, sink, ServeOptions::default(), async {
            let _ = stop_rx.await;
        })
        .await;
    });
    (sock, stop_tx)
}

#[tokio::test]
async fn serve_round_trips_a_delivery_over_a_real_socket() {
    // The end-to-end shape console drives: one framed request in, one ack out,
    // and the delivery on disk in the receiver's inbox by the time the ack
    // reaches the sender.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let (sock, _stop) = spawn_listener(tmp.path(), Arc::new(inbox.clone()));

    let d = delivery("e2e-1");
    let frame = RelayFrame::new(
        &d.delivery_id,
        &d.source,
        &d.event,
        &d.headers,
        &d.body_b64,
        &d.provenance,
        d.received_at_unix_ms,
        d.attempts,
    );
    let response: RelayResponse =
        crate::uds::send_framed_request(&sock, &frame, Duration::from_secs(5))
            .await
            .expect("round trip");

    assert!(response.is_ack(), "a durably-held delivery must be acked");
    let held = inbox.list().expect("list");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].1, d, "the ack must post-date the durable write");
}

#[tokio::test]
async fn serve_answers_a_wrong_method_over_a_real_socket_without_storing_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let (sock, _stop) = spawn_listener(tmp.path(), Arc::new(inbox.clone()));

    let request = serde_json::json!({
        "jsonrpc": JSONRPC_VERSION,
        "method": "webhook.drop",
        "id": "wrong-method-1",
        "params": delivery("wrong-method-1"),
    });
    let response: RelayResponse =
        crate::uds::send_framed_request(&sock, &request, Duration::from_secs(5))
            .await
            .expect("round trip");

    assert!(!response.is_ack());
    assert_eq!(response.error.expect("error").code, CODE_METHOD_NOT_FOUND);
    assert!(
        inbox.list().expect("list").is_empty(),
        "a refused method must leave nothing behind"
    );
}

#[tokio::test]
async fn serve_refuses_over_a_real_socket_when_the_sink_cannot_own_the_work() {
    // The wire-level form of the ack-ordering guarantee: console's own
    // `RelayResponse::is_ack` reads this as NOT an acknowledgement, so its spool
    // entry survives.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (sock, _stop) = spawn_listener(tmp.path(), StubSink::refusing());

    let d = delivery("e2e-refuse-1");
    let frame = RelayFrame::new(
        &d.delivery_id,
        &d.source,
        &d.event,
        &d.headers,
        &d.body_b64,
        &d.provenance,
        d.received_at_unix_ms,
        d.attempts,
    );
    let response: RelayResponse =
        crate::uds::send_framed_request(&sock, &frame, Duration::from_secs(5))
            .await
            .expect("round trip");

    assert!(
        !response.is_ack(),
        "an undurable delivery must not read as acknowledged"
    );
}

#[tokio::test]
async fn serve_keeps_answering_after_a_refusal() {
    // A malformed frame must not wedge the listener — the next delivery, from a
    // fresh connection, still gets served.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let (sock, _stop) = spawn_listener(tmp.path(), Arc::new(inbox.clone()));

    let bad: RelayResponse = crate::uds::send_framed_request(
        &sock,
        &serde_json::json!({"junk": true}),
        Duration::from_secs(5),
    )
    .await
    .expect("round trip");
    assert!(!bad.is_ack());

    let d = delivery("after-refusal");
    let frame = RelayFrame::new(
        &d.delivery_id,
        &d.source,
        &d.event,
        &d.headers,
        &d.body_b64,
        &d.provenance,
        d.received_at_unix_ms,
        d.attempts,
    );
    let good: RelayResponse =
        crate::uds::send_framed_request(&sock, &frame, Duration::from_secs(5))
            .await
            .expect("round trip");
    assert!(good.is_ack());
    assert_eq!(inbox.list().expect("list").len(), 1);
}

#[tokio::test]
async fn serve_stops_on_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let (sock, stop) = spawn_listener(tmp.path(), Arc::new(inbox));

    stop.send(()).expect("signal shutdown");
    // The loop is `biased` on the shutdown branch, so one scheduler turn is
    // enough for it to observe the signal; poll until the dial fails rather
    // than sleeping a fixed interval.
    let mut refused = false;
    for _ in 0..200 {
        tokio::task::yield_now().await;
        let d = delivery("post-shutdown");
        let frame = RelayFrame::new(
            &d.delivery_id,
            &d.source,
            &d.event,
            &d.headers,
            &d.body_b64,
            &d.provenance,
            d.received_at_unix_ms,
            d.attempts,
        );
        let sent: Result<RelayResponse, _> =
            crate::uds::send_framed_request(&sock, &frame, Duration::from_millis(200)).await;
        if sent.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(refused, "a stopped listener must stop answering");
}

#[tokio::test]
async fn serve_concurrent_deliveries_of_one_id_produce_one_stored_copy() {
    // Two relays racing on the same delivery id — the sender's retry sweep
    // landing on top of a request-path relay — must not double-store, and both
    // must be acked so neither leaves a stranded spool entry.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let (sock, _stop) = spawn_listener(tmp.path(), Arc::new(inbox.clone()));

    let d = delivery("concurrent-1");
    let mut sends = Vec::new();
    for _ in 0..8 {
        let sock = sock.clone();
        let d = d.clone();
        sends.push(tokio::spawn(async move {
            let frame = RelayFrame::new(
                &d.delivery_id,
                &d.source,
                &d.event,
                &d.headers,
                &d.body_b64,
                &d.provenance,
                d.received_at_unix_ms,
                d.attempts,
            );
            let resp: RelayResponse =
                crate::uds::send_framed_request(&sock, &frame, Duration::from_secs(5))
                    .await
                    .expect("round trip");
            resp.is_ack()
        }));
    }
    for send in sends {
        assert!(send.await.expect("join"), "every attempt must be acked");
    }
    assert_eq!(
        inbox.list().expect("list").len(),
        1,
        "a repeated delivery id must yield exactly one stored copy"
    );
}

// ─── The listener a supervised child runs ────────────────────────────────────

#[test]
fn listener_open_creates_the_inbox_before_binding() {
    // A misconfigured data directory must fail before the socket exists, so
    // console's spawn probe reports a failed start rather than a live process
    // that will refuse every delivery.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("open.sock");
    let listener = super::listener::WebhookListener::open(&sock, tmp.path().join("inbox"))
        .expect("open listener");

    assert!(listener.inbox().root().is_dir(), "inbox must exist");
    assert!(!sock.exists(), "open must not bind anything");
}

#[tokio::test]
async fn listener_serves_a_delivery_and_cleans_up_its_socket() {
    // The whole supervised-child lifecycle: bind, take durable ownership, ack,
    // exit on shutdown, and leave no socket file behind for the next spawn.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("child.sock");
    let inbox_root = tmp.path().join("inbox");
    let listener = super::listener::WebhookListener::open(&sock, &inbox_root).expect("open");
    let inbox = listener.inbox().clone();

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(async move {
        listener
            .run(async {
                let _ = stop_rx.await;
            })
            .await
    });

    // Poll for the bind rather than sleeping a fixed interval.
    let mut response: Option<RelayResponse> = None;
    let d = delivery("listener-1");
    for _ in 0..200 {
        let frame = RelayFrame::new(
            &d.delivery_id,
            &d.source,
            &d.event,
            &d.headers,
            &d.body_b64,
            &d.provenance,
            d.received_at_unix_ms,
            d.attempts,
        );
        if let Ok(resp) = crate::uds::send_framed_request::<_, RelayResponse>(
            &sock,
            &frame,
            Duration::from_secs(5),
        )
        .await
        {
            response = Some(resp);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        response.expect("the listener must answer").is_ack(),
        "a durably-held delivery must be acked"
    );
    assert_eq!(inbox.list().expect("list").len(), 1);

    stop_tx.send(()).expect("signal shutdown");
    running.await.expect("join").expect("clean exit");
    assert!(
        !sock.exists(),
        "the socket file must be unlinked so the next spawn binds cleanly"
    );
}

#[tokio::test]
async fn listener_refuses_to_take_over_a_live_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("busy.sock");
    let _live = crate::uds::bind_hardened(&sock).expect("bind the live owner");

    let err = super::listener::WebhookListener::open(&sock, tmp.path().join("inbox"))
        .expect("open")
        .run(std::future::pending::<()>())
        .await
        .expect_err("a second listener must not steal a served socket");

    assert!(
        format!("{err}").contains("already serving"),
        "expected an already-serving refusal, got {err}"
    );
}

#[tokio::test]
async fn serve_rejects_an_oversized_frame() {
    // A peer that never sends a newline would otherwise grow the read buffer
    // until the process dies. The connection is dropped without an answer, which
    // the sender classifies as `Unreachable` — pending and durable, never acked.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("flood.sock");
    let listener = crate::uds::bind_hardened(&sock).expect("bind");
    let inbox = Inbox::open(tmp.path().join("inbox")).expect("open inbox");
    let sink: Arc<dyn DeliverySink> = Arc::new(inbox.clone());
    let options = ServeOptions {
        max_frame_bytes: 64,
        ..ServeOptions::default()
    };
    tokio::spawn(async move {
        serve_until(&listener, sink, options, std::future::pending::<()>()).await;
    });

    let flood = vec![b'x'; 4096];
    let err = crate::uds::send_framed_request::<_, RelayResponse>(
        &sock,
        &String::from_utf8(flood).expect("ascii"),
        Duration::from_secs(5),
    )
    .await
    .expect_err("an over-long frame must not be answered");

    assert!(
        matches!(err, crate::uds::UdsRpcError::NoResponse { .. }),
        "expected NoResponse, got {err:?}"
    );
    assert!(
        inbox.list().expect("list").is_empty(),
        "an over-long frame must leave nothing durably owned"
    );
}
