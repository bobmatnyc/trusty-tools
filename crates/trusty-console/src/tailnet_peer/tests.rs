//! Tests for the tailnet peer gate (#9035).
//!
//! Every test injects a scripted [`PeerResolver`]; none needs a real tailnet.
//! The `serve_tailnet` cases bind a real loopback socket so the `ConnectInfo`
//! wiring the guard depends on is exercised, not simulated. The peer address of
//! such a connection is `127.0.0.1`; the host's tailnet address is a fixed
//! CGNAT address the resolver script answers for.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::whois::parse_whois_json;
use super::*;

const HOST_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1));
const LOOPBACK_PEER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const OWNER: &str = "owner@example.com";

/// What the scripted resolver answers for one address.
#[derive(Clone)]
enum Answer {
    Login(&'static str),
    Tagged(&'static str),
    Fail,
    Hang,
}

/// A resolver answering from a fixed table, counting every call.
struct Scripted {
    answers: HashMap<IpAddr, Answer>,
    calls: AtomicUsize,
    delay: Duration,
}

impl Scripted {
    fn new(answers: &[(IpAddr, Answer)]) -> Arc<Self> {
        Self::with_delay(answers, Duration::ZERO)
    }

    fn with_delay(answers: &[(IpAddr, Answer)], delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            answers: answers.iter().cloned().collect(),
            calls: AtomicUsize::new(0),
            delay,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PeerResolver for Scripted {
    fn whois(&self, ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answers.get(&ip).cloned();
        let delay = self.delay;
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let id = |login: &str, tagged| PeerIdentity {
                login: login.to_owned(),
                tagged,
            };
            match answer {
                Some(Answer::Login(l)) => Ok(id(l, false)),
                Some(Answer::Tagged(l)) => Ok(id(l, true)),
                Some(Answer::Hang) => std::future::pending().await,
                Some(Answer::Fail) | None => Err(WhoisError::Exit {
                    status: "exit status: 1".to_owned(),
                    stderr: "tailscaled is not running".to_owned(),
                }),
            }
        })
    }
}

fn gate(resolver: Arc<Scripted>) -> Arc<TailnetPeerGate> {
    Arc::new(TailnetPeerGate::new(resolver, HOST_IP))
}

fn console_router() -> Router {
    crate::server::build_router(crate::server::AppState::new(Vec::new()))
}

/// Serve `router` on a loopback socket through `serve_tailnet`, the production
/// tailnet serve path. Dropping the returned sender stops it.
async fn spawn_tailnet(
    router: Router,
    gate: Arc<TailnetPeerGate>,
) -> (SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(serve_tailnet(listener, router, gate, async move {
        let _ = rx.await;
    }));
    (addr, tx)
}

/// Send one raw HTTP/1.1 request and return the response status code.
async fn status_of(addr: SocketAddr, method: &str, path: &str, origin: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 0\r\n"
    );
    if let Some(origin) = origin {
        req.push_str(&format!("Origin: {origin}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("read");
    let head = String::from_utf8_lossy(&buf);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in response: {head:?}"))
}

/// Every method/route shape the foreign-peer cases probe: reads, writes, a
/// CORS preflight and an unrouted path, each with and without an `Origin`.
const PROBES: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/api/console/services"),
    ("POST", "/api/console/sessions"),
    ("DELETE", "/api/console/memory/palaces/p1"),
    ("OPTIONS", "/api/console/services"),
    ("GET", "/no-such-route"),
];

async fn assert_every_probe_forbidden(addr: SocketAddr) {
    for &(method, path) in PROBES {
        for origin in [
            None,
            Some("http://evil.example"),
            Some("http://127.0.0.1:7788"),
        ] {
            assert_eq!(
                status_of(addr, method, path, origin).await,
                403,
                "{method} {path} with Origin {origin:?} must be refused"
            );
        }
    }
}

#[test]
fn parse_whois_reads_login_and_untagged_node() {
    let json = br#"{"Node":{"ID":1,"Name":"laptop."},"UserProfile":{"ID":7,"LoginName":"owner@example.com"},"CapMap":null}"#;
    let id = parse_whois_json(json).expect("parse");
    assert_eq!(id.login, OWNER);
    assert!(!id.tagged);
}

#[test]
fn parse_whois_flags_tagged_node() {
    let tags =
        br#"{"Node":{"Tags":["tag:server"]},"UserProfile":{"LoginName":"owner@example.com"}}"#;
    assert!(parse_whois_json(tags).expect("parse").tagged);
    let login = br#"{"Node":{"Tags":null},"UserProfile":{"LoginName":"tagged-devices"}}"#;
    assert!(parse_whois_json(login).expect("parse").tagged);
}

#[test]
fn parse_whois_rejects_missing_login() {
    for json in [
        &br#"{"Node":{}}"#[..],
        br#"{"Node":{},"UserProfile":{"LoginName":"  "}}"#,
        b"not json",
    ] {
        assert!(parse_whois_json(json).is_err(), "{json:?} must not resolve");
    }
}

#[tokio::test]
async fn same_login_peer_is_served() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Login(OWNER)),
    ]);
    let (addr, _stop) = spawn_tailnet(console_router(), gate(resolver)).await;
    assert_eq!(status_of(addr, "GET", "/health", None).await, 200);
}

