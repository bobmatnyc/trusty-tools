//! Broker parity: drive the Rust broker with the captured Python 0.7.0 client
//! frames and compare every reply, worker command, audit line and registry
//! row byte for byte (`tests/fixtures/frames`, `audit.jsonl`, `registry.sqlite3`).
//!
//! The capture replaced Python's randomness with one seeded SHA-256 counter
//! stream (`capture.py` `_det`) shared by the client and the broker, and the
//! audit clock with 0.25 s ticks; [`Det`] and [`Ticks`] reproduce both.

mod common;

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use common::{Det, Fixed, Ticks, fixture_key, fixture_service, fixtures, json, state_dir};
use rusqlite::Connection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use trusty_browser::broker::{Broker, BrokerOptions, ChromeHostCheck, HostCheck, SOCKETS};
use trusty_browser::entropy::Entropy;
use trusty_browser::peer_darwin::HostRefused;
use trusty_browser::vault::{KeyStore, MemoryKeyStore};

/// The real Chrome-host check until `open` is set (the capture's seam).
#[derive(Default)]
struct Seam(AtomicBool);

impl HostCheck for Seam {
    fn verify(
        &self,
        sock: &std::os::unix::net::UnixStream,
        state: &Path,
    ) -> Result<(), HostRefused> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            ChromeHostCheck.verify(sock, state)
        }
    }
}

fn frame(n: usize) -> Vec<u8> {
    let index = json("frames/index.json");
    let file = common::s(&common::items(&index)[n], "file").to_owned();
    fs::read(fixtures().join("frames").join(file)).expect("frame")
}

