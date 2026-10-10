//! Tests for the tailnet peer gate (#9035).
//!
//! Every test injects a scripted [`PeerResolver`]; none needs a real tailnet.
//! The `serve_tailnet` cases bind a real loopback socket so the `ConnectInfo`
//! wiring the guard depends on is exercised, not simulated. The peer address of
//! such a connection is `127.0.0.1`; the host's tailnet address is a fixed
//! CGNAT address the resolver script answers for. The `Host` a raw request
//! sends defaults to the bound loopback address, the listener's own authority.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Uri, header};
use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::whois::parse_whois_json;
use super::*;

const HOST_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1));
const LOOPBACK_PEER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const OWNER: &str = "owner@example.com";
const MAGIC_DNS: &str = "laptop.example.ts.net";

/// What the scripted resolver answers for one address.
#[derive(Clone)]
enum Answer {
    Login(&'static str),
    /// An untagged node with a MagicDNS name.
    Named(&'static str, &'static str),
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
            let id = |login: &str, tagged, name: Option<&str>| PeerIdentity {
                login: login.to_owned(),
                tagged,
                node_name: name.map(str::to_owned),
            };
            match answer {
                Some(Answer::Login(l)) => Ok(id(l, false, None)),
                Some(Answer::Named(l, n)) => Ok(id(l, false, Some(n))),
                Some(Answer::Tagged(l)) => Ok(id(l, true, None)),
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

/// Send one raw HTTP/1.1 request, addressed to the listener itself, and return
/// the response status code.
async fn status_of(addr: SocketAddr, method: &str, path: &str, origin: Option<&str>) -> u16 {
    status_with_host(addr, method, path, &addr.to_string(), origin).await
}

/// [`status_of`] with an explicit `Host` header value.
async fn status_with_host(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    origin: Option<&str>,
) -> u16 {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: 0\r\n"
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
    let json = br#"{"Node":{"ID":1,"Name":"Laptop.Example.ts.net."},"UserProfile":{"ID":7,"LoginName":"owner@example.com"},"CapMap":null}"#;
    let id = parse_whois_json(json).expect("parse");
    assert_eq!(id.login, OWNER);
    assert!(!id.tagged);
    assert_eq!(id.node_name.as_deref(), Some(MAGIC_DNS));
    let unnamed = br#"{"Node":{"Name":"."},"UserProfile":{"LoginName":"owner@example.com"}}"#;
    assert_eq!(parse_whois_json(unnamed).expect("parse").node_name, None);
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

fn owner_resolver() -> Arc<Scripted> {
    Scripted::new(&[
        (HOST_IP, Answer::Named(OWNER, MAGIC_DNS)),
        (LOOPBACK_PEER, Answer::Login(OWNER)),
    ])
}

#[tokio::test]
async fn wrong_host_is_refused_for_an_allowed_peer() {
    let (addr, _stop) = spawn_tailnet(console_router(), gate(owner_resolver())).await;
    let port = addr.port();
    for host in [
        format!("evil.example:{port}"),
        format!("localhost:{port}"),
        format!("127.0.0.1:{}", port.wrapping_add(1)),
        "127.0.0.1".to_owned(),
        format!("{MAGIC_DNS}:{}", port.wrapping_add(1)),
        format!("{MAGIC_DNS}.:{port}"),
        format!("laptop:{port}"),
    ] {
        assert_eq!(
            status_with_host(addr, "GET", "/health", &host, None).await,
            403,
            "Host {host:?} must be refused"
        );
    }
    assert_eq!(status_of(addr, "GET", "/health", None).await, 200);
}

#[tokio::test]
async fn foreign_origin_is_refused_on_get() {
    let (addr, _stop) = spawn_tailnet(console_router(), gate(owner_resolver())).await;
    let magic = format!("http://{MAGIC_DNS}:{}", addr.port());
    let foreign = [
        "http://evil.example",
        "null",
        "http://127.0.0.1:7788",
        &format!("https://{addr}"),
        // Cross-origin between the listener's own two names.
        &magic,
    ];
    for origin in foreign {
        for (method, path) in [("GET", "/health"), ("GET", "/proxy/search/health")] {
            assert_eq!(
                status_of(addr, method, path, Some(origin)).await,
                403,
                "{method} {path} with Origin {origin:?} must be refused"
            );
        }
    }
}

#[tokio::test]
async fn self_origin_is_served() {
    let (addr, _stop) = spawn_tailnet(console_router(), gate(owner_resolver())).await;
    let own = format!("http://{addr}");
    assert_eq!(status_of(addr, "GET", "/health", Some(&own)).await, 200);
    let magic = format!("{MAGIC_DNS}:{}", addr.port());
    let magic_origin = format!("http://{magic}");
    assert_eq!(
        status_with_host(addr, "GET", "/health", &magic, Some(&magic_origin)).await,
        200
    );
}

#[test]
fn check_target_accepts_the_listener_authorities() {
    let listen: SocketAddr = "100.64.0.1:7788".parse().expect("addr");
    let allowed = SelfAuthorities::new(listen, Some(MAGIC_DNS));
    let uri = Uri::from_static("/health");
    for (host, origin) in [
        ("100.64.0.1:7788", None),
        ("100.64.0.1:7788", Some("http://100.64.0.1:7788")),
        (
            "LAPTOP.example.ts.net:7788",
            Some("http://laptop.example.ts.net:7788"),
        ),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static(host));
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        }
        assert_eq!(
            check_target(&headers, &uri, &allowed),
            Ok(()),
            "{host} {origin:?}"
        );
    }
    // HTTP/2 carries the authority in the request target, not in `Host`.
    let h2 = Uri::from_static("http://100.64.0.1:7788/health");
    assert_eq!(check_target(&HeaderMap::new(), &h2, &allowed), Ok(()));
}

#[test]
fn check_target_refuses_every_unknown_shape() {
    let listen: SocketAddr = "100.64.0.1:7788".parse().expect("addr");
    let allowed = SelfAuthorities::new(listen, None);
    let own = HeaderValue::from_static("100.64.0.1:7788");
    let path = Uri::from_static("/health");

    let refused = |headers: &HeaderMap, uri: &Uri| check_target(headers, uri, &allowed).is_err();

    // No Host and no request-target authority.
    assert!(refused(&HeaderMap::new(), &path));
    // Two Host headers.
    let mut two = HeaderMap::new();
    two.append(header::HOST, own.clone());
    two.append(header::HOST, own.clone());
    assert!(refused(&two, &path));
    // A non-UTF-8 Host.
    let mut binary = HeaderMap::new();
    binary.insert(
        header::HOST,
        HeaderValue::from_bytes(b"\xff:7788").expect("value"),
    );
    assert!(refused(&binary, &path));
    // A request target naming another authority than Host.
    let mut ok_host = HeaderMap::new();
    ok_host.insert(header::HOST, own.clone());
    assert!(refused(
        &ok_host,
        &Uri::from_static("http://evil.example/health")
    ));
    // An unknown MagicDNS name when the host's name did not resolve.
    let mut named = HeaderMap::new();
    named.insert(
        header::HOST,
        HeaderValue::from_static("laptop.example.ts.net:7788"),
    );
    assert!(refused(&named, &path));
    // Two Origin headers, even both the self-origin.
    let mut origins = ok_host.clone();
    origins.append(
        header::ORIGIN,
        HeaderValue::from_static("http://100.64.0.1:7788"),
    );
    origins.append(
        header::ORIGIN,
        HeaderValue::from_static("http://100.64.0.1:7788"),
    );
    assert!(refused(&origins, &path));
    // A non-UTF-8 Origin.
    let mut bad_origin = ok_host.clone();
    bad_origin.insert(
        header::ORIGIN,
        HeaderValue::from_bytes(b"http://\xff").expect("value"),
    );
    assert!(refused(&bad_origin, &path));
}

#[tokio::test]
async fn missing_connect_info_is_refused_without_a_lookup() {
    use tower::ServiceExt;

    let resolver = owner_resolver();
    let guard = TailnetGuard {
        gate: gate(Arc::clone(&resolver)),
        listen: "127.0.0.1:7788".parse().expect("addr"),
    };
    let app = Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            guard,
            guard_tailnet_peer,
        ));
    let req = axum::http::Request::builder()
        .uri("/health")
        .header(header::HOST, "127.0.0.1:7788")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("infallible");
    assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    assert_eq!(resolver.calls(), 0, "no lookup without a peer address");
}

