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
