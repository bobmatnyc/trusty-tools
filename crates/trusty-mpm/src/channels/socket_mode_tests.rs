//! Tests for the Socket Mode inbound source (#9923, #8433 P6).
//!
//! Why: the source is a Fail-Open Check site. Each test goes red against a
//! loop that acks before it claims, acts on a retried event, keeps its claim
//! only in memory, acts or acks after a claim failure, holds a socket while
//! not ready, or closes a question when the socket drops.
//! What: a recording env over a real claim log under a hermetic temp dir, a
//! scripted connection and transport, the real `decide` over the inbound
//! fake for the effect, and the real WebSocket client against a loopback
//! server.
//! Test: this is the test module.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use tokio_util::sync::CancellationToken;

use super::envelope::{EventEnvelope, Frame, parse_frame};
use super::transport::{SocketConn, SocketTransport, fake_url, fake_url_path, socket_url};
use super::{SocketEnv, Timing, handle_event, run};
use crate::channels::inbound::tests::{Fake, OWNER, owner_conv};
use crate::channels::inbound::{InboundEvent, Verdict, decide};
use crate::channels::question_table::claims::{
    Claim, ClaimError, ClaimFault, ClaimStore, RETENTION, claims_path,
};
use crate::channels::question_table::{lock_path, torn_path};

/// The order of every step, shared by the env and the connection.
type Log = Arc<Mutex<Vec<String>>>;

fn entries(log: &Log) -> Vec<String> {
    log.lock().expect("log").clone()
}

/// How a [`TestEnv`] opens its claim store.
type Opener = Box<dyn Fn(&Path) -> Result<ClaimStore, ClaimError> + Send + Sync>;

/// A check a fake runs at one of its steps.
type Hook<T> = Option<Box<dyn Fn(T) + Send + Sync>>;

/// A recording env over a real claim log at `path`.
struct TestEnv {
    path: PathBuf,
    opener: Opener,
    slot: Mutex<Option<Arc<ClaimStore>>>,
    policy_ready: Mutex<Result<(), String>>,
    log: Log,
    /// The effect: the real `decide` over this inbound fake, when set.
    inbound: Option<Arc<Fake>>,
    /// Called inside `act`, before the effect, with the claim log's text.
    on_act: Hook<String>,
}

impl TestEnv {
    fn new(path: &Path, log: &Log) -> Self {
        Self {
            path: path.to_path_buf(),
            opener: Box::new(ClaimStore::open_at),
            slot: Mutex::new(None),
            policy_ready: Mutex::new(Ok(())),
            log: Arc::clone(log),
            inbound: None,
            on_act: None,
        }
    }

    /// The held store, opened through the opener when none is held.
    fn store(&self) -> Result<Arc<ClaimStore>, String> {
        let mut slot = self.slot.lock().expect("slot");
        if let Some(store) = slot.as_ref().filter(|s| !s.is_poisoned()) {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new((self.opener)(&self.path).map_err(|e| e.to_string())?);
        *slot = Some(Arc::clone(&store));
        Ok(store)
    }

    fn acts(&self) -> usize {
        entries(&self.log).iter().filter(|e| *e == "act").count()
    }
}

#[async_trait::async_trait]
impl SocketEnv for TestEnv {
    async fn ready(&self) -> Result<(), String> {
        self.policy_ready.lock().expect("ready").clone()?;
        self.store().map(drop)
    }

    async fn claim(&self, event_id: &str) -> Result<Claim, String> {
        let claim = self.store()?.claim(event_id).map_err(|e| e.to_string())?;
        self.log
            .lock()
            .expect("log")
            .push(format!("claim:{event_id}"));
        Ok(claim)
    }

    async fn settle(&self, event_id: &str, outcome: &str) -> Result<(), String> {
        self.store()?
            .settle(event_id, outcome)
            .map_err(|e| e.to_string())?;
        self.log
            .lock()
            .expect("log")
            .push(format!("settle:{event_id}"));
        Ok(())
    }

