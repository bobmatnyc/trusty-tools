//! Unit tests for `emulation.rs`: request checks, the reset on lease end and
//! the error arm of a failed reset (#11371).

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

use super::*;
use crate::broker::{Broker, BrokerOptions, Claim};
use crate::pairing::Route;
use crate::peer_conn::Peer;
use crate::vault::MemoryKeyStore;

async fn broker() -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let options = BrokerOptions::new(dir.path(), Box::new(MemoryKeyStore::new()));
    (dir, Broker::start(options).expect("start"))
}

fn route() -> Route {
    Route {
        profile: "p".into(),
        email: "a@example.com".into(),
        workflow: None,
        url: None,
        account: None,
        browser_key: "k".into(),
        ticket: None,
    }
}

fn entry(token: &str) -> Emulated {
    Emulated {
        generation: "gen".into(),
        tab: 7,
        grant: "g".into(),
        token: token.into(),
        resetting: false,
        failures: 0,
    }
}

fn claim(token: &str, deadline: f64) -> Claim {
    Claim {
        generation: "gen".into(),
        tab: 7,
        owner: "owner".into(),
        owner_session: 1,
        token: token.into(),
        deadline,
        busy: false,
        document: None,
        origin: None,
        indicator: false,
    }
}

async fn read_json(s: &mut UnixStream) -> Json {
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.expect("head");
    let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
    s.read_exact(&mut body).await.expect("body");
    Json::parse(&body).expect("json")
}

/// A connected worker of generation `gen` that answers each command with the next reply.
fn worker(core: &Arc<Core>, replies: Vec<&'static str>) -> tokio::task::JoinHandle<Vec<Json>> {
    let (ours, mut theirs) = UnixStream::pair().expect("pair");
    let (_reader, writer) = ours.into_split();
    let peer = Peer::new(writer, &route(), "inc".into(), "gen".into());
    core.lock().peers.push(Arc::clone(&peer));
    tokio::spawn(async move {
        let _keep = _reader;
        let mut frames = Vec::new();
        for reply in replies {
            let cmd = read_json(&mut theirs).await;
            let id = cmd
                .get("id")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_owned();
            let mut reply = Json::parse(reply.as_bytes()).expect("reply");
            reply.insert("id", Json::from(id.as_str()));
            assert!(peer.deliver(&id, reply), "a command waits for {id}");
            frames.push(cmd);
        }
        frames
    })
}

const RESET_OK: &str = r#"{"ok":true,"email":"a@example.com","tab":7,"result":{"javascript":true,"color_scheme":"none","width":0,"height":0}}"#;

#[test]
fn emulation_requests_are_validated() {
    let frame = |text: &str| Json::parse(text.as_bytes()).expect("json");
    let ok = |text: &str| crate::pages::validate(&frame(text)).is_ok();
    assert!(ok(
        r#"{"op":"page_javascript","lease":"l","enabled":false}"#
    ));
    assert!(!ok(
        r#"{"op":"page_javascript","lease":"l","enabled":"no"}"#
    ));
    assert!(!ok(r#"{"op":"page_javascript","lease":"l"}"#));
    for scheme in ["light", "dark", "none"] {
        let text = format!(r#"{{"op":"page_color_scheme","lease":"l","scheme":"{scheme}"}}"#);
        assert!(ok(&text), "{scheme}");
    }
    assert!(!ok(
        r#"{"op":"page_color_scheme","lease":"l","scheme":"sepia"}"#
    ));
    for (w, h, want) in [
        (1440, 900, true),
        (0, 0, true),
        (200, 200, true),
        (7680, 4320, true),
        (199, 900, false),
        (1440, 4321, false),
        (0, 900, false),
        (-1, -1, false),
    ] {
        let text = format!(r#"{{"op":"page_viewport","lease":"l","width":{w},"height":{h}}}"#);
        assert_eq!(ok(&text), want, "{w}x{h}");
    }
    assert!(!ok(
        r#"{"op":"page_viewport","lease":"l","width":1440.0,"height":900}"#
    ));
    assert!(
        crate::pages::validate(&frame(r#"{"op":"page_emulation_reset","tab":7}"#)).is_err(),
        "the reset is broker-only"
    );
}

#[tokio::test]
async fn a_lease_end_resets_an_emulated_tab() {
    let (_dir, broker) = broker().await;
    let core = Arc::clone(broker.core());
    let far = core.lease_clock.now() + 1e6;
    {
        let mut inner = core.lock();
        inner.claims.push(claim("live", far));
        inner.emulated.push(entry("live"));
    }
    let answers = worker(&core, vec![RESET_OK]);
    reap(&core).await;
    assert_eq!(
        core.lock().emulated.len(),
        1,
        "a live lease keeps its emulation"
    );
    core.lock().drop_claim("live");
    reap(&core).await;
    assert!(core.lock().emulated.is_empty(), "an ended lease is reset");
    let frames = answers.await.expect("worker");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].get("op"), Some(&Json::from(RESET_OP)));
    assert_eq!(frames[0].get("tab"), Some(&Json::Int(7)));
    assert_eq!(frames[0].get("grant"), Some(&Json::from("g")));
    broker.close().await;
}

#[tokio::test]
async fn a_failed_reset_keeps_the_lease_and_refuses_the_next_select() {
    let (_dir, broker) = broker().await;
    let core = Arc::clone(broker.core());
    let far = core.lease_clock.now() + 1e6;
    {
        let mut inner = core.lock();
        inner.claims.push(claim("held", far));
        inner.emulated.push(entry("held"));
    }
    let refused = r#"{"ok":false,"error":"Weird","tab":7}"#;
    let answers = worker(&core, vec![refused, refused, RESET_OK]);
    // `release` runs `reset_lease` first and drops the lease only on Ok.
    let code = reset_lease(&core, "held").await.err().map(|r| r.code);
    assert_eq!(code.as_deref(), Some("BrowserRefused"));
    {
        let inner = core.lock();
        assert!(inner.claims.iter().any(|c| c.token == "held"), "lease kept");
        let pending = inner.emulated.first().expect("still pending");
        assert_eq!((pending.failures, pending.resetting), (1, false));
    }
    // The lease ends another way; a new lease on the tab is refused until a reset runs.
    core.lock().drop_claim("held");
    let code = settle(&core, "gen", 7).await.err().map(|r| r.code);
    assert_eq!(code.as_deref(), Some("BrowserRefused"));
    assert_eq!(core.lock().emulated.first().map(|e| e.failures), Some(2));
    assert!(settle(&core, "gen", 7).await.is_ok(), "the retry resets it");
    assert!(core.lock().emulated.is_empty());
    assert_eq!(answers.await.expect("worker").len(), 3);
    broker.close().await;
}

#[tokio::test]
async fn a_reset_with_no_worker_connection_is_settled() {
    let (_dir, broker) = broker().await;
    let core = Arc::clone(broker.core());
    core.lock().emulated.push(entry("gone"));
    assert!(reset(&core, "gen", 7).await.is_ok());
    assert!(
        core.lock().emulated.is_empty(),
        "the extension resets on disconnect"
    );
    broker.close().await;
}
