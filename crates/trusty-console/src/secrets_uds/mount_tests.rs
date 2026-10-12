//! The secrets routes as the console mounts and serves them (#9066).
//!
//! `tests.rs` drives the secrets sub-router alone; these drive the real
//! router (`build_router`), the tailnet serve path (`serve_tailnet`) and the
//! loopback serve path (`serve_listeners`), where the pid lookup starts at
//! accept.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt as _;
use trusty_secrets::ancestry::{OsProcessTable, ProcessError, ProcessTable, StartTime};

use super::dial::SecretsDial;
use super::errors::Refusal;
use super::pid_map::{lookups, tests::spawn_client};
use super::tests::{
    BRIDGE_REGISTRY, LIST_BODY, OWN, Seen, ok_stub, own_request, registry_listing, text, token,
};
use super::{CSRF_HEADER, SecretsBridge};
use crate::server::{AppState, build_router};
use crate::tailnet_peer::{
    OwnMagicDns, PeerIdentity, PeerResolver, TailnetPeerGate, WhoisError, serve_listeners,
    serve_tailnet,
};

/// A bridge to the stub at `socket`, owning `own` and judging with `table`.
fn bridge(
    socket: &std::path::Path,
    own: SocketAddr,
    table: Arc<dyn ProcessTable>,
) -> SecretsBridge {
    let dir = socket.parent().expect("socket dir");
    let bridge = SecretsBridge::new(SecretsDial::new(socket.to_path_buf()).with_external(true))
        .expect("mint")
        .with_table(table)
        .with_registry(Some(registry_listing(dir, BRIDGE_REGISTRY, &["/p"])));
    bridge.set_authorities(&[own]);
    bridge
}

/// Every tailnet peer is the host's own login, so the peer gate admits it.
struct AnyOwner;

impl PeerResolver for AnyOwner {
    fn whois(&self, _ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        Box::pin(async {
            Ok(PeerIdentity {
                login: "owner@example.com".to_owned(),
                tagged: false,
                node_name: None,
            })
        })
    }
}

/// A raw `POST` from the console's own page served at `addr`.
fn raw_post(addr: SocketAddr, csrf: &str, path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\n\
         {CSRF_HEADER}: {csrf}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Send `request` to `addr`; the status code and the whole response.
async fn exchange(addr: SocketAddr, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let code = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {text:?}"));
    (code, text)
}

// ─── T3 through the real router ──────────────────────────────────────────────

#[tokio::test]
async fn the_real_router_sends_no_cors_header_on_secrets_routes() {
    let (_tmp, socket, seen) = ok_stub();
    let own: SocketAddr = OWN.parse().expect("addr");
    let state =
        AppState::new(Vec::new()).with_secrets(bridge(&socket, own, Arc::new(OsProcessTable)));
    let secrets = state.secrets();
    let app = build_router(state);
    let request = |method: &str, path: &str, origin: Option<&str>| {
        let mut request = own_request(&secrets, path, LIST_BODY);
        *request.method_mut() = method.parse().expect("method");
        request.headers_mut().remove("origin");
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().expect("origin"));
        }
        request
            .headers_mut()
            .insert("access-control-request-method", "POST".parse().expect("hv"));
        request
    };
    let own_origin = format!("http://{OWN}");
    let cases = [
        (
            "POST",
            "/api/secrets/list",
            Some(own_origin.as_str()),
            StatusCode::OK,
        ),
        ("POST", "/api/secrets/list", None, StatusCode::FORBIDDEN),
        (
            "POST",
            "/api/secrets/list",
            Some("http://evil.example"),
            StatusCode::FORBIDDEN,
        ),
        (
            "OPTIONS",
            "/api/secrets/list",
            Some(own_origin.as_str()),
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/secrets/list",
            Some(own_origin.as_str()),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "POST",
            "/api/secrets/resolve",
            Some(own_origin.as_str()),
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            "POST",
            "/api/secrets/no-such",
            Some(own_origin.as_str()),
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            "/api/secrets/a/b",
            Some(own_origin.as_str()),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (method, path, origin, expected) in cases {
        let response = app
            .clone()
            .oneshot(request(method, path, origin))
            .await
            .expect("response");
        assert_eq!(response.status(), expected, "{method} {path} {origin:?}");
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin"),
            "{method} {path} {origin:?} must carry no CORS header"
        );
    }
    // #9066 finding 3: an unknown secrets path is the bridge's 404, not the proxy's.
    let response = app
        .clone()
        .oneshot(request("POST", "/api/secrets/no-such", Some(&own_origin)))
        .await
        .expect("response");
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    assert!(String::from_utf8_lossy(&body).contains(text(Refusal::UnknownRoute)));
    assert_eq!(
        seen.lock().expect("lock").len(),
        1,
        "only the admitted list"
    );

    let console = Request::builder()
        .uri("/api/console/services")
        .header("host", OWN)
        .header("origin", "http://evil.example")
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(console).await.expect("response");
    // #9792: the permissive CORS layer is gone from /api/console/* too.
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin"),
        "/api/console/* must carry no CORS header"
    );
}

