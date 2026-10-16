//! Tests for the loopback listener's Host/Origin check (#9792).
//!
//! Every serve case drives `serve_listeners` on `127.0.0.1:0`, the path
//! `run_serve` uses, with the real console router and raw HTTP/1.1 requests,
//! so the `Host` and `Origin` a test names are exactly what the server reads.
//! The MagicDNS name is injected through [`OwnMagicDns`]; no case needs a
//! tailnet or the `tailscale` CLI.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::own_name::lookup_magic_dns;
use super::*;

const MAGIC_DNS: &str = "studio.tail-net.ts.net";
const TAILNET_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));

/// Paths a foreign page must not read: a console route, both proxy shapes,
/// and the SSE stream.
const READS: &[&str] = &[
    "/api/console/sessions",
    "/api/console/services",
    "/api/review/health",
    "/proxy/review/health",
    "/api/console/machine-status/stream",
];

/// A resolver that answers every address with one fixed outcome.
struct OwnNode(Result<Option<&'static str>, ()>);

impl PeerResolver for OwnNode {
    fn whois(&self, _ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        let answer = self.0;
        Box::pin(async move {
            match answer {
                Ok(name) => Ok(PeerIdentity {
                    login: "owner@example.com".to_owned(),
                    tagged: false,
                    node_name: name.map(str::to_owned),
                }),
                Err(()) => Err(WhoisError::NoLogin),
            }
        })
    }
}

/// A resolver whose lookup never finishes.
struct Hangs;

impl PeerResolver for Hangs {
    fn whois(&self, _ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        Box::pin(std::future::pending())
    }
}

/// Serve the console router on one loopback listener and return its address.
async fn serve_loopback(magic_dns: &OwnMagicDns) -> SocketAddr {
    let router = crate::server::build_router(crate::server::AppState::new(Vec::new()));
    serve_listeners(
        &["127.0.0.1:0".parse().expect("addr")],
        &router,
        Arc::new(OwnNode(Err(()))),
        magic_dns,
        std::future::pending::<()>,
    )
    .await
    .expect("serve listeners")
    .bound[0]
}

/// Send one raw request with `Host: host` and the given `Origin`; return the
/// status code and the whole response text.
async fn exchange(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    origin: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: 0\r\n\
         Access-Control-Request-Method: GET\r\n"
    );
    if let Some(origin) = origin {
        req.push_str(&format!("Origin: {origin}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.expect("write");
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

/// Assert `method path` with `host`/`origin` is refused with no CORS header.
async fn assert_refused(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    origin: Option<&str>,
) {
    let (code, text) = exchange(addr, method, path, host, origin).await;
    assert_eq!(
        code, 403,
        "{method} {path} Host {host} Origin {origin:?}: {text}"
    );
    assert!(
        !text.to_ascii_lowercase().contains("access-control-"),
        "{method} {path} Host {host} Origin {origin:?} carried a CORS header: {text}"
    );
}

/// Why: #9792 — any page the operator visited could read sessions, pids and
/// host metrics, because every route answered every origin.
/// What: a foreign `Origin` on reads, both proxy shapes, the SSE stream and a
/// preflight is refused with 403 and no `access-control-*` header.
/// Test: itself.
#[tokio::test]
async fn foreign_origin_is_refused_on_reads_proxies_and_sse_9792() {
    let addr = serve_loopback(&OwnMagicDns::default()).await;
    let host = addr.to_string();
    for path in READS {
        assert_refused(addr, "GET", path, &host, Some("https://evil.example")).await;
    }
    assert_refused(
        addr,
        "OPTIONS",
        "/api/console/sessions",
        &host,
        Some("https://evil.example"),
    )
    .await;
}

/// Why: #9792 — DNS rebinding points a foreign name at 127.0.0.1, so the
/// browser sends that name as `Host` and treats the reply as same-origin.
/// What: a foreign `Host` is refused on reads, proxies and SSE, with no
/// `Origin` and with its own matching one.
/// Test: itself.
#[tokio::test]
async fn foreign_host_is_refused_9792() {
    let addr = serve_loopback(&OwnMagicDns::default()).await;
    let host = format!("evil.example:{}", addr.port());
    let origin = format!("http://{host}");
    for path in READS {
        assert_refused(addr, "GET", path, &host, None).await;
        assert_refused(addr, "GET", path, &host, Some(&origin)).await;
    }
}

/// Why: #9792 Q5 — a page on another local port is not the console, for
/// reads or writes; the shared write guard trusted any loopback port.
/// What: `Origin: http://localhost:3000` is refused on GET, POST and DELETE,
/// under both own `Host` names.
/// Test: itself.
#[tokio::test]
async fn wrong_port_origin_is_refused_on_reads_and_writes_9792() {
    let addr = serve_loopback(&OwnMagicDns::default()).await;
    let origin = Some("http://localhost:3000");
    for host in [addr.to_string(), format!("localhost:{}", addr.port())] {
        assert_refused(addr, "GET", "/api/console/services", &host, origin).await;
        assert_refused(
            addr,
            "POST",
            "/api/console/sessions/abc/stop",
            &host,
            origin,
        )
        .await;
        assert_refused(addr, "DELETE", "/api/console/sessions/abc", &host, origin).await;
    }
}

/// Why: #9792 — the console's own page, `tm`, `curl` and the reqwest proxy
/// must keep working.
/// What: `127.0.0.1:<bound>` and `localhost:<bound>`, each with its own
/// `http://` origin or no `Origin`, are served.
/// Test: itself.
#[tokio::test]
async fn own_origins_are_served_9792() {
    let addr = serve_loopback(&OwnMagicDns::default()).await;
    for host in [addr.to_string(), format!("localhost:{}", addr.port())] {
        let origin = format!("http://{host}");
        for origin in [Some(origin.as_str()), None] {
            for path in ["/health", "/api/console/services"] {
                let (code, text) = exchange(addr, "GET", path, &host, origin).await;
                assert_eq!(
                    code, 200,
                    "GET {path} Host {host} Origin {origin:?}: {text}"
                );
            }
        }
    }
}

/// Why: #9792 Q4 — `tailscale serve` proxies `https://<name>/` to the
/// loopback listener and keeps the browser's `Host`, so the name is an own
/// authority, over https only.
/// What: with the name known, `Host: <name>` with `https://<name>` or no
/// `Origin` is served; an `http://` origin, a ported `Host`, or a cross-pair
/// with the loopback authority is refused.
/// Test: itself.
#[tokio::test]
async fn magic_dns_name_is_served_over_https_when_known_9792() {
    let magic = OwnMagicDns::default();
    assert!(magic.set(MAGIC_DNS));
    let addr = serve_loopback(&magic).await;
    let https = format!("https://{MAGIC_DNS}");
    for origin in [Some(https.as_str()), None] {
        let (code, text) = exchange(addr, "GET", "/api/console/services", MAGIC_DNS, origin).await;
        assert_eq!(code, 200, "Origin {origin:?}: {text}");
    }
    let ported = format!("{MAGIC_DNS}:{}", addr.port());
    let own = addr.to_string();
    let refused = [
        (MAGIC_DNS.to_owned(), format!("http://{MAGIC_DNS}")),
        (ported.clone(), format!("https://{ported}")),
        (MAGIC_DNS.to_owned(), format!("http://{own}")),
        (own.clone(), https.clone()),
    ];
    for (host, origin) in &refused {
        assert_refused(addr, "GET", "/api/console/services", host, Some(origin)).await;
    }
}

/// Why: #9792 fail-open check — a failed name lookup must leave the
/// allow-set loopback-only, never open.
/// What: with no name recorded, `Host: <name>` is refused, with and without
/// its https origin.
/// Test: itself.
#[tokio::test]
async fn magic_dns_host_is_refused_when_the_name_is_unknown_9792() {
    let addr = serve_loopback(&OwnMagicDns::default()).await;
    let https = format!("https://{MAGIC_DNS}");
    assert_refused(
        addr,
        "GET",
        "/api/console/services",
        MAGIC_DNS,
        Some(&https),
    )
    .await;
    assert_refused(addr, "GET", "/api/console/services", MAGIC_DNS, None).await;
}

/// Why: #9792 fail-open check — every error arm of the name lookup yields
/// no name, and a name that is not a plain DNS name is never recorded.
/// What: no tailnet address, a failed or hung `whois`, no node name, and
/// malformed names all yield `None`; the node's own name yields it.
/// Test: itself.
#[tokio::test]
async fn magic_dns_lookup_fails_closed_9792() {
    let bound = Duration::from_millis(200);
    let found = |ip: Option<IpAddr>| move || ip;
    assert_eq!(
        lookup_magic_dns(found(None), &OwnNode(Ok(Some(MAGIC_DNS))), bound).await,
        None
    );
    assert_eq!(
        lookup_magic_dns(found(Some(TAILNET_IP)), &OwnNode(Err(())), bound).await,
        None
    );
    assert_eq!(
        lookup_magic_dns(found(Some(TAILNET_IP)), &Hangs, bound).await,
        None
    );
    assert_eq!(
        lookup_magic_dns(found(Some(TAILNET_IP)), &OwnNode(Ok(None)), bound).await,
        None
    );
    for bad in [
        "evil.example:7788",
        "evil.example/x",
        "localhost",
        "127.0.0.1",
        "single-label",
        "has space.ts.net",
        "-lead.ts.net",
    ] {
        let resolver = OwnNode(Ok(Some(bad)));
        assert_eq!(
            lookup_magic_dns(found(Some(TAILNET_IP)), &resolver, bound).await,
            None,
            "{bad}"
        );
        assert!(
            !OwnMagicDns::default().set(bad),
            "{bad} must not be recorded"
        );
    }
    assert_eq!(
        lookup_magic_dns(
            found(Some(TAILNET_IP)),
            &OwnNode(Ok(Some(MAGIC_DNS))),
            bound
        )
        .await,
        Some(MAGIC_DNS.to_owned())
    );
}