#[tokio::test]
async fn spawn_tailnet_listeners_gates_every_listener() {
    let any_port: SocketAddr = "127.0.0.1:0".parse().expect("addr");
    let never = || std::future::pending::<()>();

    // Host and peer are the same loopback address here, so an unresolvable
    // identity is what a plain `axum::serve` would answer 200 to.
    let failing = Scripted::new(&[(LOOPBACK_PEER, Answer::Fail)]);
    let bound = spawn_tailnet_listeners(&[any_port], &console_router(), failing.clone(), never)
        .await
        .expect("spawn");
    assert_eq!(status_of(bound[0], "GET", "/health", None).await, 403);
    assert!(failing.calls() > 0, "the gate consulted the resolver");

    let owner = Scripted::new(&[(LOOPBACK_PEER, Answer::Login(OWNER))]);
    let bound = spawn_tailnet_listeners(&[any_port], &console_router(), owner, never)
        .await
        .expect("spawn");
    assert_eq!(status_of(bound[0], "GET", "/health", None).await, 200);
    let foreign = format!("http://evil.example:{}", bound[0].port());
    assert_eq!(
        status_of(bound[0], "GET", "/health", Some(&foreign)).await,
        403
    );
}

// ── #7524: every non-loopback listener is gated, whatever its position ──────