    async fn act(&self, ev: InboundEvent) -> String {
        if let Some(check) = &self.on_act {
            check(std::fs::read_to_string(&self.path).unwrap_or_default());
        }
        self.log.lock().expect("log").push("act".to_owned());
        match &self.inbound {
            Some(fake) => match decide(&**fake, &ev) {
                Verdict::Resolved(_) => "resolved".to_owned(),
                Verdict::Dropped(d) => d.code.to_owned(),
            },
            None => "acted".to_owned(),
        }
    }
}

/// A scripted connection: frames in order, then closed.
struct FakeConn {
    frames: VecDeque<Option<Result<String, String>>>,
    log: Log,
    ack_fails: bool,
}

impl FakeConn {
    fn new(log: &Log, frames: impl IntoIterator<Item = Option<Result<String, String>>>) -> Self {
        Self {
            frames: frames.into_iter().collect(),
            log: Arc::clone(log),
            ack_fails: false,
        }
    }

    fn acks(&self) -> usize {
        entries(&self.log)
            .iter()
            .filter(|e| e.starts_with("ack:"))
            .count()
    }
}

#[async_trait::async_trait]
impl SocketConn for FakeConn {
    async fn recv(&mut self) -> Option<Result<String, String>> {
        self.frames.pop_front().flatten()
    }

    async fn ack(&mut self, envelope_id: &str) -> Result<(), String> {
        self.log
            .lock()
            .expect("log")
            .push(format!("ack:{envelope_id}"));
        if self.ack_fails {
            return Err("ack write failed".into());
        }
        Ok(())
    }
}

/// A transport handing out scripted connections, then refusing.
struct FakeTransport {
    conns: Mutex<VecDeque<FakeConn>>,
    connects: Mutex<Vec<tokio::time::Instant>>,
    /// Called at each connect with its 0-based index.
    on_connect: Hook<usize>,
}

impl FakeTransport {
    fn new(conns: impl IntoIterator<Item = FakeConn>) -> Self {
        Self {
            conns: Mutex::new(conns.into_iter().collect()),
            connects: Mutex::new(Vec::new()),
            on_connect: None,
        }
    }

