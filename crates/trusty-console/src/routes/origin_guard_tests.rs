//! Tests for the console's MagicDNS write origins (#11749).
//!
//! The listener cases serve the real router through `serve_listeners` on
//! `127.0.0.1:0`, the path `run_serve` uses, and send raw HTTP/1.1, so the
//! `Host` and `Origin` are exactly what the server reads. The MagicDNS name is
//! injected through [`OwnMagicDns`]; no case needs a tailnet.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::future::BoxFuture;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

use super::origin_guard::WriteOrigins;
use crate::server::{AppState, build_router, build_router_with_self_origins};
use crate::tailnet_peer::{OwnMagicDns, PeerIdentity, PeerResolver, WhoisError, serve_listeners};

const NAME: &str = "studio.tail-net.ts.net";
const TAILNET_PORT: u16 = 7788;
const ASK: &str = "/api/console/assistants/izzie/ask";
const GUARD_MESSAGE: &str = "cross-origin write requests are not allowed";

/// A resolver that refuses every peer; only loopback listeners are served.
struct NoPeer;

impl PeerResolver for NoPeer {
    fn whois(&self, _ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        Box::pin(async { Err(WhoisError::NoLogin) })
    }
}

fn known_name() -> OwnMagicDns {
    let name = OwnMagicDns::default();
    assert!(name.set(NAME));
    name
}

/// Write origins as `run_serve` builds them in `--tailscale` mode.
fn tailscale_origins(name: &OwnMagicDns) -> WriteOrigins {
    let addrs: [SocketAddr; 2] = [
        format!("127.0.0.1:{TAILNET_PORT}").parse().expect("addr"),
        format!("100.64.0.9:{TAILNET_PORT}").parse().expect("addr"),
    ];
    WriteOrigins::from_bind_addrs(&addrs, Some(name.clone()))
}

/// Serve `router` on one loopback listener that knows `name`.
async fn serve(router: axum::Router, name: &OwnMagicDns) -> SocketAddr {
    serve_listeners(
        &["127.0.0.1:0".parse().expect("addr")],
        &router,
        Arc::new(NoPeer),
        name,
        std::future::pending::<()>,
    )
    .await
    .expect("serve listeners")
    .bound[0]
}

/// POST an empty JSON object to the ask route with `Host: <NAME>`, as
/// `tailscale serve` forwards it; return the status line's code and the text.
async fn post_ask(addr: SocketAddr, origin: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!(
        "POST {ASK} HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: 2\r\n"
    );
    if let Some(origin) = origin {
        req.push_str(&format!("Origin: {origin}\r\n"));
    }
    req.push_str("\r\n{}");
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

/// Why: #11749 — the shared guard trusts IPs only, so a lookalike name must
/// not slip in with the MagicDNS origin.
/// What: with the name known, only `https://<name>` and
/// `http://<name>:<tailnet port>` (any case) are added; lookalike hosts, other
/// schemes and ports, userinfo, and every origin when no name is known or the
/// console is not in `--tailscale` mode are refused. Loopback stays trusted.
/// Test: itself.
#[test]
fn write_origins_trust_only_the_exact_magic_dns_origins_11749() {
    let name = known_name();
    let origins = tailscale_origins(&name);
    for ok in [
        format!("https://{NAME}"),
        format!("HTTPS://{}", NAME.to_ascii_uppercase()),
        format!("http://{NAME}:{TAILNET_PORT}"),
        "http://127.0.0.1:7788".to_owned(),
        format!("http://100.64.0.9:{TAILNET_PORT}"),
    ] {
        assert!(origins.trusts(&ok), "{ok} must be trusted");
    }
    for bad in [
        "https://evil-studio.tail-net.ts.net".to_owned(),
        format!("https://{NAME}.evil.com"),
        format!("https://x.{NAME}"),
        format!("https://{NAME}/"),
        format!("http://{NAME}"),
        format!("https://{NAME}:443"),
        format!("https://{NAME}:{TAILNET_PORT}"),
        format!("http://{NAME}:9999"),
        format!("https://user@{NAME}"),
        "https://tail-net.ts.net".to_owned(),
        "http://evil.example".to_owned(),
    ] {
        assert!(!origins.trusts(&bad), "{bad} must be refused");
    }
    let https = format!("https://{NAME}");
    let unknown = tailscale_origins(&OwnMagicDns::default());
    assert!(!unknown.trusts(&https), "no name known must fail closed");
    let local = WriteOrigins::from_bind_addrs(&["127.0.0.1:7788".parse().expect("addr")], None);
    assert!(!local.trusts(&https), "without --tailscale: loopback only");
    assert!(!WriteOrigins::default().trusts(&https));
}

/// Why: #11749 — the owner's phone, on `https://<node>.ts.net` through
/// `tailscale serve`, got `cross-origin write requests are not allowed` from
/// the Ask Assistant POST.
/// What: under `--tailscale` write origins, the ask with `Host: <name>` and
/// `Origin: https://<name>` passes both origin guards and reaches the body
/// check (`400`, not `403`); the same request with no `Origin` is still `403`.
/// Test: itself.
#[tokio::test]
async fn tailnet_origin_write_is_accepted_under_tailscale_11749() {
    let name = known_name();
    let router =
        build_router_with_self_origins(AppState::new(Vec::new()), tailscale_origins(&name));
    let addr = serve(router, &name).await;
    let (code, text) = post_ask(addr, Some(&format!("https://{NAME}"))).await;
    assert_eq!(code, 400, "{text}");
    assert!(text.contains("the body must be"), "{text}");
    let (code, text) = post_ask(addr, None).await;
    assert_eq!(code, 403, "{text}");
}

/// Why: #11749 — behaviour without `--tailscale` stays loopback-only even
/// when the loopback listener knows the name (`tailscale serve`, #9792).
/// What: the default router refuses the same ask with the shared guard's
/// `403` and message.
/// Test: itself.
#[tokio::test]
async fn magic_dns_origin_write_is_refused_without_tailscale_11749() {
    let name = known_name();
    let addr = serve(build_router(AppState::new(Vec::new())), &name).await;
    let (code, text) = post_ask(addr, Some(&format!("https://{NAME}"))).await;
    assert_eq!(code, 403, "{text}");
    assert!(text.contains(GUARD_MESSAGE), "{text}");
}

/// Why: #11749 — the router-wide guard must keep refusing every other
/// cross-origin write with the same `403` and message.
/// What: a write route, called on the router alone (no listener guard), with
/// lookalike, foreign, other-scheme and other-port origins → `403` and the
/// guard message; the MagicDNS origin → not `403`.
/// Test: itself.
#[tokio::test]
async fn lookalike_origin_writes_are_refused_11749() {
    let name = known_name();
    let router =
        build_router_with_self_origins(AppState::new(Vec::new()), tailscale_origins(&name));
    let send = |origin: String| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method("DELETE")
                .uri("/api/console/memory/palaces/bad!id")
                .header("origin", origin)
                .body(Body::empty())
                .expect("request");
            let resp = router.oneshot(req).await.expect("response");
            let status = resp.status();
            let bytes = resp.into_body().collect().await.expect("body").to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    for bad in [
        "https://evil-studio.tail-net.ts.net".to_owned(),
        format!("https://{NAME}.evil.com"),
        format!("http://{NAME}"),
        format!("https://{NAME}:8443"),
        format!("http://{NAME}:9999"),
        "http://evil.example".to_owned(),
    ] {
        let (status, body) = send(bad.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{bad}: {body}");
        assert!(body.contains(GUARD_MESSAGE), "{bad}: {body}");
    }
    let (status, body) = send(format!("https://{NAME}")).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}