#[tokio::test]
async fn foreign_login_peer_gets_403_on_every_route() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Login("stranger@example.org")),
    ]);
    let (addr, _stop) = spawn_tailnet(console_router(), gate(resolver)).await;
    assert_every_probe_forbidden(addr).await;
}

#[tokio::test]
async fn resolver_error_fails_closed() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Fail),
    ]);
    let (addr, _stop) = spawn_tailnet(console_router(), gate(resolver)).await;
    assert_every_probe_forbidden(addr).await;
}

#[tokio::test]
async fn host_identity_error_fails_closed() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Fail),
        (LOOPBACK_PEER, Answer::Login(OWNER)),
    ]);
    assert!(matches!(
        gate(resolver).authorize(LOOPBACK_PEER).await,
        PeerVerdict::Unresolved(_)
    ));
}

#[tokio::test]
async fn tagged_peer_is_refused() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Tagged(OWNER)),
    ]);
    assert_eq!(
        gate(resolver).authorize(LOOPBACK_PEER).await,
        PeerVerdict::Tagged
    );
}

#[tokio::test(start_paused = true)]
async fn lookup_timeout_fails_closed() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Hang),
    ]);
    let verdict = gate(resolver).authorize(LOOPBACK_PEER).await;
    assert!(
        matches!(&verdict, PeerVerdict::Unresolved(r) if r.contains("timed out")),
        "{verdict:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn burst_runs_one_lookup_per_address_until_ttl() {
    let resolver = Scripted::with_delay(
        &[
            (HOST_IP, Answer::Login(OWNER)),
            (LOOPBACK_PEER, Answer::Login(OWNER)),
        ],
        Duration::from_millis(50),
    );
    let gate = gate(Arc::clone(&resolver));
    let burst = (0..20).map(|_| gate.authorize(LOOPBACK_PEER));
    let verdicts = futures_util::future::join_all(burst).await;
    assert!(verdicts.iter().all(|v| *v == PeerVerdict::Allow));
    assert_eq!(
        resolver.calls(),
        2,
        "one lookup for the host, one for the peer"
    );

    tokio::time::advance(IDENTITY_TTL).await;
    assert_eq!(gate.authorize(LOOPBACK_PEER).await, PeerVerdict::Allow);
    assert_eq!(
        resolver.calls(),
        4,
        "an expired identity is looked up again"
    );
}

#[tokio::test(start_paused = true)]
async fn failed_lookup_is_cached_briefly() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Fail),
    ]);
    let gate = gate(Arc::clone(&resolver));
    for _ in 0..5 {
        assert!(matches!(
            gate.authorize(LOOPBACK_PEER).await,
            PeerVerdict::Unresolved(_)
        ));
    }
    assert_eq!(resolver.calls(), 2);
    tokio::time::advance(FAILURE_TTL).await;
    let _ = gate.authorize(LOOPBACK_PEER).await;
    assert_eq!(resolver.calls(), 3, "only the expired failure is retried");
}

#[tokio::test]
async fn loopback_listener_is_not_gated() {
    let resolver = Scripted::new(&[
        (HOST_IP, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Login("stranger@example.org")),
    ]);
    let router = console_router();
    let (tailnet, _stop) = spawn_tailnet(router.clone(), gate(Arc::clone(&resolver))).await;

    // The loopback listener is served exactly as `run_serve` serves it: the
    // same router value, plain `axum::serve`, no gate.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let loopback = listener.local_addr().expect("local addr");
    tokio::spawn(async move { axum::serve(listener, router).await });

    assert_eq!(status_of(tailnet, "GET", "/health", None).await, 403);
    let calls_after_tailnet = resolver.calls();
    assert_eq!(status_of(loopback, "GET", "/health", None).await, 200);
    assert_eq!(
        resolver.calls(),
        calls_after_tailnet,
        "a loopback request must not trigger an identity lookup"
    );
}
