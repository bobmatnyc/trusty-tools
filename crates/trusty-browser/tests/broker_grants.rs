//! Grant lifecycle on a live broker: reconnect grace and rebinding (`grants.py`).

mod common;

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{fixture_key, fixture_service, state_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use trusty_browser::broker::{Broker, BrokerOptions, HostCheck};
use trusty_browser::entropy::Monotonic;
use trusty_browser::peer_darwin::HostRefused;
use trusty_browser::proof::{Hello, Role, pairing_proof};
use trusty_browser::pyjson::Json;
use trusty_browser::vault::{KeyStore, MemoryKeyStore};

const PROFILE: &str = "0dcdaf05-6de3-4872-9ad0-186105244c23";
const EMAIL: &str = "alpha@example.test";
const KEY: &str = "bc1755f18c664ce836300569acb31692fca2cbfa21aeb25edf99aa8127951418";
const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone)]
struct Manual(Arc<Mutex<f64>>);

impl Monotonic for Manual {
    fn now(&self) -> f64 {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Open;

impl HostCheck for Open {
    fn verify(
        &self,
        _: &std::os::unix::net::UnixStream,
        _: &std::path::Path,
    ) -> Result<(), HostRefused> {
        Ok(())
    }
}

async fn call(s: &mut UnixStream, body: &str) -> Json {
    s.write_all(&(body.len() as u32).to_ne_bytes())
        .await
        .expect("head");
    s.write_all(body.as_bytes()).await.expect("body");
    recv(s).await
}

async fn recv(s: &mut UnixStream) -> Json {
    let mut head = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut head))
        .await
        .expect("in time")
        .expect("reply");
    let mut reply = vec![0u8; u32::from_ne_bytes(head) as usize];
    s.read_exact(&mut reply).await.expect("reply body");
    Json::parse(&reply).expect("json")
}

async fn once(state: &std::path::Path, sock: &str, body: &str) -> Json {
    let mut s = UnixStream::connect(state.join(sock))
        .await
        .expect("connect");
    call(&mut s, body).await
}