    fn connects(&self) -> Vec<tokio::time::Instant> {
        self.connects.lock().expect("connects").clone()
    }
}

#[async_trait::async_trait]
impl SocketTransport for FakeTransport {
    async fn connect(&self) -> Result<Box<dyn SocketConn>, String> {
        let n = {
            let mut connects = self.connects.lock().expect("connects");
            connects.push(tokio::time::Instant::now());
            connects.len() - 1
        };
        if let Some(check) = &self.on_connect {
            check(n);
        }
        match self.conns.lock().expect("conns").pop_front() {
            Some(conn) => Ok(Box::new(conn)),
            None => Err("connect refused".into()),
        }
    }
}

/// One `events_api` frame from the owner's DM thread.
fn event_frame(envelope: &str, event_id: &str, retry: u64, text: &str) -> String {
    serde_json::json!({
        "envelope_id": envelope,
        "type": "events_api",
        "retry_attempt": retry,
        "retry_reason": if retry > 0 { "timeout" } else { "" },
        "payload": {
            "event_id": event_id,
            "event": {
                "type": "message",
                "channel": owner_conv().dm_channel_id,
                "channel_type": "im",
                "user": OWNER,
                "thread_ts": owner_conv().thread_ts,
                "ts": "1700000001.000200",
                "text": text,
            },
        },
    })
    .to_string()
}

fn envelope(raw: &str) -> EventEnvelope {
    match parse_frame(raw) {
        Frame::Event(event) => event,
        other => panic!("not an event: {other:?}"),
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    path: PathBuf,
    log: Log,
}

fn fixture() -> Fixture {
    let tmp = crate::test_support::hermetic_temp_dir();
    let path = tmp.path().join("channels").join("inbound_claims.jsonl");
    Fixture {
        _tmp: tmp,
        path,
        log: Log::default(),
    }
}

/// 🔴 #9923 (plan item 3): claim, then ack, then the effect, then settle.
#[tokio::test]
async fn claim_precedes_ack_in_event_order() {
    let fx = fixture();
    let env = TestEnv::new(&fx.path, &fx.log);
    let mut conn = FakeConn::new(&fx.log, []);
    handle_event(
        &env,
        &mut conn,
        envelope(&event_frame("E1", "Ev1", 0, "yes")),
    )
    .await;
    assert_eq!(
        entries(&fx.log),
        ["claim:Ev1", "ack:E1", "act", "settle:Ev1"]
    );
}

/// 🔴 #9923 (plan item 3): at the effect, the claim is already on disk.
#[tokio::test]
async fn event_id_claimed_by_daemon_before_any_effect() {
    let fx = fixture();
    let seen = Arc::new(Mutex::new(None));
    let mut env = TestEnv::new(&fx.path, &fx.log);
    let sink = Arc::clone(&seen);
    env.on_act = Some(Box::new(move |log| {
        *sink.lock().expect("seen") = Some(log.to_owned());
    }));
    let mut conn = FakeConn::new(&fx.log, []);
    handle_event(
        &env,
        &mut conn,
        envelope(&event_frame("E1", "Ev1", 0, "yes")),
    )
    .await;
    let at_act = seen.lock().expect("seen").clone().expect("the effect ran");
    assert!(
        at_act.contains("\"kind\":\"claimed\"") && at_act.contains("\"event_id\":\"Ev1\""),
        "the claim was not durable before the effect: {at_act:?}"
    );
}

/// 🔴 #9923 (plan item 4): a retry of a claimed id (same `event_id`, a new
/// envelope and `retry_attempt`) is acked and does nothing.
#[tokio::test]
async fn retried_envelope_is_acked_and_not_redelivered() {
    let fx = fixture();
    let mut env = TestEnv::new(&fx.path, &fx.log);
    let inbound = Arc::new(Fake::new());
    let question = inbound.open("owner-0", OWNER, &owner_conv());
    env.inbound = Some(Arc::clone(&inbound));
    let mut conn = FakeConn::new(&fx.log, []);
    for (envelope_id, retry) in [("E1", 0), ("E2", 1), ("E1", 0)] {
        let raw = event_frame(envelope_id, "Ev1", retry, "approve");
        handle_event(&env, &mut conn, envelope(&raw)).await;
    }
    assert_eq!(conn.acks(), 3, "every delivery is acked");
    assert_eq!(env.acts(), 1, "one effect");
    assert_eq!(inbound.resolves(), 1, "one resolve");
    assert!(
        inbound.table.answer(question).is_some(),
        "the first one resolved"
    );
}

/// 🔴 #9923 (plan item 5): the claim log is reopened from disk by a new
/// daemon, and the retry it gets is acked and not redelivered.
#[tokio::test]
async fn claim_survives_daemon_restart_so_a_retry_after_restart_is_not_redelivered() {
    let fx = fixture();
    let first = TestEnv::new(&fx.path, &fx.log);
    let mut conn = FakeConn::new(&fx.log, []);
    handle_event(
        &first,
        &mut conn,
        envelope(&event_frame("E1", "Ev1", 0, "yes")),
    )
    .await;
    assert_eq!(first.acts(), 1);
    drop(first);
    let log = Log::default();
    let restarted = TestEnv::new(&fx.path, &log);
    let mut conn = FakeConn::new(&log, []);
    handle_event(
        &restarted,
        &mut conn,
        envelope(&event_frame("E2", "Ev1", 1, "yes")),
    )
    .await;
    assert_eq!(
        entries(&log),
        ["claim:Ev1", "ack:E2"],
        "acked, nothing done"
    );
}

/// One fail-open arm: the event through `handle_event`; no ack, no effect.
async fn assert_no_ack_no_act(env: &TestEnv, log: &Log, what: &str) {
    let mut conn = FakeConn::new(log, []);
    handle_event(
        env,
        &mut conn,
        envelope(&event_frame("E1", "Ev1", 0, "yes")),
    )
    .await;
    assert_eq!(conn.acks(), 0, "{what}: acked");
    assert_eq!(env.acts(), 0, "{what}: acted");
}

/// 🔴 #9923 (plan item 6, Fail-Open Check): an append failure acks nothing
/// and acts on nothing.
#[tokio::test]
async fn claim_store_write_failure_acks_nothing_and_acts_on_nothing() {
    let fx = fixture();
    let mut env = TestEnv::new(&fx.path, &fx.log);
    env.opener = Box::new(|p| ClaimStore::open_with_fault(p, Utc::now(), ClaimFault::Append));
    assert_no_ack_no_act(&env, &fx.log, "append failure").await;
}

/// 🔴 #9923 (Fail-Open Check): the lock held by another store.
#[tokio::test]
async fn claim_lock_failure_acks_nothing_and_acts_on_nothing() {
    let fx = fixture();
    let _holder = ClaimStore::open_at(&fx.path).expect("first store");
    let env = TestEnv::new(&fx.path, &fx.log);
    assert_no_ack_no_act(&env, &fx.log, "lock held").await;
}

/// 🔴 #9923 (Fail-Open Check): a corrupt line, and a torn line whose
/// `.torn` file is unsealed, each fail the replay.
#[tokio::test]
async fn corrupt_claim_replay_acks_nothing_and_acts_on_nothing() {
    for (body, plant_torn) in [("not json\n", false), ("{\"v\":1,\"kind\"", true)] {
        let fx = fixture();
        std::fs::create_dir_all(fx.path.parent().expect("dir")).expect("dir");
        std::fs::write(&fx.path, body).expect("log");
        std::fs::set_permissions(&fx.path, std::fs::Permissions::from_mode(0o600)).expect("0600");
        if plant_torn {
            std::os::unix::fs::symlink("/etc/hosts", torn_path(&fx.path)).expect("plant");
        }
        let env = TestEnv::new(&fx.path, &fx.log);
        assert_no_ack_no_act(&env, &fx.log, body).await;
    }
}

/// 🔴 #9923 (Fail-Open Check): an unsealed log or lock fails the seal check.
#[tokio::test]
async fn unsealed_claim_log_acks_nothing_and_acts_on_nothing() {
    for lock in [false, true] {
        let fx = fixture();
        drop(ClaimStore::open_at(&fx.path).expect("create"));
        let target = if lock {
            lock_path(&fx.path)
        } else {
            fx.path.clone()
        };
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let env = TestEnv::new(&fx.path, &fx.log);
        assert_no_ack_no_act(&env, &fx.log, &target.display().to_string()).await;
    }
}

/// 🔴 #9923 Q5 (Fail-Open Check): a prune failure at startup acks and acts
/// on nothing, opens no socket, and deletes no unexpired claim.
#[tokio::test]
async fn prune_failure_at_startup_acks_nothing_and_acts_on_nothing() {
    let fx = fixture();
    {
        let store = ClaimStore::open_at(&fx.path).expect("open");
        store.claim("EvYoung").expect("claim");
    }
    let old = Utc::now() - RETENTION - TimeDelta::hours(1);
    let stale = format!(
        "{{\"v\":1,\"kind\":\"claimed\",\"event_id\":\"EvOld\",\"at\":\"{}\"}}\n",
        old.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    let mut text = std::fs::read_to_string(&fx.path).expect("log");
    text.push_str(&stale);
    std::fs::write(&fx.path, &text).expect("log");
    let mut env = TestEnv::new(&fx.path, &fx.log);
    env.opener = Box::new(|p| ClaimStore::open_with_fault(p, Utc::now(), ClaimFault::Prune));
    assert!(env.ready().await.is_err(), "a failed prune is not ready");
    assert_no_ack_no_act(&env, &fx.log, "prune failure").await;
    let transport = Arc::new(FakeTransport::new([]));
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        Arc::new(env),
        Arc::clone(&transport) as Arc<dyn SocketTransport>,
        cancel.clone(),
        fast(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    task.await.expect("run ends");
    assert!(transport.connects().is_empty(), "no socket while not ready");
    assert_eq!(
        std::fs::read_to_string(&fx.path).expect("log"),
        text,
        "untouched"
    );
    let store = ClaimStore::open_at(&fx.path).expect("open");
    assert_eq!(store.claim("EvYoung").expect("claim"), Claim::Duplicate);
}

/// 🔴 #9923 Q8: a not-ready policy runs no loop and holds no socket; once
/// ready, it connects (so the count can go red).
#[tokio::test(start_paused = true)]
async fn socket_mode_source_absent_when_policy_not_ready() {
    let fx = fixture();
    let env = Arc::new(TestEnv::new(&fx.path, &fx.log));
    *env.policy_ready.lock().expect("ready") = Err("snapshot_never_loaded".into());
    let transport = Arc::new(FakeTransport::new([]));
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        Arc::clone(&env) as Arc<dyn SocketEnv>,
        Arc::clone(&transport) as Arc<dyn SocketTransport>,
        cancel.clone(),
        Timing::default(),
    ));
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert!(transport.connects().is_empty(), "a socket while not ready");
    // Q8 per event too: not ready, so no claim and no ack.
    let mut conn = FakeConn::new(&fx.log, []);
    handle_event(
        &*env,
        &mut conn,
        envelope(&event_frame("E1", "Ev1", 0, "yes")),
    )
    .await;
    assert!(entries(&fx.log).is_empty(), "{:?}", entries(&fx.log));
    *env.policy_ready.lock().expect("ready") = Ok(());
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!transport.connects().is_empty(), "ready, so it connects");
    cancel.cancel();
    task.await.expect("run ends");
}

fn fast() -> Timing {
    Timing {
        poll: Duration::from_millis(10),
        backoff_min: Duration::from_secs(1),
        backoff_max: Duration::from_secs(8),
    }
}

/// 🔴 #9923 (plan item 7): a dropped socket reconnects after a doubling
/// wait, and the question open before the drop is still open and still
/// answerable after it.
#[tokio::test(start_paused = true)]
async fn socket_loss_backs_off_and_denies_nothing_open() {
    let fx = fixture();
    let inbound = Arc::new(Fake::new());
    let question = inbound.open("owner-0", OWNER, &owner_conv());
    let mut env = TestEnv::new(&fx.path, &fx.log);
    env.inbound = Some(Arc::clone(&inbound));
    let hello = || Some(Ok(r#"{"type":"hello"}"#.to_owned()));
    let first = FakeConn::new(&fx.log, [hello(), Some(Err("connection reset".to_owned()))]);
    let second = FakeConn::new(
        &fx.log,
        [hello(), Some(Ok(event_frame("E1", "Ev1", 0, "approve")))],
    );
    let mut transport = FakeTransport::new([first, second]);
    let open_at_connect = Arc::new(Mutex::new(Vec::new()));
    let (sink, table) = (Arc::clone(&open_at_connect), Arc::clone(&inbound));
    transport.on_connect = Some(Box::new(move |_| {
        let open = table.table.state(question).is_some_and(|s| !s.answered);
        sink.lock().expect("sink").push(open);
    }));
    let transport = Arc::new(transport);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        Arc::new(env),
        Arc::clone(&transport) as Arc<dyn SocketTransport>,
        cancel.clone(),
        fast(),
    ));
    tokio::time::sleep(Duration::from_secs(20)).await;
    cancel.cancel();
    task.await.expect("run ends");
    let at = transport.connects();
    assert!(at.len() >= 4, "reconnected: {} connects", at.len());
    let gaps: Vec<Duration> = at.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] >= Duration::from_secs(1), "no backoff: {gaps:?}");
    assert!(gaps[2] >= gaps[1] * 2, "the wait doubles: {gaps:?}");
    assert_eq!(
        open_at_connect.lock().expect("sink")[..2],
        [true, true],
        "the drop closed the question"
    );
    assert!(
        inbound.table.answer(question).is_some(),
        "answered after the drop"
    );
}

