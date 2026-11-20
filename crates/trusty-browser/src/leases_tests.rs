//! Unit tests for `leases.rs`: lease expiry on an injected clock and the
//! `lease_show`/`lease_clear` command frames.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::*;
use crate::broker::{Broker, BrokerOptions};
use crate::pairing::Route;
use crate::vault::MemoryKeyStore;

fn claim(token: &str, deadline: f64, busy: bool) -> Claim {
    Claim {
        generation: "gen".into(),
        tab: 7,
        owner: "owner".into(),
        owner_session: 1,
        token: token.into(),
        deadline,
        busy,
        document: None,
        origin: None,
        indicator: false,
    }
}

async fn broker() -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let options = BrokerOptions::new(dir.path(), Box::new(MemoryKeyStore::new()));
    let broker = Broker::start(options).expect("start");
    (dir, broker)
}

#[tokio::test]
async fn an_idle_lease_expires_and_a_busy_one_does_not() {
    let (_dir, broker) = broker().await;
    let core = broker.core();
    {
        let mut inner = core.lock();
        inner.claims.push(claim("idle", 100.0 + TTL, false));
        assert!(
            inner.active("gen", 7, 100.0 + TTL - 0.001).is_some(),
            "live before the deadline"
        );
        assert!(
            inner.active("gen", 7, 100.0 + TTL).is_none(),
            "expired at the deadline"
        );
        assert!(inner.claims.is_empty(), "an expired idle lease is dropped");
        inner.claims.push(claim("busy", 100.0, true));
        assert!(
            inner.active("gen", 7, 1e9).is_some(),
            "an in-flight lease never expires"
        );
        assert!(
            inner.active("other", 7, 0.0).is_none(),
            "another generation"
        );
        inner.with_claim("busy", |c| c.busy = false);
        assert!(inner.active("gen", 7, 1e9).is_none());
    }
    broker.close().await;
}

fn route() -> Route {
    Route {
        profile: "p".into(),
        email: "a@example.test".into(),
        workflow: None,
        url: Some("https://example.test/".into()),
        account: None,
        browser_key: "k".into(),
        ticket: None,
    }
}

async fn read_json(s: &mut UnixStream) -> Json {
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.expect("head");
    let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
    s.read_exact(&mut body).await.expect("body");
    Json::parse(&body).expect("json")
}

#[tokio::test]
async fn indicator_commands_carry_the_route_and_python_reply_rules() {
    let (_dir, broker) = broker().await;
    let core = Arc::clone(broker.core());
    let (ours, mut worker) = UnixStream::pair().expect("pair");
    let (_reader, writer) = ours.into_split();
    let peer = Peer::new(writer, &route(), "inc".into(), "gen".into());
    let mut shown = claim("tok", 0.0, false);
    shown.indicator = true;
    let replies = [
        r#"{"ok":true,"email":"a@example.test","url":"https://example.test/","account":"None","tab":7,"result":1}"#,
        r#"{"ok":true,"email":"a@example.test","url":"https://other.test/","account":"None","tab":7}"#,
        r#"{"ok":false,"error":"Weird","tab":7}"#,
        r#"{"ok":true,"email":"a@example.test","url":"https://example.test/","account":"None","tab":8}"#,
    ];
    let p = Arc::clone(&peer);
    let answer = tokio::spawn(async move {
        let mut frames = Vec::new();
        for reply in replies {
            let cmd = read_json(&mut worker).await;
            let id = cmd
                .get("id")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_owned();
            let mut reply = Json::parse(reply.as_bytes()).expect("reply");
            reply.insert("id", Json::from(id.as_str()));
            assert!(p.deliver(&id, reply), "a command waits for {id}");
            frames.push(cmd);
        }
        let _ = worker.shutdown().await;
        frames
    });
    let got = indicator(&core, &peer, &shown, true).await;
    let want = r#"{"email":"a@example.test","url":"https://example.test/","account":"None","tab":7,"result":1}"#;
    assert_eq!(
        got.ok().and_then(|j| j.dumps(false).ok()).as_deref(),
        Some(want)
    );
    let code = |r: Result<Json, Refusal>| r.err().map(|e| e.code);
    assert_eq!(
        code(indicator(&core, &peer, &shown, false).await).as_deref(),
        Some("IdentityMismatch")
    );
    assert_eq!(
        code(indicator(&core, &peer, &shown, true).await).as_deref(),
        Some("Weird")
    );
    assert_eq!(
        code(indicator(&core, &peer, &shown, true).await).as_deref(),
        Some("TabMismatch")
    );
    let frames = answer.await.expect("worker");
    let keys: Vec<&str> = match &frames[0] {
        Json::Object(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(
        keys,
        [
            "id", "op", "email", "url", "account", "tab", "lease", "workflow", "ttl_ms"
        ]
    );
    assert_eq!(frames[0].get("op"), Some(&Json::from("lease_show")));
    assert_eq!(frames[1].get("op"), Some(&Json::from("lease_clear")));
    assert_eq!(frames[0].get("ttl_ms"), Some(&Json::Int(60000)));
    assert_eq!(frames[0].get("workflow"), Some(&Json::Null));
    // A code-session lease shows no indicator, so clearing it sends nothing.
    clear(&core, Some(&peer), &claim("tok", 0.0, false)).await;
    broker.close().await;
}
