//! Page-op parity: replay the Python 0.7.0 page-op capture against the Rust
//! broker and compare every reply, worker command and audit line byte for byte
//! (`tests/fixtures/frames_pages`, `audit_pages.jsonl`, `capture/capture_pages.py`).
//!
//! The capture is sequential: its driver played both the agents and the
//! worker, so each entry (`open`, `send`, `recv`, `close` on a named
//! connection) can be replayed in order. Randomness, the audit clock, the grant
//! clock and the page clocks are the same fixed seams the capture installed.

mod common;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use common::{
    Det, Fixed, Ticks, fixture_key, fixture_service, fixtures, items, json, s, state_dir,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use trusty_browser::audit::Clock;
use trusty_browser::broker::{Broker, BrokerOptions, HostCheck};
use trusty_browser::entropy::Monotonic;
use trusty_browser::peer_darwin::HostRefused;
use trusty_browser::pyjson::Json;
use trusty_browser::vault::{KeyStore, MemoryKeyStore};

/// The capture's seam: every enrollment caller passes the Chrome-host check.
struct Open;

impl HostCheck for Open {
    fn verify(&self, _: &std::os::unix::net::UnixStream, _: &Path) -> Result<(), HostRefused> {
        Ok(())
    }
}

/// A fixed clock (`pages.time.monotonic`, `pages.time.time` in the capture).
struct At(f64);

impl Monotonic for At {
    fn now(&self) -> f64 {
        self.0
    }
}

impl Clock for At {
    fn now(&self) -> f64 {
        self.0
    }
}

fn options(state: &Path, lease_clock: Arc<dyn Monotonic>) -> BrokerOptions {
    let store = MemoryKeyStore::new();
    let account = fs::canonicalize(state)
        .expect("canon")
        .to_string_lossy()
        .into_owned();
    store
        .set(&fixture_service(), &account, &fixture_key())
        .expect("key");
    let mut o = BrokerOptions::new(state, Box::new(store));
    o.service = fixture_service();
    o.audit_clock = Box::new(Ticks(AtomicU64::new(0)));
    o.entropy = Arc::new(Det::default());
    o.clock = Arc::new(Fixed);
    o.lease_clock = lease_clock;
    o.wall_clock = Arc::new(At(1_790_000_000.0));
    o.host_check = Arc::new(Open);
    o
}

async fn recv(s: &mut UnixStream) -> Vec<u8> {
    let mut head = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut head))
        .await
        .expect("reply in time")
        .expect("header");
    let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
    s.read_exact(&mut body).await.expect("body");
    [head.to_vec(), body].concat()
}

fn entry_bytes(entry: &Json) -> Vec<u8> {
    fs::read(fixtures().join("frames_pages").join(s(entry, "file"))).expect("frame")
}

/// Replay the capture through the entry labelled `until` (all of it for `None`);
/// panics at the first frame that differs and returns the open connections.
async fn replay(state: &Path, until: Option<&str>) -> HashMap<String, UnixStream> {
    let index = json("frames_pages/index.json");
    let mut conns: HashMap<String, UnixStream> = HashMap::new();
    for entry in items(&index) {
        let conn = s(entry, "conn").to_owned();
        let label = entry.get("label").and_then(Json::as_str).map(str::to_owned);
        match s(entry, "action") {
            "open" => {
                let sock = UnixStream::connect(state.join(s(entry, "socket")))
                    .await
                    .expect("connect");
                conns.insert(conn, sock);
            }
            "send" => {
                let sock = conns.get_mut(&conn).expect("open conn");
                // A refusal sent before the request is read can close the socket first.
                let _ = sock.write_all(&entry_bytes(entry)).await;
            }
            "recv" => {
                let sock = conns.get_mut(&conn).expect("open conn");
                let got = recv(sock).await;
                let want = entry_bytes(entry);
                let label = s(entry, "label");
                assert_eq!(
                    String::from_utf8_lossy(&got[4..]),
                    String::from_utf8_lossy(&want[4..]),
                    "{label}"
                );
                assert_eq!(got, want, "{label} bytes");
            }
            "write" => {
                // #11363: the published manifest whose stamp the hello acknowledgement carries.
                let path = state.join(s(entry, "path"));
                fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                fs::write(path, entry_bytes(entry)).expect("write");
            }
            "close" => {
                conns.remove(&conn);
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            other => panic!("unknown action {other}"),
        }
        if until.is_some() && label.as_deref() == until {
            break;
        }
    }
    conns
}

/// Every page op, `release`, `switch`, the journal, each refusal and the
/// asset stamp in the hello acknowledgement answer as Python does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_ops_replay_byte_identically() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let conns = replay(state, None).await;
    assert!(conns.is_empty(), "every connection closed");
    broker.close().await;
    let audit = fs::read_to_string(state.join("audit.jsonl")).expect("audit");
    let want = fs::read_to_string(fixtures().join("audit_pages.jsonl")).expect("fixture audit");
    let (got, want): (Vec<_>, Vec<_>) = (audit.lines().collect(), want.lines().collect());
    for (n, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "audit line {n}");
    }
    assert_eq!(got.len(), want.len(), "audit line count");
}