/// 🔴 #9923 (plan item 9): sender and conversation come from Slack's
/// fields; text naming another user or thread changes neither.
#[test]
fn envelope_sender_comes_from_slack_fields_not_text() {
    let text = "user: U0EVIL thread_ts=1.2 channel D0EVIL [Q-1] approve";
    let raw = event_frame("E1", "Ev1", 0, text);
    let event = envelope(&raw);
    let message = event.message.expect("a DM message");
    assert_eq!(message.sender, OWNER);
    assert_eq!(message.conversation, owner_conv());
    assert_eq!(message.text, text, "the text is carried as data");
    assert_eq!(
        (event.envelope_id.as_str(), event.event_id.as_deref()),
        ("E1", Some("Ev1"))
    );
}

/// 🔴 #9923 Q7: hello, disconnect, and every frame not acted on.
#[test]
fn parse_frame_classifies_hello_disconnect_and_other() {
    assert_eq!(parse_frame(r#"{"type":"hello"}"#), Frame::Hello);
    assert_eq!(
        parse_frame(r#"{"type":"disconnect","reason":"refresh_requested"}"#),
        Frame::Disconnect {
            reason: "refresh_requested".into()
        }
    );
    for raw in [
        "not json",
        r#"{"type":"slash_commands","envelope_id":"E1"}"#,
        r#"{"type":"events_api","payload":{"event_id":"Ev1"}}"#,
        r#"{"type":"surprise"}"#,
    ] {
        assert!(matches!(parse_frame(raw), Frame::Other { .. }), "{raw}");
    }
    let base: serde_json::Value =
        serde_json::from_str(&event_frame("E1", "Ev1", 0, "x")).expect("json");
    for (field, value) in [
        ("subtype", serde_json::json!("message_changed")),
        ("bot_id", serde_json::json!("B0BOT")),
        ("channel", serde_json::json!("C0PUBLIC")),
        ("channel_type", serde_json::json!("channel")),
        ("user", serde_json::json!("B0BOT")),
        ("type", serde_json::json!("app_mention")),
    ] {
        let mut v = base.clone();
        v["payload"]["event"][field] = value;
        let event = envelope(&v.to_string());
        assert!(event.message.is_none(), "{field} acted on");
        assert_eq!(event.event_id.as_deref(), Some("Ev1"), "still claimed");
    }
    let mut v = base;
    v["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("event_id");
    assert_eq!(envelope(&v.to_string()).event_id, None);
}

/// 🔴 #9923 (plan item 3): an envelope with no event id and a non-message
/// event: the first is never acked; the second is claimed, acked, ignored.
#[tokio::test]
async fn unclaimable_envelope_is_never_acked_and_non_message_is_ignored() {
    let fx = fixture();
    let env = TestEnv::new(&fx.path, &fx.log);
    let mut conn = FakeConn::new(&fx.log, []);
    let mut no_id = envelope(&event_frame("E1", "Ev1", 0, "x"));
    no_id.event_id = None;
    handle_event(&env, &mut conn, no_id).await;
    assert!(entries(&fx.log).is_empty());
    let mut ignored = envelope(&event_frame("E2", "Ev2", 0, "x"));
    ignored.message = None;
    handle_event(&env, &mut conn, ignored).await;
    assert_eq!(entries(&fx.log), ["claim:Ev2", "ack:E2", "settle:Ev2"]);
}

/// 🔴 #9923: a failed ack leaves the claim standing; the redelivery is
/// acked and dropped, so the effect runs once.
#[tokio::test]
async fn failed_ack_keeps_the_claim_so_the_redelivery_is_dropped() {
    let fx = fixture();
    let env = TestEnv::new(&fx.path, &fx.log);
    let mut conn = FakeConn::new(&fx.log, []);
    conn.ack_fails = true;
    handle_event(&env, &mut conn, envelope(&event_frame("E1", "Ev1", 0, "x"))).await;
    conn.ack_fails = false;
    handle_event(&env, &mut conn, envelope(&event_frame("E2", "Ev1", 1, "x"))).await;
    assert_eq!(env.acts(), 1);
    assert_eq!(conn.acks(), 2);
}

/// 🔴 #9923 Q6: the fake server is named only in a sandbox, and only as a
/// loopback `ws://` URL.
#[test]
fn fake_socket_server_is_unreachable_outside_the_sandbox() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let home = tmp.path();
    let file = fake_url_path(home);
    std::fs::create_dir_all(file.parent().expect("dir")).expect("dir");
    std::fs::write(&file, "ws://127.0.0.1:18999/link\n").expect("write");
    assert_eq!(fake_url(false, Some(home)), None, "outside the sandbox");
    assert_eq!(
        fake_url(true, Some(home)).as_deref(),
        Some("ws://127.0.0.1:18999/link")
    );
    for url in [
        "wss://example.com/",
        "ws://10.0.0.1:1/",
        "ws://127.0.0.1.evil:1/",
    ] {
        std::fs::write(&file, url).expect("write");
        assert_eq!(fake_url(true, Some(home)), None, "{url}");
    }
    assert_eq!(fake_url(true, None), None);
}

/// 🔴 #9923 Q6: only a `wss://` URL on a Slack host is connected to.
#[test]
fn socket_url_accepts_only_a_slack_wss_url() {
    let ok = r#"{"ok":true,"url":"wss://wss-primary.slack.com/link/?ticket=t"}"#;
    assert_eq!(
        socket_url(200, ok).as_deref(),
        Ok("wss://wss-primary.slack.com/link/?ticket=t")
    );
    for (status, body) in [
        (500, ok),
        (200, "nope"),
        (200, r#"{"ok":false,"error":"invalid_auth"}"#),
        (200, r#"{"ok":true}"#),
        (
            200,
            r#"{"ok":true,"url":"ws://wss-primary.slack.com/link"}"#,
        ),
        (
            200,
            r#"{"ok":true,"url":"wss://slack.com.evil.example/link"}"#,
        ),
        (200, r#"{"ok":true,"url":"wss://evilslack.com/link"}"#),
    ] {
        assert!(socket_url(status, body).is_err(), "{status} {body}");
    }
}

/// 🔴 #9923: the WebSocket connection acks with `{"envelope_id": …}` and
/// answers a ping, against a real loopback server.
#[tokio::test]
async fn ws_conn_acks_by_envelope_id_and_answers_pings() {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(tcp)
            .await
            .expect("handshake");
        ws.send(Message::Ping(b"p".to_vec())).await.expect("ping");
        ws.send(Message::Text(r#"{"type":"hello"}"#.into()))
            .await
            .expect("hello");
        let mut seen = Vec::new();
        while seen.len() < 2 {
            match ws.next().await {
                Some(Ok(Message::Pong(p))) => {
                    seen.push(format!("pong:{}", String::from_utf8_lossy(&p)))
                }
                Some(Ok(Message::Text(t))) => seen.push(t),
                Some(Ok(_)) => {}
                other => panic!("server read: {other:?}"),
            }
        }
        seen
    });
    let mut conn = super::transport::WsConn::connect(&format!("ws://{addr}/link"))
        .await
        .expect("connect");
    let first = conn.recv().await.expect("a frame").expect("ok");
    assert_eq!(parse_frame(&first), Frame::Hello);
    conn.ack("E1").await.expect("ack");
    let seen = server.await.expect("server");
    assert!(seen.contains(&"pong:p".to_owned()), "{seen:?}");
    let ack = seen.iter().find(|s| s.starts_with('{')).expect("the ack");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(ack).expect("json"),
        serde_json::json!({ "envelope_id": "E1" })
    );
}

/// Write the host ceiling under `home` with `connection` for Slack.
fn write_host(home: &Path, connection: &str) {
    let path = trusty_common::crate_config::crate_config_path_at(home, "trusty-mpm");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let text = format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {connection}\n    projects: [/abs]\n"
    );
    std::fs::write(&path, text).expect("write host");
}

/// 🔴 #9923 (plan item 2): the app token is read through the host's
/// `app_ref` alone: the door is asked for `app_ref`'s registry variable and
/// nothing else, and no `app_ref` asks it for nothing.
#[test]
fn daemon_holds_app_token_via_app_ref_only() {
    use super::token::{app_ref, resolve_app_token_with};
    let tmp = crate::test_support::hermetic_temp_dir();
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    assert!(app_ref(None).is_err());
    assert!(app_ref(Some(&home)).is_err(), "no config.yaml");
    write_host(&home, "{ bot_ref: slack }");
    let refused = app_ref(Some(&home)).expect_err("no app_ref");
    assert!(refused.contains("app_ref"), "{refused}");
    write_host(&home, "{ bot_ref: slack, app_ref: slack-app }");
    let named = app_ref(Some(&home)).expect("app_ref");
    assert_eq!(named, "slack-app");
    let asked = Mutex::new(Vec::new());
    let token = resolve_app_token_with(&named, |var| {
        asked.lock().expect("asked").push(var.to_owned());
        Ok("xapp-test".to_owned())
    })
    .expect("token");
    assert_eq!(token.expose(), "xapp-test");
    assert_eq!(*asked.lock().expect("asked"), ["SLACK_APP_TOKEN"]);
}

/// The production files of the Socket Mode path.
fn socket_mode_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        src.join("channels/socket_mode.rs"),
        src.join("channels/question_table/claims.rs"),
        src.join("daemon/managed_routes/socket_mode.rs"),
    ];
    for entry in std::fs::read_dir(src.join("channels/socket_mode")).expect("dir") {
        files.push(entry.expect("entry").path());
    }
    files
}

/// 🔴 #9923 (plan item 2): no user token and no environment variable: the
/// user and bot refs are refused before the door is asked, the host parser
/// refuses `slack-user` as an `app_ref`, and no Socket Mode source reads an
/// environment variable or names a token variable.
#[test]
fn no_user_token_loaded_anywhere_in_channels_path() {
    use super::token::{check_app_ref, resolve_app_token_with};
    for refused in ["slack-user", "slack", "github", ""] {
        assert!(check_app_ref(refused).is_err(), "{refused}");
        let called = std::sync::atomic::AtomicBool::new(false);
        let out = resolve_app_token_with(refused, |_| {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("leaked".to_owned())
        });
        assert!(out.is_err(), "{refused}");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "{refused}: door asked"
        );
    }
    let tmp = crate::test_support::hermetic_temp_dir();
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    write_host(&home, "{ bot_ref: slack, app_ref: slack-user }");
    assert!(
        super::token::app_ref(Some(&home)).is_err(),
        "slack-user as app_ref"
    );
    let forbidden = [
        "env::var",
        "var_os(",
        "\"slack-user\"",
        "SLACK_USER_TOKEN",
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "resolve_secret(",
        "dotenv",
    ];
    for file in socket_mode_sources() {
        let text = std::fs::read_to_string(&file).expect("read source");
        for needle in forbidden {
            assert!(!text.contains(needle), "{} names {needle}", file.display());
        }
        let door = text.contains("resolve_bounded_gated");
        assert_eq!(
            door,
            file.ends_with("socket_mode/token.rs"),
            "{}: the credential door is called from token.rs only",
            file.display()
        );
    }
}

/// 🔴 #9923 Q3: the claim log is named under the home, beside the question
/// table, so the trust-anchor seal names it too.
#[test]
fn claim_log_sits_beside_the_question_table() {
    let home = Path::new("/h");
    assert_eq!(
        claims_path(home),
        Path::new("/h/.trusty-tools/trusty-mpm/channels/inbound_claims.jsonl")
    );
}