// ─── A2 through serve_tailnet ────────────────────────────────────────────────

/// A process table that counts its reads and answers none of them.
#[derive(Default)]
struct Counting(AtomicUsize);

impl ProcessTable for Counting {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProcessError::Unreadable { pid })
    }
    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProcessError::Unreadable { pid })
    }
}

#[tokio::test]
async fn serve_tailnet_refuses_every_secrets_route_before_any_lookup() {
    let (_tmp, socket, seen) = ok_stub();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let table = Arc::new(Counting::default());
    let state = AppState::new(Vec::new()).with_secrets(bridge(
        &socket,
        addr,
        Arc::clone(&table) as Arc<dyn ProcessTable>,
    ));
    let csrf = token(&state.secrets());
    let gate = Arc::new(TailnetPeerGate::new(Arc::new(AnyOwner), addr.ip()));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_tailnet(
        listener,
        build_router(state),
        gate,
        async move {
            let _ = stopped.await;
        },
    ));
    for path in [
        "/api/secrets/session",
        // #9067: the picker routes are refused on the tailnet too.
        "/api/secrets/projects",
        "/api/secrets/scopes",
        "/api/secrets/list",
        "/api/secrets/set",
        "/api/secrets/delete",
        "/api/secrets/copy",
        "/api/secrets/agents_may_use",
        "/api/secrets/resolve",
        "/api/secrets/x",
    ] {
        let (code, response) = exchange(addr, &raw_post(addr, &csrf, path, LIST_BODY)).await;
        assert_eq!(code, 403, "{path}: {response}");
        assert!(response.contains(text(Refusal::Tailnet)), "{path}");
    }
    assert!(seen.lock().expect("lock").is_empty(), "no socket call");
    assert_eq!(table.0.load(Ordering::SeqCst), 0, "no process-table read");
    assert_eq!(lookups::count(addr), 0, "no pid lookup");
    let _ = stop.send(());
}

// ─── A9 through serve_listeners: the pid is read at accept ───────────────────

/// How the table answers for the child at request time.
const AGENT: u8 = 0;
const NOT_AGENT: u8 = 1;
const RESTARTED: u8 = 2;
const GONE: u8 = 3;

/// The OS table, except the child's agent flag, parent and start time.
#[derive(Default)]
struct ChildTable {
    child: AtomicU32,
    mode: AtomicU8,
}

impl ChildTable {
    fn is_child(&self, pid: u32) -> bool {
        pid == self.child.load(Ordering::SeqCst)
    }
}

impl ProcessTable for ChildTable {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        if self.is_child(pid) {
            Ok(1)
        } else {
            OsProcessTable.parent(pid)
        }
    }
    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        match (self.is_child(pid), self.mode.load(Ordering::SeqCst)) {
            (true, RESTARTED) => Ok(StartTime::from_raw(u64::MAX - 1)),
            (true, GONE) => Err(ProcessError::Unreadable { pid }),
            _ => OsProcessTable.start_time(pid),
        }
    }
    fn is_agent(&self, pid: u32) -> Result<bool, ProcessError> {
        if self.is_child(pid) {
            Ok(self.mode.load(Ordering::SeqCst) == AGENT)
        } else {
            OsProcessTable.is_agent(pid)
        }
    }
}