/// The select-without-`request_id` frames of the capture: `select` takes none,
/// so a malformed probe is `BrowserRefused` and an extra `request_id` is a bare
/// `InvalidRequest` (not the `field`-naming one).
#[test]
fn select_request_id_cases_match_the_capture() {
    let index = json("frames_pages/index.json");
    let reply = |label: &str| {
        let entry = items(&index)
            .iter()
            .find(|e| e.get("label").and_then(Json::as_str) == Some(label))
            .unwrap_or_else(|| panic!("{label}"));
        Json::parse(&entry_bytes(entry)[4..]).expect("json")
    };
    let bare = r#"{"ok":false,"error":"InvalidRequest"}"#;
    let refused = r#"{"ok":false,"error":"BrowserRefused"}"#;
    let named = r#"{"ok":false,"error":"InvalidRequest","field":"request_id"}"#;
    let dumps = |v: Json| v.dumps(false).expect("dumps");
    assert_eq!(dumps(reply("select-with-request-id-reply")), bare);
    assert_eq!(dumps(reply("select-malformed-probe-reply")), refused);
    for label in ["no", "empty", "int"] {
        let got = reply(&format!("page-click-{label}-request-id-reply"));
        assert_eq!(dumps(got), named, "{label}");
    }
}

/// A lease clock the test moves (f64 bits).
#[derive(Default)]
struct Settable(AtomicU64);

impl Settable {
    fn set(&self, t: f64) {
        self.0
            .store(t.to_bits(), std::sync::atomic::Ordering::SeqCst);
    }
}

impl Monotonic for Settable {
    fn now(&self) -> f64 {
        f64::from_bits(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

async fn send_json(sock: &mut UnixStream, frame: &str) {
    let body = frame.as_bytes();
    let mut raw = (body.len() as u32).to_ne_bytes().to_vec();
    raw.extend_from_slice(body);
    sock.write_all(&raw).await.expect("send");
}

async fn recv_json(sock: &mut UnixStream) -> Json {
    let raw = recv(sock).await;
    Json::parse(&raw[4..]).expect("json")
}

/// Answer one worker command with `result`; returns the command.
async fn answer(worker: &mut UnixStream, result: &str) -> Json {
    let cmd = recv_json(worker).await;
    let id = cmd.get("id").and_then(Json::as_str).expect("id").to_owned();
    let tab = cmd.get("tab").cloned().unwrap_or(Json::Null);
    let email = cmd
        .get("email")
        .and_then(Json::as_str)
        .expect("email")
        .to_owned();
    let reply = format!(
        r#"{{"id":"{id}","ok":true,"email":"{email}","result":{result},"tab":{}}}"#,
        tab.dumps(false).expect("tab")
    );
    send_json(worker, &reply).await;
    cmd
}

fn wire(v: &Json) -> String {
    v.dumps(false).expect("dumps")
}

/// A lease idle past its TTL frees the tab for another session, and its old
/// token answers `StaleLease`; a renewed lease does not expire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_lease_frees_the_tab_for_another_session() {
    let dir = state_dir();
    let state = dir.path();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let broker =
        Broker::start(options(state, Arc::clone(&clock) as Arc<dyn Monotonic>)).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let lease = r#"{"op":"page_read","lease":"YtbbXmIbar4-E6OcqOQZedqh522fyHj2"}"#;
    let mut b = conns.remove("b").expect("b");
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    clock.set(5000.0 + 59.999);
    send_json(&mut b, r#"{"op":"select","tab":7}"#).await;
    assert_eq!(
        recv_json(&mut b).await.get("error"),
        Some(&Json::from("TabClaimed"))
    );
    clock.set(5000.0 + 60.0);
    send_json(&mut b, r#"{"op":"select","tab":7}"#).await;
    answer(
        &mut worker,
        r#"{"access":true,"document":"d","origin":"https://example.test","title":"t"}"#,
    )
    .await;
    let taken = recv_json(&mut b).await;
    assert_eq!(taken.get("ok"), Some(&Json::Bool(true)), "{}", wire(&taken));
    send_json(&mut a, lease).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"StaleLease"}"#
    );
    broker.close().await;
}

/// A result line that cannot be written ends the grant (`audit`): a mutating
/// op answers a journaled `OutcomeUnknown`, then the session hears `GrantEnded`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_result_line_ends_the_grant() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("select-reply")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let click = r#"{"op":"page_click","lease":"WKYMQmzaE-9QA7jfA8GUGUSwUOcWzFEK","request_id":"rx","ref":"e1"}"#;
    send_json(&mut a, click).await;
    let cmd = recv_json(&mut worker).await;
    // The admitted line is written; make the log shared so the result line fails closed.
    let log = state.join("audit.jsonl");
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).expect("chmod");
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    let reply = format!(
        r#"{{"id":"{id}","ok":true,"email":"alpha@example.test","result":{{"document":"d"}},"tab":7}}"#
    );
    send_json(&mut worker, &reply).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"OutcomeUnknown"}"#
    );
    send_json(&mut a, click).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"GrantEnded","reason":"audit"}"#
    );
    broker.close().await;
}

