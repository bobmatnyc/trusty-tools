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
    let ack = call(&mut s, &format!(r#"{{"type":"proof","proof":"{proof}"}}"#)).await;
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