/// Run one child client to completion; the assertion the socket saw.
async fn child_assertion(
    addr: SocketAddr,
    request: &str,
    table: &ChildTable,
    seen: &Seen,
) -> Value {
    let before = seen.lock().expect("lock").len();
    let mut child = spawn_client(addr, Some(request));
    table.child.store(child.id(), Ordering::SeqCst);
    for _ in 0..300 {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), before + 1, "one call per request");
    seen[before]["params"]["bridge"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loopback_request_is_judged_from_the_pid_found_at_accept() {
    let (tmp, socket, seen) = ok_stub();
    let table = Arc::new(ChildTable::default());
    let secrets = SecretsBridge::new(SecretsDial::new(socket).with_external(true))
        .expect("mint")
        .with_table(Arc::clone(&table) as Arc<dyn ProcessTable>)
        .with_registry(Some(registry_listing(tmp.path(), BRIDGE_REGISTRY, &["/p"])));
    let state = AppState::new(Vec::new()).with_secrets(secrets);
    let bridge = state.secrets();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let listeners = serve_listeners(
        &["127.0.0.1:0".parse().expect("addr")],
        &build_router(state),
        Arc::new(AnyOwner),
        &OwnMagicDns::default(),
        move || {
            let mut stopped = stopped.clone();
            async move {
                let _ = stopped.wait_for(|s| *s).await;
            }
        },
    )
    .await
    .expect("serve");
    let addr = listeners.bound[0];
    bridge.set_authorities(&listeners.bound);
    let request = raw_post(addr, &token(&bridge), "/api/secrets/list", LIST_BODY);
    let cases = [
        (AGENT, json!({ "agent_parent": true, "source": "mapped" })),
        (
            NOT_AGENT,
            json!({ "agent_parent": false, "source": "mapped" }),
        ),
        (
            RESTARTED,
            json!({ "agent_parent": true, "source": "unreadable" }),
        ),
        (
            GONE,
            json!({ "agent_parent": true, "source": "unreadable" }),
        ),
    ];
    for (mode, expected) in cases {
        table.mode.store(mode, Ordering::SeqCst);
        let sent = child_assertion(addr, &request, &table, &seen).await;
        assert_eq!(sent, expected, "mode {mode}");
    }
    assert!(lookups::count(addr) >= 4, "looked up at accept");
    let _ = stop.send(true);
}

// ─── #9066 F1: the lookup never runs in the accept loop ──────────────────────

/// How long each lookup on the listener under test sleeps.
const SLOW_LOOKUP: Duration = Duration::from_millis(600);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_lookup_delays_no_accept_and_no_other_route() {
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let listeners = serve_listeners(
        &["127.0.0.1:0".parse().expect("addr")],
        &build_router(AppState::new(Vec::new())),
        Arc::new(AnyOwner),
        &OwnMagicDns::default(),
        move || {
            let mut stopped = stopped.clone();
            async move {
                let _ = stopped.wait_for(|s| *s).await;
            }
        },
    )
    .await
    .expect("serve");
    let addr = listeners.bound[0];
    lookups::slow_down(addr, SLOW_LOOKUP);
    let health = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let started = Instant::now();
    let (first, second) = tokio::join!(exchange(addr, &health), exchange(addr, &health));
    let took = started.elapsed();
    assert_eq!((first.0, second.0), (200, 200), "{first:?} {second:?}");
    assert!(
        took < SLOW_LOOKUP / 2,
        "two fresh connections took {took:?} against a {SLOW_LOOKUP:?} lookup"
    );
    // A9: the lookups still started at accept, though nothing awaited them.
    let deadline = Instant::now() + Duration::from_secs(5);
    while lookups::count(addr) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        lookups::count(addr),
        2,
        "one lookup per accepted connection"
    );
    let _ = stop.send(true);
}