// #11371: emulation resets on every lease end. Each test below fails when its
// reset path is removed (`src/emulation.rs`).
const EMULATED: &str = r#"{"javascript":false,"color_scheme":"dark","width":1440,"height":900}"#;
const RESET: &str = r#"{"javascript":true,"color_scheme":null,"width":0,"height":0}"#;
const A_LEASE: &str = "WKYMQmzaE-9QA7jfA8GUGUSwUOcWzFEK";
const A_EARLY_LEASE: &str = "YtbbXmIbar4-E6OcqOQZedqh522fyHj2";

/// Emulate on `lease` and answer the worker; returns the agent's reply.
async fn emulate(agent: &mut UnixStream, worker: &mut UnixStream, lease: &str) -> Json {
    let frame = format!(
        r#"{{"op":"page_emulate","lease":"{lease}","javascript":false,"color_scheme":"dark","width":1440,"height":900}}"#
    );
    send_json(agent, &frame).await;
    let cmd = answer(worker, EMULATED).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_emulate")));
    assert_eq!(cmd.get("javascript"), Some(&Json::Bool(false)));
    assert_eq!(cmd.get("width"), Some(&Json::Int(1440)));
    recv_json(agent).await
}

/// The next worker command is the emulation reset of tab 7; answers it.
async fn expect_reset(worker: &mut UnixStream) {
    let cmd = answer(worker, RESET).await;
    assert_eq!(
        cmd.get("op"),
        Some(&Json::from("page_emulate")),
        "{}",
        wire(&cmd)
    );
    assert_eq!(cmd.get("tab"), Some(&Json::Int(7)));
    assert_eq!(cmd.get("javascript"), Some(&Json::Bool(true)));
    assert_eq!(cmd.get("color_scheme"), Some(&Json::Null));
    assert_eq!(cmd.get("width"), Some(&Json::Int(0)));
    assert_eq!(cmd.get("height"), Some(&Json::Int(0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_emulated_tab_resets_on_release() {
    let dir = state_dir();
    let broker = Broker::start(options(dir.path(), Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(dir.path(), Some("select-reply")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let reply = emulate(&mut a, &mut worker, A_LEASE).await;
    assert_eq!(
        wire(&reply),
        r#"{"ok":true,"tab":7,"javascript":false,"color_scheme":"dark","width":1440,"height":900}"#
    );
    send_json(
        &mut a,
        &format!(r#"{{"op":"release","lease":"{A_LEASE}"}}"#),
    )
    .await;
    // The release answers only after the reset; the watcher alone would answer first.
    let early = tokio::time::timeout(Duration::from_millis(300), recv_json(&mut a)).await;
    assert!(
        early.is_err(),
        "release answered before the emulation reset"
    );
    expect_reset(&mut worker).await;
    assert_eq!(wire(&recv_json(&mut a).await), r#"{"ok":true}"#);
    broker.close().await;
}

/// A lease that expired while its tab was emulated: the next `select` resets
/// the tab before it probes, so the new holder never sees the old state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_lease_resets_a_tab_left_emulated() {
    let dir = state_dir();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let lease_clock = Arc::clone(&clock) as Arc<dyn Monotonic>;
    let broker = Broker::start(options(dir.path(), lease_clock)).expect("start");
    let mut conns = replay(dir.path(), Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut b = conns.remove("b").expect("b");
    let mut worker = conns.remove("worker").expect("worker");
    let reply = emulate(&mut a, &mut worker, A_EARLY_LEASE).await;
    assert_eq!(reply.get("ok"), Some(&Json::Bool(true)), "{}", wire(&reply));
    clock.set(5000.0 + 60.0);
    send_json(&mut b, r#"{"op":"select","tab":7}"#).await;
    expect_reset(&mut worker).await;
    let probe = answer(
        &mut worker,
        r#"{"access":true,"document":"d","origin":"https://example.test","title":"t"}"#,
    )
    .await;
    assert_eq!(probe.get("op"), Some(&Json::from("page_tab")));
    assert_eq!(recv_json(&mut b).await.get("ok"), Some(&Json::Bool(true)));
    broker.close().await;
}

/// An emulated lease that expires with nobody calling still resets (the watcher).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_emulated_lease_resets_without_a_caller() {
    let dir = state_dir();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let lease_clock = Arc::clone(&clock) as Arc<dyn Monotonic>;
    let broker = Broker::start(options(dir.path(), lease_clock)).expect("start");
    let mut conns = replay(dir.path(), Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    emulate(&mut a, &mut worker, A_EARLY_LEASE).await;
    clock.set(5000.0 + 60.0);
    expect_reset(&mut worker).await;
    broker.close().await;
}

/// A session that closes while its tab is emulated resets the tab.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_session_resets_its_emulated_tab() {
    let dir = state_dir();
    let broker = Broker::start(options(dir.path(), Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(dir.path(), Some("select-reply")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    emulate(&mut a, &mut worker, A_LEASE).await;
    drop(a);
    // Well inside the watcher's one-second period, so only the session-end reset meets it.
    let soon = tokio::time::timeout(Duration::from_millis(500), expect_reset(&mut worker)).await;
    assert!(soon.is_ok(), "no reset at session end");
    broker.close().await;
}

/// A reset the worker refuses keeps its record: the watcher sends it again
/// until the worker accepts, so the emulation cannot reach a later lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_emulation_reset_is_retried() {
    let dir = state_dir();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let lease_clock = Arc::clone(&clock) as Arc<dyn Monotonic>;
    let broker = Broker::start(options(dir.path(), lease_clock)).expect("start");
    let mut conns = replay(dir.path(), Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    emulate(&mut a, &mut worker, A_EARLY_LEASE).await;
    clock.set(5000.0 + 60.0);
    let cmd = recv_json(&mut worker).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_emulate")));
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    send_json(
        &mut worker,
        &format!(r#"{{"id":"{id}","ok":false,"error":"Unavailable"}}"#),
    )
    .await;
    let retry = tokio::time::timeout(Duration::from_secs(5), expect_reset(&mut worker)).await;
    assert!(retry.is_ok(), "the refused reset was not retried");
    broker.close().await;
}

/// Select fails closed: while a refused reset is pending, a new lease on the
/// tab is refused before any `page_tab` probe, and its claim is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_refuses_a_tab_whose_emulation_reset_failed() {
    let dir = state_dir();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let lease_clock = Arc::clone(&clock) as Arc<dyn Monotonic>;
    let broker = Broker::start(options(dir.path(), lease_clock)).expect("start");
    let mut conns = replay(dir.path(), Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut b = conns.remove("b").expect("b");
    let mut worker = conns.remove("worker").expect("worker");
    emulate(&mut a, &mut worker, A_EARLY_LEASE).await;
    clock.set(5000.0 + 60.0);
    send_json(&mut b, r#"{"op":"select","tab":7}"#).await;
    let cmd = recv_json(&mut worker).await;
    assert_eq!(
        cmd.get("op"),
        Some(&Json::from("page_emulate")),
        "{}",
        wire(&cmd)
    );
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    send_json(
        &mut worker,
        &format!(r#"{{"id":"{id}","ok":false,"error":"Unavailable"}}"#),
    )
    .await;
    let reply = tokio::time::timeout(Duration::from_secs(5), recv_json(&mut b))
        .await
        .expect("select answered without a page_tab probe");
    assert_eq!(
        wire(&reply),
        r#"{"ok":false,"error":"EmulationResetPending","tab":7,"retryable":true}"#
    );
    // The next worker command is the retried reset, never a probe.
    expect_reset(&mut worker).await;
    broker.close().await;
}

// #11808: an action on a frame-scoped ref answers with the child frame it ran
// in, and its `operation` audit line names that child's host in `frames`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_action_is_audited_with_the_frame_origin() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("select-reply")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let typed = format!(
        r#"{{"op":"page_type","lease":"{A_LEASE}","request_id":"f1","ref":"7:22.0","text":"hi"}}"#
    );
    send_json(&mut a, &typed).await;
    let frame = r#"{"id":7,"origin":"https://b.test:8443"}"#;
    let cmd = answer(
        &mut worker,
        &format!(r#"{{"document":"d","frame":{frame}}}"#),
    )
    .await;
    assert_eq!(cmd.get("ref"), Some(&Json::from("7:22.0")));
    assert_eq!(
        wire(&recv_json(&mut a).await),
        format!(r#"{{"ok":true,"tab":7,"document":"d","frame":{frame}}}"#)
    );
    let audit = fs::read_to_string(state.join("audit.jsonl")).expect("audit");
    let last = audit.lines().last().expect("a line");
    let last = Json::parse(last.as_bytes()).expect("json");
    assert_eq!(last.get("op"), Some(&Json::from("page_type")), "{audit}");
    assert_eq!(last.get("outcome"), Some(&Json::from("ok")));
    assert_eq!(
        last.get("frames"),
        Some(&Json::Array(vec![Json::from("b.test")]))
    );
    // A malformed frame is refused, never relayed as a frame action.
    let bad =
        format!(r#"{{"op":"page_hover","lease":"{A_LEASE}","request_id":"f2","ref":"7:22.0"}}"#);
    send_json(&mut a, &bad).await;
    let zero = r#"{"document":"d","frame":{"id":0,"origin":"https://b.test"}}"#;
    answer(&mut worker, zero).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"BrowserRefused"}"#
    );
    broker.close().await;
}

/// Refuse the next worker command with `error` and the extra JSON `fields`.
async fn refuse_next(worker: &mut UnixStream, error: &str, fields: &str) {
    let cmd = recv_json(worker).await;
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    let reply = format!(r#"{{"id":"{id}","ok":false,"error":"{error}"{fields}}}"#);
    send_json(worker, &reply).await;
}

/// The last audit line of `state`.
fn last_audit(state: &Path) -> Json {
    let audit = fs::read_to_string(state.join("audit.jsonl")).expect("audit");
    Json::parse(audit.lines().last().expect("a line").as_bytes()).expect("json")
}

// #11808: a refused frame action, an unfinished one included, still names its
// child frame to the agent and in the `operation` audit line's `frames`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_frame_action_is_audited_with_the_frame_origin() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("select-reply")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let frame = r#"{"id":7,"origin":"https://b.test:8443"}"#;
    let hosts = Some(Json::Array(vec![Json::from("b.test")]));
    let typed = format!(
        r#"{{"op":"page_type","lease":"{A_LEASE}","request_id":"r1","ref":"7:22.0","text":"hi"}}"#
    );
    send_json(&mut a, &typed).await;
    let denied = format!(r#","tab":7,"origin":"https://b.test:8443","frame":{frame}"#);
    refuse_next(&mut worker, "PermissionNeeded", &denied).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        format!(r#"{{"ok":false,"error":"PermissionNeeded"{denied}}}"#)
    );
    let line = last_audit(state);
    assert_eq!(line.get("outcome"), Some(&Json::from("PermissionNeeded")));
    assert_eq!(line.get("frames"), hosts.as_ref());
    let clicked =
        format!(r#"{{"op":"page_click","lease":"{A_LEASE}","request_id":"r2","ref":"7:22.1"}}"#);
    send_json(&mut a, &clicked).await;
    refuse_next(
        &mut worker,
        "OutcomeUnknown",
        &format!(r#","frame":{frame}"#),
    )
    .await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        format!(r#"{{"ok":false,"error":"OutcomeUnknown","frame":{frame}}}"#)
    );
    let line = last_audit(state);
    assert_eq!(line.get("op"), Some(&Json::from("page_click")));
    assert_eq!(line.get("outcome"), Some(&Json::from("OutcomeUnknown")));
    assert_eq!(line.get("frames"), hosts.as_ref());
    broker.close().await;
}

/// #11211 AC2: `page_screenshot` on a tab this session does not lease (another
/// session's live lease, or no lease) is refused `StaleLease` by `leased` and
/// sends the worker no capture; the holder's own lease does reach the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_screenshot_refuses_a_tab_this_session_does_not_lease() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut b = conns.remove("b").expect("b");
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let shoot = |lease: &str| format!(r#"{{"op":"page_screenshot","lease":"{lease}"}}"#);
    for lease in [A_EARLY_LEASE, "not-a-lease-of-this-session"] {
        send_json(&mut b, &shoot(lease)).await;
        assert_eq!(
            wire(&recv_json(&mut b).await),
            r#"{"ok":false,"error":"StaleLease"}"#,
            "{lease}"
        );
    }
    let quiet = tokio::time::timeout(Duration::from_millis(300), recv_json(&mut worker)).await;
    assert!(quiet.is_err(), "a refused screenshot reached the worker");
    send_json(&mut a, &shoot(A_EARLY_LEASE)).await;
    let cmd = recv_json(&mut worker).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_screenshot")));
    assert_eq!(cmd.get("tab"), Some(&Json::Int(7)));
    broker.close().await;
}

/// #11208: `close_tab` on a tab this session does not lease answers
/// `TabNotLeased` and reaches no worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_tab_refuses_a_tab_this_session_does_not_lease() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut b = conns.remove("b").expect("b");
    let mut worker = conns.remove("worker").expect("worker");
    for tab in [7, 99] {
        let close = format!(r#"{{"op":"close_tab","request_id":"c{tab}","tab":{tab}}}"#);
        send_json(&mut b, &close).await;
        assert_eq!(
            wire(&recv_json(&mut b).await),
            format!(r#"{{"ok":false,"error":"TabNotLeased","tab":{tab}}}"#)
        );
    }
    let quiet = tokio::time::timeout(Duration::from_millis(300), recv_json(&mut worker)).await;
    assert!(quiet.is_err(), "a refused close reached the worker");
    broker.close().await;
}

/// #11208 AC3: a user tab this session leased with `browser_select` but did not
/// open is refused `TabNotOpened`, stays open and keeps its lease; a tab the
/// session opened with `open_tab` closes, and its lease ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_tab_refuses_a_selected_user_tab_and_closes_an_opened_one() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    send_json(&mut a, r#"{"op":"close_tab","request_id":"c7","tab":7}"#).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"TabNotOpened","tab":7}"#
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), recv_json(&mut worker)).await;
    assert!(quiet.is_err(), "the user tab's close reached the worker");
    let read = format!(r#"{{"op":"page_read","lease":"{A_EARLY_LEASE}"}}"#);
    send_json(&mut a, &read).await;
    let cmd = recv_json(&mut worker).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_read")), "lease kept");
    refuse_next_cmd(&mut worker, &cmd, "OutcomeUnknown").await;
    recv_json(&mut a).await;
    let open = r#"{"op":"open_tab","request_id":"o1","url":"https://example.test/"}"#;
    send_json(&mut a, open).await;
    let opened = r#"{"tab":12,"window":1,"access":true,"document":"d12","origin":"https://example.test","title":"New"}"#;
    answer(&mut worker, opened).await;
    let reply = recv_json(&mut a).await;
    assert_eq!(reply.get("tab"), Some(&Json::Int(12)), "{}", wire(&reply));
    let lease = reply.get("lease").and_then(Json::as_str).expect("lease");
    let lease = lease.to_owned();
    send_json(&mut a, r#"{"op":"close_tab","request_id":"c12","tab":12}"#).await;
    let cmd = answer(&mut worker, r#"{"closed":true}"#).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_close_tab")));
    assert_eq!(cmd.get("tab"), Some(&Json::Int(12)));
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":true,"tab":12,"closed":true}"#
    );
    send_json(
        &mut a,
        &format!(r#"{{"op":"page_read","lease":"{lease}"}}"#),
    )
    .await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"StaleLease"}"#
    );
    broker.close().await;
}

/// Refuse the already-received worker command `cmd` with `error`.
async fn refuse_next_cmd(worker: &mut UnixStream, cmd: &Json, error: &str) {
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    let reply = format!(r#"{{"id":"{id}","ok":false,"error":"{error}"}}"#);
    send_json(worker, &reply).await;
}

/// #11211: a screenshot hands out a `capture_id`; a coordinate action maps its
/// image pixels to CSS pixels and is refused `StaleCapture` after 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_maps_clicks_and_goes_stale() {
    use base64::Engine as _;
    let dir = state_dir();
    let state = dir.path();
    let clock = Arc::new(Settable::default());
    clock.set(5000.0);
    let broker =
        Broker::start(options(state, Arc::clone(&clock) as Arc<dyn Monotonic>)).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let lease = "YtbbXmIbar4-E6OcqOQZedqh522fyHj2";
    let read = format!(r#"{{"op":"page_read","lease":"{lease}"}}"#);
    send_json(&mut a, &read).await;
    answer(
        &mut worker,
        r#"{"document":"d1","origin":"https://example.test","url":"https://example.test/","title":"t","text":"","elements":[],"truncated":false}"#,
    )
    .await;
    recv_json(&mut a).await;
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(1600u32.to_be_bytes());
    png.extend(1200u32.to_be_bytes());
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let metrics = r#"{"css_width":800,"css_height":600,"device_pixel_ratio":2,"zoom":1,"scroll_x":0,"scroll_y":0}"#;
    let shoot = format!(r#"{{"op":"page_screenshot","lease":"{lease}"}}"#);
    send_json(&mut a, &shoot).await;
    let shot = format!(
        r#"{{"document":"d1","data":"{data}","region":{{"x":0,"y":0,"width":800,"height":600}},"metrics":{metrics}}}"#
    );
    let cmd = answer(&mut worker, &shot).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_screenshot")));
    let reply = recv_json(&mut a).await;
    assert_eq!(
        reply.get("width"),
        Some(&Json::Int(1600)),
        "{}",
        wire(&reply)
    );
    assert_eq!(reply.get("height"), Some(&Json::Int(1200)));
    let id = reply
        .get("capture_id")
        .and_then(Json::as_str)
        .expect("capture_id")
        .to_owned();
    let click = |rid: &str, capture: &str, x: i64| {
        format!(
            r#"{{"op":"page_click_at","lease":"{lease}","request_id":"{rid}","capture_id":"{capture}","x":{x},"y":100}}"#
        )
    };
    send_json(&mut a, &click("k1", &id, 1600)).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"OutOfViewport","tab":7}"#
    );
    send_json(&mut a, &click("k2", &id, 200)).await;
    let cmd = answer(&mut worker, r#"{"document":"d1"}"#).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_click_at")));
    assert_eq!(cmd.get("x"), Some(&Json::Float(100.25)));
    assert_eq!(cmd.get("y"), Some(&Json::Float(50.25)));
    assert_eq!(cmd.get("document"), Some(&Json::from("d1")));
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":true,"tab":7,"document":"d1"}"#
    );
    clock.set(5000.0 + 5.5);
    for (rid, capture) in [("k3", id.as_str()), ("k4", "unknown")] {
        send_json(&mut a, &click(rid, capture, 200)).await;
        assert_eq!(
            wire(&recv_json(&mut a).await),
            r#"{"ok":false,"error":"StaleCapture","tab":7}"#,
            "{capture}"
        );
    }
    broker.close().await;
}

/// #11211 review: a PNG whose aspect ratio is not its region's (the viewport
/// resized between the metrics read and the shot) is refused `StaleCapture`
/// and hands out no `capture_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_whose_aspect_disagrees_with_its_region_is_stale() {
    use base64::Engine as _;
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let lease = "YtbbXmIbar4-E6OcqOQZedqh522fyHj2";
    send_json(
        &mut a,
        &format!(r#"{{"op":"page_read","lease":"{lease}"}}"#),
    )
    .await;
    answer(
        &mut worker,
        r#"{"document":"d1","origin":"https://example.test","url":"https://example.test/","title":"t","text":"","elements":[],"truncated":false}"#,
    )
    .await;
    recv_json(&mut a).await;
    // An 800x600 region at 2x, but the picture is 1600x1000: 100 CSS px shorter.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(1600u32.to_be_bytes());
    png.extend(1000u32.to_be_bytes());
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let metrics = r#"{"css_width":800,"css_height":600,"device_pixel_ratio":2,"zoom":1,"scroll_x":0,"scroll_y":0}"#;
    send_json(
        &mut a,
        &format!(r#"{{"op":"page_screenshot","lease":"{lease}"}}"#),
    )
    .await;
    let shot = format!(
        r#"{{"document":"d1","data":"{data}","region":{{"x":0,"y":0,"width":800,"height":600}},"metrics":{metrics}}}"#
    );
    answer(&mut worker, &shot).await;
    assert_eq!(
        wire(&recv_json(&mut a).await),
        r#"{"ok":false,"error":"StaleCapture","tab":7}"#
    );
    broker.close().await;
}

/// #11211 review: an `open_tab` whose outcome is unknown is journaled, so a
/// retry with the same `request_id` answers the same refusal and sends the
/// worker no second `page_open_tab`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_open_tab_is_journaled_and_never_opens_a_second_tab() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut worker = conns.remove("worker").expect("worker");
    let open = r#"{"op":"open_tab","request_id":"o1","url":"https://example.test/"}"#;
    send_json(&mut a, open).await;
    let cmd = recv_json(&mut worker).await;
    assert_eq!(cmd.get("op"), Some(&Json::from("page_open_tab")));
    let id = cmd.get("id").and_then(Json::as_str).expect("id");
    let email = cmd.get("email").and_then(Json::as_str).expect("email");
    let failed =
        format!(r#"{{"id":"{id}","ok":false,"email":"{email}","error":"OutcomeUnknown"}}"#);
    send_json(&mut worker, &failed).await;
    let unknown = r#"{"ok":false,"error":"OutcomeUnknown"}"#;
    assert_eq!(wire(&recv_json(&mut a).await), unknown);
    send_json(&mut a, open).await;
    let second = tokio::time::timeout(Duration::from_millis(300), recv_json(&mut worker)).await;
    assert!(
        second.is_err(),
        "a second page_open_tab reached the worker: {}",
        second.map(|c| wire(&c)).unwrap_or_default()
    );
    assert_eq!(wire(&recv_json(&mut a).await), unknown);
    broker.close().await;
}

/// Assert the worker hears no `page_open_tab` before it goes quiet for 300 ms.
async fn no_tab_opened(worker: &mut UnixStream) {
    while let Ok(cmd) = tokio::time::timeout(Duration::from_millis(300), recv_json(worker)).await {
        let op = cmd.get("op").and_then(Json::as_str);
        assert_ne!(op, Some("page_open_tab"), "a tab opened: {}", wire(&cmd));
    }
}

/// #11208 AC2: `open_tab` opens no tab without a live grant held by an
/// attached session. A connection that never attached, a request that carries
/// any tab lease (its own or another session's), and a session whose grant was
/// revoked are each refused, and the worker hears no `page_open_tab`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_tab_opens_no_tab_without_a_held_grant() {
    let dir = state_dir();
    let state = dir.path();
    let broker = Broker::start(options(state, Arc::new(At(5000.0)))).expect("start");
    let mut conns = replay(state, Some("attach-b-ack")).await;
    let mut a = conns.remove("a").expect("a");
    let mut b = conns.remove("b").expect("b");
    let mut worker = conns.remove("worker").expect("worker");
    let open = r#"{"op":"open_tab","request_id":"o1","url":"https://example.test/"}"#;
    let mut bare = UnixStream::connect(state.join("agent.sock"))
        .await
        .expect("connect");
    send_json(&mut bare, open).await;
    assert_eq!(
        wire(&recv_json(&mut bare).await),
        r#"{"ok":false,"error":"InvalidRequest"}"#
    );
    for (own, lease) in [
        (true, A_EARLY_LEASE),
        (false, A_EARLY_LEASE),
        (false, "stale"),
    ] {
        let sock = if own { &mut a } else { &mut b };
        let leased = format!(
            r#"{{"op":"open_tab","request_id":"o2","url":"https://example.test/","lease":"{lease}"}}"#
        );
        send_json(sock, &leased).await;
        assert_eq!(
            wire(&recv_json(sock).await),
            r#"{"ok":false,"error":"InvalidRequest"}"#,
            "{lease}"
        );
    }
    no_tab_opened(&mut worker).await;
    let mut operator = UnixStream::connect(state.join("operator.sock"))
        .await
        .expect("connect");
    send_json(
        &mut operator,
        r#"{"op":"revoke_grant","grant":"g-27f66c0d132f"}"#,
    )
    .await;
    let revoked = recv_json(&mut operator).await;
    assert_eq!(
        revoked.get("ok"),
        Some(&Json::Bool(true)),
        "{}",
        wire(&revoked)
    );
    send_json(&mut b, open).await;
    assert_eq!(
        wire(&recv_json(&mut b).await),
        r#"{"ok":false,"error":"GrantEnded","reason":"revoked"}"#
    );
    no_tab_opened(&mut worker).await;
    broker.close().await;
}