async fn handshake(state: &std::path::Path, incarnation: &str) -> UnixStream {
    let mut s = UnixStream::connect(state.join("service.sock"))
        .await
        .expect("connect");
    let hello = format!(
        r#"{{"type":"hello","profile":"{PROFILE}","email":"{EMAIL}","incarnation":"{incarnation}","nonce":"{NONCE}"}}"#
    );
    let challenge = call(&mut s, &hello).await;
    let server = challenge
        .get("nonce")
        .and_then(Json::as_str)
        .expect("nonce")
        .to_owned();
    let fields = Hello {
        profile: PROFILE,
        email: EMAIL,
        incarnation,
        nonce: NONCE,
    };
    let proof = pairing_proof(KEY, Role::Extension, &fields, &server).expect("proof");
    let mut ack = call(&mut s, &format!(r#"{{"type":"proof","proof":"{proof}"}}"#)).await;
    // A grant the new incarnation may not rebind is ended with a `grant_end` frame first.
    if ack.get("op").and_then(Json::as_str) == Some("grant_end") {
        ack = recv(&mut s).await;
    }
    assert_eq!(ack.get("ok"), Some(&Json::Bool(true)), "{ack:?}");
    s
}

fn grant_count(reply: &Json) -> usize {
    match reply.get("profiles") {
        Some(Json::Array(list)) => list.len(),
        _ => usize::MAX,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grace_expiry_ends_a_grant_whose_worker_never_returns() {
    let dir = state_dir();
    let state = dir.path();
    let store = MemoryKeyStore::new();
    let account = fs::canonicalize(state)
        .expect("canon")
        .to_string_lossy()
        .into_owned();
    store
        .set(&fixture_service(), &account, &fixture_key())
        .expect("key");
    let clock = Manual(Arc::new(Mutex::new(1000.0)));
    let mut options = BrokerOptions::new(state, Box::new(store));
    options.service = fixture_service();
    options.clock = Arc::new(clock.clone());
    options.host_check = Arc::new(Open);
    let broker = Broker::start(options).expect("start");
    let cred = format!(r#""profile":"{PROFILE}","email":"{EMAIL}","key":"{KEY}""#);
    let paired = once(
        state,
        "enrollment.sock",
        &format!(r#"{{"op":"pair",{cred}}}"#),
    )
    .await;
    assert_eq!(paired.get("ok"), Some(&Json::Bool(true)));

    let worker = handshake(state, "inc-1").await;
    let connected = once(
        state,
        "enrollment.sock",
        &format!(r#"{{"op":"connect",{cred}}}"#),
    )
    .await;
    assert_eq!(
        connected.get("ok"),
        Some(&Json::Bool(true)),
        "{connected:?}"
    );
    drop(worker);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Inside the grace the grant stays listed; a same-incarnation worker rebinds it.
    *clock.0.lock().expect("clock") = 1040.0;
    assert_eq!(
        grant_count(&once(state, "agent.sock", r#"{"op":"profiles"}"#).await),
        1
    );
    let worker = handshake(state, "inc-1").await;
    drop(worker);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The second loss restarts the grace from 1040; at 1085 it has run out.
    *clock.0.lock().expect("clock") = 1084.0;
    assert_eq!(
        grant_count(&once(state, "agent.sock", r#"{"op":"profiles"}"#).await),
        1
    );
    *clock.0.lock().expect("clock") = 1085.0;
    assert_eq!(
        grant_count(&once(state, "agent.sock", r#"{"op":"profiles"}"#).await),
        0
    );
    broker.close().await;
    let audit = fs::read_to_string(state.join("audit.jsonl")).expect("audit");
    assert!(audit.contains(r#""event":"grant_reuse""#), "{audit}");
    assert!(audit.contains(r#""outcome":"browser_gone""#), "{audit}");
}

fn start(state: &std::path::Path, clock: &Manual) -> Broker {
    let store = MemoryKeyStore::new();
    let account = fs::canonicalize(state)
        .expect("canon")
        .to_string_lossy()
        .into_owned();
    store
        .set(&fixture_service(), &account, &fixture_key())
        .expect("key");
    let mut options = BrokerOptions::new(state, Box::new(store));
    options.service = fixture_service();
    options.clock = Arc::new(clock.clone());
    options.host_check = Arc::new(Open);
    Broker::start(options).expect("start")
}

fn cred() -> String {
    format!(r#""profile":"{PROFILE}","email":"{EMAIL}","key":"{KEY}""#)
}

/// Pair, attach a worker and connect; returns the grant id. The worker is dropped.
async fn connected(state: &std::path::Path) -> String {
    let c = cred();
    let paired = once(state, "enrollment.sock", &format!(r#"{{"op":"pair",{c}}}"#)).await;
    assert_eq!(paired.get("ok"), Some(&Json::Bool(true)));
    let worker = handshake(state, "inc-1").await;
    let reply = once(
        state,
        "enrollment.sock",
        &format!(r#"{{"op":"connect",{c}}}"#),
    )
    .await;
    drop(worker);
    reply
        .get("grant")
        .and_then(Json::as_str)
        .expect("grant")
        .to_owned()
}

async fn listed(state: &std::path::Path) -> usize {
    grant_count(&once(state, "agent.sock", r#"{"op":"profiles"}"#).await)
}

/// #11528: a restart keeps the grant; the stored incarnation and the grace still decide its fate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_keeps_a_grant_until_its_grace_ends() {
    let dir = state_dir();
    let state = dir.path();
    let clock = Manual(Arc::new(Mutex::new(1000.0)));
    let first = start(state, &clock);
    let grant = connected(state).await;
    first.close().await;

    let stored = fs::read_to_string(state.join("grants.json")).expect("store");
    assert!(stored.contains(&grant), "{stored}");
    assert!(!stored.contains(KEY), "the store must not hold the key");

    let second = start(state, &clock);
    assert_eq!(listed(state).await, 1, "grant must survive the restart");
    // The same incarnation rebinds it inside the grace.
    *clock.0.lock().expect("clock") = 1030.0;
    let worker = handshake(state, "inc-1").await;
    assert_eq!(listed(state).await, 1);
    drop(worker);
    second.close().await;

    // A new worker incarnation after a restart ends the grant.
    let third = start(state, &clock);
    let worker = handshake(state, "inc-2").await;
    assert_eq!(
        listed(state).await,
        0,
        "a different incarnation must not rebind"
    );
    drop(worker);
    third.close().await;
    // And the ended grant does not come back on the next start.
    let fourth = start(state, &clock);
    assert_eq!(listed(state).await, 0, "the ended grant must not come back");
    fourth.close().await;
}

/// #11528: no grant outlives its revocation, even across a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_grant_does_not_return_after_a_restart() {
    let dir = state_dir();
    let state = dir.path();
    let clock = Manual(Arc::new(Mutex::new(1000.0)));
    let first = start(state, &clock);
    let grant = connected(state).await;
    let revoked = once(
        state,
        "operator.sock",
        &format!(r#"{{"op":"revoke_grant","grant":"{grant}"}}"#),
    )
    .await;
    assert_eq!(revoked.get("ok"), Some(&Json::Bool(true)), "{revoked:?}");
    first.close().await;
    let second = start(state, &clock);
    assert_eq!(listed(state).await, 0);
    second.close().await;
}

/// #11529: a refused hello carries the published asset stamp, so a stale worker that never
/// reaches the acknowledgement can still reload itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_handshake_carries_the_published_asset_stamp() {
    let dir = state_dir();
    let state = dir.path();
    let clock = Manual(Arc::new(Mutex::new(1000.0)));
    let broker = start(state, &clock);
    let c = cred();
    let paired = once(state, "enrollment.sock", &format!(r#"{{"op":"pair",{c}}}"#)).await;
    assert_eq!(paired.get("ok"), Some(&Json::Bool(true)));
    trusty_browser::assets::extension_path(state).expect("publish");
    let stamp = trusty_browser::assets::published(state).expect("stamp");

    let mut s = UnixStream::connect(state.join("service.sock"))
        .await
        .expect("connect");
    let hello = format!(
        r#"{{"type":"hello","profile":"{PROFILE}","email":"{EMAIL}","incarnation":"inc-1","nonce":"{NONCE}"}}"#
    );
    let challenge = call(&mut s, &hello).await;
    assert_eq!(
        challenge.get("type").and_then(Json::as_str),
        Some("challenge")
    );
    let bad = "0".repeat(64);
    let refusal = call(&mut s, &format!(r#"{{"type":"proof","proof":"{bad}"}}"#)).await;
    assert_eq!(refusal.get("ok"), Some(&Json::Bool(false)), "{refusal:?}");
    assert_eq!(
        refusal.get("assets").and_then(Json::as_str),
        Some(stamp.as_str()),
        "{refusal:?}"
    );
    broker.close().await;
}