const WILDCARD: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Serve the console router on `addrs` through `serve_listeners`, the
/// production path `run_serve` uses, and return the bound addresses.
async fn serve_all(addrs: &[SocketAddr], resolver: Arc<Scripted>) -> Vec<SocketAddr> {
    serve_listeners(addrs, &console_router(), resolver, || {
        std::future::pending::<()>()
    })
    .await
    .expect("serve listeners")
    .bound
}

/// The loopback address that reaches `bound`, which may be a wildcard bind.
fn via_loopback(bound: SocketAddr) -> SocketAddr {
    SocketAddr::new(LOOPBACK_PEER, bound.port())
}

/// An Explicit-mode listener on a non-loopback address is the only, primary
/// listener. Both the `--http` flag and `TRUSTY_CONSOLE_BIND` resolve to it.
#[tokio::test]
async fn explicit_non_loopback_listener_is_gated_7524() {
    let default = crate::DEFAULT_HTTP;
    let flag = crate::bind::BindMode::from_flags_and_bind_env("0.0.0.0:0", default, false, None);
    let env =
        crate::bind::BindMode::from_flags_and_bind_env(default, default, false, Some("0.0.0.0:0"));
    for (source, mode) in [("--http", flag), ("TRUSTY_CONSOLE_BIND", env)] {
        assert_eq!(
            mode,
            crate::bind::BindMode::Explicit("0.0.0.0:0".to_owned()),
            "{source}"
        );
        let addrs = crate::bind::resolve_bind_addrs(&mode, crate::DEFAULT_PORT, || {
            panic!("Explicit mode must not detect a tailnet address")
        });
        assert_eq!(addrs.len(), 1, "{source}: Explicit mode binds one listener");
        // An empty script: no peer resolves, so the gate authorizes no one.
        let bound = serve_all(&addrs, Scripted::new(&[])).await;
        assert_eq!(
            status_of(via_loopback(bound[0]), "GET", "/health", None).await,
            403,
            "{source}: a non-loopback primary listener served an unauthorized peer"
        );
        assert_every_probe_forbidden(via_loopback(bound[0])).await;
    }
}

/// The gate follows the bound IP: a non-loopback listener is gated first or
/// second in the list, and a loopback listener is served ungated either way.
#[tokio::test]
async fn listener_gating_follows_the_ip_not_the_position_7524() {
    let wildcard = SocketAddr::new(WILDCARD, 0);
    let loopback = SocketAddr::new(LOOPBACK_PEER, 0);
    for addrs in [[wildcard, loopback], [loopback, wildcard]] {
        let bound = serve_all(&addrs, Scripted::new(&[])).await;
        for local in bound {
            let want = if local.ip().is_loopback() { 200 } else { 403 };
            assert_eq!(
                status_of(via_loopback(local), "GET", "/health", None).await,
                want,
                "listener {local} in {addrs:?}"
            );
        }
    }
}

/// A wildcard bind has no tailnet address of its own, so no host login exists
/// to compare against: the gate refuses without a lookup, even for a resolver
/// that would answer the owner's login for every address.
#[tokio::test]
async fn wildcard_host_address_fails_closed_7524() {
    let owner_everywhere = Scripted::new(&[
        (WILDCARD, Answer::Login(OWNER)),
        (LOOPBACK_PEER, Answer::Login(OWNER)),
    ]);
    let gate = TailnetPeerGate::new(owner_everywhere.clone(), WILDCARD);
    assert!(matches!(
        gate.authorize(LOOPBACK_PEER).await,
        PeerVerdict::Unresolved(_)
    ));
    assert_eq!(owner_everywhere.calls(), 0, "refused without a lookup");
}