fn options(state: &Path, det: &Det, seam: &Arc<Seam>) -> BrokerOptions {
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
    o.entropy = Arc::new(det.clone());
    o.clock = Arc::new(Fixed);
    o.host_check = Arc::clone(seam) as Arc<dyn HostCheck>;
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

async fn open(state: &Path, sock: &str) -> UnixStream {
    UnixStream::connect(state.join(sock))
        .await
        .expect("connect")
}

fn same(n: usize, got: &[u8]) {
    let want = frame(n);
    assert_eq!(
        String::from_utf8_lossy(&got[4..]),
        String::from_utf8_lossy(&want[4..]),
        "frame {n}"
    );
    assert_eq!(got, want.as_slice(), "frame {n} bytes");
}

async fn exchange(s: &mut UnixStream, send: usize, reply: usize) {
    // A refusal sent before the request is read can close the socket first (EPIPE).
    let _ = s.write_all(&frame(send)).await;
    same(reply, &recv(s).await);
}

async fn once(state: &Path, sock: &str, n: usize) {
    let mut s = open(state, sock).await;
    exchange(&mut s, n, n + 1).await;
}

/// A fake worker: each broker command must equal the captured one; answer with the captured reply.
async fn worker(mut s: UnixStream, script: Vec<(usize, usize)>, done: Arc<Mutex<usize>>) {
    for (cmd, reply) in script {
        same(cmd, &recv(&mut s).await);
        s.write_all(&frame(reply)).await.expect("worker reply");
        *done.lock().expect("lock") += 1;
    }
    let mut rest = Vec::new();
    let _ = s.read_to_end(&mut rest).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn captured_exchanges_replay_byte_identically() {
    let dir = state_dir();
    let state = dir.path();
    let det = Det::default();
    let seam = Arc::new(Seam::default());
    let broker = Broker::start(options(state, &det, &seam)).expect("start");

    // The real Chrome-host check refuses this test process, then the seam opens.
    once(state, "enrollment.sock", 0).await;
    seam.0.store(true, Ordering::SeqCst);
    for n in [2, 4, 6, 8, 10, 12, 14] {
        once(state, "enrollment.sock", n).await;
    }
    for n in [16, 18, 20] {
        once(state, "service.sock", n).await;
    }
    let mut s = open(state, "service.sock").await;
    exchange(&mut s, 22, 23).await;
    exchange(&mut s, 24, 25).await;
    drop(s);
    once(state, "agent.sock", 26).await;

    // The paired worker: the client drew its hello nonce from the shared stream.
    det.bytes(32);
    let mut browser = open(state, "service.sock").await;
    exchange(&mut browser, 28, 29).await;
    exchange(&mut browser, 30, 31).await;
    let done = Arc::new(Mutex::new(0));
    let work = tokio::spawn(worker(
        browser,
        vec![(43, 44), (47, 48), (64, 65)],
        Arc::clone(&done),
    ));

    once(state, "enrollment.sock", 32).await;
    once(state, "enrollment.sock", 34).await;
    once(state, "agent.sock", 36).await;
    once(state, "agent.sock", 38).await;
    let mut agent = open(state, "agent.sock").await;
    exchange(&mut agent, 40, 41).await;
    exchange(&mut agent, 42, 45).await;
    exchange(&mut agent, 46, 49).await;
    exchange(&mut agent, 50, 51).await;
    for n in [52, 54, 56, 58, 60, 62] {
        once(state, "operator.sock", n).await;
    }
    for _ in 0..100 {
        if *done.lock().expect("lock") == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        *done.lock().expect("lock"),
        3,
        "grant_end reached the worker"
    );
    drop(agent);
    tokio::time::sleep(Duration::from_millis(50)).await;
    once(state, "enrollment.sock", 66).await;
    tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .expect("worker ends")
        .expect("worker");
    broker.close().await;

    let audit = fs::read_to_string(state.join("audit.jsonl")).expect("audit");
    let want = fs::read_to_string(fixtures().join("audit.jsonl")).expect("fixture audit");
    assert_eq!(audit, want, "audit.jsonl");
    assert_eq!(
        rows(&state.join("registry.sqlite3")),
        rows(&fixtures().join("registry.sqlite3"))
    );
    for sock in SOCKETS {
        assert!(!state.join(sock).exists(), "{sock} unlinked on close");
    }
}

fn rows(db: &Path) -> Vec<(String, String, String)> {
    let conn = Connection::open(db).expect("open");
    let mut stmt = conn
        .prepare("SELECT profile, email, browser_key FROM pairings ORDER BY profile")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query");
    rows.collect::<Result<_, _>>().expect("rows")
}

#[tokio::test]
async fn broker_sockets_have_python_paths_and_modes() {
    let dir = state_dir();
    let det = Det::default();
    let broker = Broker::start(options(dir.path(), &det, &Arc::default())).expect("start");
    for sock in SOCKETS {
        let meta = fs::symlink_metadata(dir.path().join(sock)).expect(sock);
        assert!(meta.file_type().is_socket(), "{sock}");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{sock}");
    }
    let lock = fs::metadata(dir.path().join("service.lock")).expect("lock");
    assert!(lock.is_file());
    let db = fs::metadata(dir.path().join("registry.sqlite3")).expect("db");
    assert_eq!(db.permissions().mode() & 0o777, 0o600);
    broker.close().await;
}

#[tokio::test]
async fn a_second_broker_is_refused_by_the_lock() {
    let dir = state_dir();
    let det = Det::default();
    let first = Broker::start(options(dir.path(), &det, &Arc::default())).expect("start");
    assert!(Broker::start(options(dir.path(), &det, &Arc::default())).is_err());
    first.close().await;
    let again = Broker::start(options(dir.path(), &det, &Arc::default())).expect("restart");
    again.close().await;
}

#[tokio::test]
async fn a_non_socket_at_a_socket_path_is_refused() {
    let dir = state_dir();
    fs::write(dir.path().join("agent.sock"), b"x").expect("file");
    let det = Det::default();
    let err = Broker::start(options(dir.path(), &det, &Arc::default())).err();
    assert!(err.is_some_and(|e| e.to_string().contains("socket path is occupied")));
}

#[tokio::test]
async fn operator_revoke_retires_a_legacy_route() {
    let dir = state_dir();
    let state = dir.path();
    let det = Det::default();
    let seam = Arc::new(Seam::default());
    let broker = Broker::start(options(state, &det, &seam)).expect("start");
    {
        let vault_store = MemoryKeyStore::new();
        let account = fs::canonicalize(state)
            .expect("canon")
            .to_string_lossy()
            .into_owned();
        vault_store
            .set(&fixture_service(), &account, &fixture_key())
            .expect("key");
        let vault = trusty_browser::vault::Vault::with_service(
            state,
            &fixture_service(),
            Box::new(vault_store),
        )
        .expect("vault");
        let profile = "00000000-0000-4000-8000-000000000002";
        let sealed = vault
            .seal(&format!("browser:{profile}"), &"a".repeat(64))
            .expect("seal");
        let conn = Connection::open(state.join("registry.sqlite3")).expect("db");
        conn.execute(
            "INSERT INTO routes VALUES (?,?,?,?,?,?,?)",
            [
                profile,
                "w@example.test",
                "wf-1",
                "https://example.test/",
                "acct",
                &sealed,
                "hash",
            ],
        )
        .expect("route");
    }
    let mut s = open(state, "operator.sock").await;
    let body = br#"{"op":"revoke","workflow":"wf-1"}"#;
    s.write_all(&(body.len() as u32).to_ne_bytes())
        .await
        .expect("head");
    s.write_all(body).await.expect("body");
    let reply = recv(&mut s).await;
    assert_eq!(&reply[4..], br#"{"ok":true}"#);
    broker.close().await;
    let conn = Connection::open(state.join("registry.sqlite3")).expect("db");
    let left: i64 = conn
        .query_row("SELECT count(*) FROM routes", [], |r| r.get(0))
        .expect("count");
    let revoked: i64 = conn
        .query_row("SELECT count(*) FROM revoked", [], |r| r.get(0))
        .expect("count");
    assert_eq!((left, revoked), (0, 1));
}
