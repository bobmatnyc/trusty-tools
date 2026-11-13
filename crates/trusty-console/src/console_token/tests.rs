//! Tests for the console token gate (#11074).
//!
//! The router cases drive the real console router through `oneshot`. The
//! listener cases wrap it in the production loopback guard and tailnet serve
//! path, so the order "listener check, then token" is what is exercised.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use futures_util::future::BoxFuture;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

use super::*;
use crate::tailnet_peer::{
    OwnMagicDns, PeerIdentity, PeerResolver, TailnetPeerGate, WhoisError, guard_loopback,
    serve_tailnet,
};

/// The test token: long enough to load, distinct enough to grep for.
const TOKEN: &str = "f00dfeed11074f00dfeed11074f00dfeed11074f00d";

/// Every route shape a caller can reach: reads, a write, a proxy, the SSE
/// stream, a tool SPA, the secrets bridge and an unrouted path.
const PROBES: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/api/console/services"),
    ("GET", "/api/console/sessions"),
    ("DELETE", "/api/console/sessions/abc"),
    ("GET", "/api/review/health"),
    ("GET", "/api/console/machine-status/stream"),
    ("GET", "/tools/search/"),
    ("GET", "/api/secrets"),
    ("POST", "/api/webhooks/github"),
    ("GET", "/"),
    ("GET", "/no-such-route"),
];

/// Load [`TOKEN`] from a `0600` file, the only way to build a token.
fn load_token() -> ConsoleToken {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("console-token");
    std::fs::write(&path, format!("{TOKEN}\n")).expect("write token");
    std::fs::set_permissions(
        &path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
    .expect("chmod");
    trusty_common::console_token::load_from_path(&path).expect("token loads")
}

fn gate() -> Arc<TokenGate> {
    Arc::new(TokenGate::new(load_token()))
}

fn console() -> Router {
    crate::server::build_router(crate::server::AppState::new(Vec::new()))
}

fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder().method(method).uri(path)
}

async fn send(router: &Router, req: Request<Body>) -> Response {
    tokio::time::timeout(Duration::from_secs(20), router.clone().oneshot(req))
        .await
        .expect("request finished in time")
        .expect("infallible")
}

async fn body_bytes(resp: Response) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec()
}

/// The `Set-Cookie` of a successful login, and the session id it carries.
async fn login(router: &Router) -> (String, String) {
    let resp = send(
        router,
        request("POST", LOGIN_PATH)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("token={TOKEN}")))
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers()[header::LOCATION], "/");
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .expect("ascii cookie")
        .to_owned();
    let id = cookie
        .split(';')
        .next()
        .and_then(|pair| pair.strip_prefix(&format!("{SESSION_COOKIE}=")))
        .expect("session pair first")
        .to_owned();
    (cookie, id)
}

/// The #11074 regression: with the token on, a request with no token is
/// refused on every route. Before the fix the console served all of them.
#[tokio::test]
async fn unauthenticated_request_is_401_with_token_file_set_11074() {
    let router = guard(console(), Some(gate()));
    for &(method, path) in PROBES {
        let resp = send(
            &router,
            request(method, path).body(Body::empty()).expect("req"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(resp.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }
}

#[tokio::test]
async fn right_bearer_is_served_on_every_probe_11074() {
    let router = guard(console(), Some(gate()));
    for &(method, path) in PROBES {
        for scheme in ["Bearer", "bearer"] {
            let resp = send(
                &router,
                request(method, path)
                    .header(header::AUTHORIZATION, format!("{scheme} {TOKEN}"))
                    .body(Body::empty())
                    .expect("req"),
            )
            .await;
            assert_ne!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
            assert_ne!(resp.status(), StatusCode::SEE_OTHER, "{method} {path}");
        }
    }
    let resp = send(
        &router,
        request("GET", "/health")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// TOK-6: missing, wrong, truncated and non-bearer credentials are refused
/// identically, so the response does not say which check failed.
#[tokio::test]
async fn missing_and_wrong_token_get_the_same_401_11074() {
    let router = guard(console(), Some(gate()));
    let wrong = format!("Bearer {}", TOKEN.replace('f', "e"));
    let short = format!("Bearer {}", &TOKEN[..TOKEN.len() - 1]);
    let basic = format!("Basic {TOKEN}");
    let mut seen = Vec::new();
    for auth in [
        None,
        Some(wrong.as_str()),
        Some(short.as_str()),
        Some(basic.as_str()),
        Some("Bearer"),
    ] {
        let mut req = request("GET", "/api/console/services");
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        let resp = send(&router, req.body(Body::empty()).expect("req")).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        let www = resp.headers()[header::WWW_AUTHENTICATE].clone();
        seen.push((www, body_bytes(resp).await));
    }
    assert!(
        seen.windows(2).all(|w| w[0] == w[1]),
        "responses differ: {seen:?}"
    );
    assert_eq!(seen[0].1, UNAUTHORIZED_BODY.as_bytes());
}

#[tokio::test]
async fn browser_navigation_redirects_to_login_11074() {
    let router = guard(console(), Some(gate()));
    for path in ["/", "/ui/", "/tools/search/"] {
        let resp = send(
            &router,
            request("GET", path)
                .header(header::ACCEPT, "text/html,application/xhtml+xml")
                .body(Body::empty())
                .expect("req"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{path}");
        assert_eq!(resp.headers()[header::LOCATION], LOGIN_PATH);
    }
}

#[tokio::test]
async fn login_form_is_served_unauthenticated_11074() {
    let router = guard(console(), Some(gate()));
    let resp = send(
        &router,
        request("GET", LOGIN_PATH).body(Body::empty()).expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(body_bytes(resp).await).expect("utf8");
    assert!(
        body.contains(r#"<form method="post" action="/login">"#),
        "{body}"
    );
    assert!(!body.contains(TOKEN));
}

/// TOK-5: the cookie attributes, a value that is not the token, and that the
/// cookie then opens the console.
#[tokio::test]
async fn login_cookie_opens_the_console_and_holds_no_token_11074() {
    let router = guard(console(), Some(gate()));
    let (cookie, id) = login(&router).await;
    let attrs: Vec<&str> = cookie.split(';').map(str::trim).collect();
    for want in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=43200",
    ] {
        assert!(attrs.contains(&want), "missing {want}: {cookie}");
    }
    assert!(!cookie.contains(TOKEN), "cookie holds the token: {cookie}");
    assert_eq!(id.len(), 64);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    let resp = send(
        &router,
        request("GET", "/api/console/services")
            .header(header::COOKIE, format!("theme=dark; {SESSION_COOKIE}={id}"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // A second login is a second, different session.
    let (_, second) = login(&router).await;
    assert_ne!(id, second);
}

#[tokio::test]
async fn wrong_login_sets_no_cookie_11074() {
    let router = guard(console(), Some(gate()));
    for body in ["token=not-the-token", "token=", ""] {
        let resp = send(
            &router,
            request("POST", LOGIN_PATH)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .expect("req"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{body:?}");
        assert!(resp.headers().get(header::SET_COOKIE).is_none());
    }
}

/// A body the form extractor rejects is a refusal, never an extractor error.
#[tokio::test]
async fn malformed_login_sets_no_cookie_11074() {
    let router = guard(console(), Some(gate()));
    let resp = send(
        &router,
        request("POST", LOGIN_PATH)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(r#"{{"token":"{TOKEN}"}}"#)))
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn expired_session_is_refused_11074() {
    let gate = Arc::new(TokenGate::with_session_ttl(load_token(), Duration::ZERO));
    let router = guard(console(), Some(Arc::clone(&gate)));
    let (_, id) = login(&router).await;
    let resp = send(
        &router,
        request("GET", "/api/console/services")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={id}"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // An unknown id is refused the same way.
    let resp = send(
        &router,
        request("GET", "/api/console/services")
            .header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", "0".repeat(64)),
            )
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn session_store_is_bounded_11074() {
    let store = SessionStore::new(SESSION_TTL);
    for _ in 0..session::MAX_SESSIONS + 5 {
        store.mint().expect("mint");
    }
    assert_eq!(store.len(), session::MAX_SESSIONS);
}

/// A router whose one handler reports the credential headers it received.
fn echo_router() -> Router {
    Router::new().route(
        "/echo",
        get(|headers: HeaderMap| async move {
            let auth = headers.get(header::AUTHORIZATION).is_some();
            let cookie = headers
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            format!("auth={auth};cookie={cookie}")
        }),
    )
}

/// TOK-7: handlers and the reverse proxy never see the token or the session.
#[tokio::test]
async fn credentials_never_reach_a_handler_11074() {
    let router = guard(echo_router(), Some(gate()));
    let (_, id) = login(&router).await;
    let resp = send(
        &router,
        request("GET", "/echo")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::COOKIE, format!("a=1; {SESSION_COOKIE}={id}; b=2"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, b"auth=false;cookie=a=1; b=2");
    let resp = send(
        &router,
        request("GET", "/echo")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={id}"))
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(body_bytes(resp).await, b"auth=false;cookie=");
}

/// TOK-7: the token and the session id appear in no log line, even under a
/// request logger that records every header it sees. The capture is the
/// crate's one global subscriber (`test_log_capture`).
#[tokio::test]
async fn token_and_session_id_never_reach_the_log_11074() {
    crate::test_log_capture::capture_logs();
    let logged = echo_router().layer(
        tower_http::trace::TraceLayer::new_for_http()
            .make_span_with(tower_http::trace::DefaultMakeSpan::new().include_headers(true)),
    );
    let router = guard(logged, Some(gate()));
    let (_, id) = login(&router).await;
    for (auth, cookie) in [
        (Some(format!("Bearer {TOKEN}")), None),
        (None, Some(format!("{SESSION_COOKIE}={id}"))),
        (Some(format!("Bearer {TOKEN}x")), None),
    ] {
        let mut req = request("GET", "/echo");
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        send(&router, req.body(Body::empty()).expect("req")).await;
    }
    let log = crate::test_log_capture::logged();
    assert!(
        log.contains("/echo"),
        "the request logger recorded nothing: {log}"
    );
    assert!(!log.contains(TOKEN), "token in log: {log}");
    assert!(!log.contains(&id), "session id in log: {log}");
}

/// TOK-1: unset, the router is the one it was — same status, headers and
/// bytes, and `/login` is not mounted.
#[tokio::test]
async fn unset_token_leaves_the_router_unchanged_11074() {
    let plain = console();
    let guarded = guard(plain.clone(), None);
    for &(method, path) in &[
        ("GET", "/no-such-route"),
        ("GET", LOGIN_PATH),
        ("POST", LOGIN_PATH),
        ("GET", "/api/console/sessions"),
    ] {
        let a = send(
            &plain,
            request(method, path).body(Body::empty()).expect("req"),
        )
        .await;
        let b = send(
            &guarded,
            request(method, path).body(Body::empty()).expect("req"),
        )
        .await;
        assert_eq!(a.status(), b.status(), "{method} {path}");
        assert_eq!(a.headers(), b.headers(), "{method} {path}");
        assert_eq!(body_bytes(a).await, body_bytes(b).await, "{method} {path}");
    }
    let resp = send(
        &guarded,
        request("GET", "/health").body(Body::empty()).expect("req"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// The loopback listener: Host/Origin first (403), then the token (401).
#[tokio::test]
async fn loopback_listener_checks_host_then_token_11074() {
    let listen: SocketAddr = "127.0.0.1:7788".parse().expect("addr");
    let router = guard_loopback(
        guard(console(), Some(gate())),
        listen,
        OwnMagicDns::default(),
    );
    let bearer = format!("Bearer {TOKEN}");
    for (host, auth, want) in [
        (
            "evil.example:7788",
            Some(bearer.as_str()),
            StatusCode::FORBIDDEN,
        ),
        ("127.0.0.1:7788", None, StatusCode::UNAUTHORIZED),
        ("localhost:7788", None, StatusCode::UNAUTHORIZED),
        ("127.0.0.1:7788", Some(bearer.as_str()), StatusCode::OK),
    ] {
        let mut req = request("GET", "/health").header(header::HOST, host);
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        let resp = send(&router, req.body(Body::empty()).expect("req")).await;
        assert_eq!(resp.status(), want, "{host} {auth:?}");
    }
}

/// A resolver that answers every peer with the host's own login, or fails.
struct Peer(bool);

impl PeerResolver for Peer {
    fn whois(&self, _ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        let known = self.0;
        Box::pin(async move {
            if known {
                Ok(PeerIdentity {
                    login: "owner@example.com".to_owned(),
                    tagged: false,
                    node_name: None,
                })
            } else {
                Err(WhoisError::NoLogin)
            }
        })
    }
}

/// One raw HTTP/1.1 request to `addr`; returns the status code.
async fn raw_status(addr: SocketAddr, auth: Option<&str>) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut req = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(auth) = auth {
        req.push_str(&format!("Authorization: {auth}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("read");
    let head = String::from_utf8_lossy(&buf);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {head:?}"))
}

/// The tailnet listener: the peer gate first (403), then the token (401).
#[tokio::test]
async fn tailnet_listener_checks_peer_then_token_11074() {
    let bearer = format!("Bearer {TOKEN}");
    for (known, auth, want) in [
        (false, Some(bearer.as_str()), 403),
        (true, None, 401),
        (
            true,
            Some("Bearer wrong-token-wrong-token-wrong-token"),
            401,
        ),
        (true, Some(bearer.as_str()), 200),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let peer_gate = Arc::new(TailnetPeerGate::new(
            Arc::new(Peer(known)),
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        ));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(serve_tailnet(
            listener,
            guard(console(), Some(gate())),
            peer_gate,
            async move {
                let _ = stopped.await;
            },
        ));
        assert_eq!(raw_status(addr, auth).await, want, "known={known} {auth:?}");
        let _ = stop.send(());
    }
}

/// TOK-3: with the variable set to a bad file, `run_serve` fails naming the
/// path, before it binds. The listen address is held by this test, so a
/// bind attempted first would fail with "address in use" instead.
/// #9123: it sets a `TOKEN`-named variable, so it is `#[serial]` and runs in
/// the credential sandbox.
#[test]
#[serial_test::serial]
fn run_serve_refuses_a_bad_token_file_before_binding_11074() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let addr = held.local_addr().expect("addr");
    let dir = tempfile::tempdir().expect("tempdir");
    let readable = dir.path().join("group-readable");
    std::fs::write(&readable, TOKEN).expect("write");
    std::fs::set_permissions(
        &readable,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o640),
    )
    .expect("chmod");
    let short = dir.path().join("short");
    std::fs::write(&short, &TOKEN[..8]).expect("write");
    std::fs::set_permissions(
        &short,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
    .expect("chmod");
    let missing = dir.path().join("missing");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let _env = crate::detect::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _sandbox = trusty_common::credentials::test_sandbox::CredentialSandbox::enter();
    for (path, reason) in [
        (&missing, "does not exist"),
        (&readable, "group or other"),
        (&short, "shorter than"),
    ] {
        // SAFETY: serialised by ENV_LOCK, the crate's one env lock.
        unsafe { std::env::set_var(trusty_common::console_token::TOKEN_FILE_ENV, path) };
        let args = <crate::ServeArgs as clap::Parser>::parse_from([
            "serve".to_owned(),
            "--http".to_owned(),
            addr.to_string(),
        ]);
        let err = runtime
            .block_on(crate::run_serve(args))
            .expect_err("a bad token file must stop the console");
        let text = format!("{err:?}");
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains(reason), "{text}");
        assert!(!text.contains(&TOKEN[..8]), "error quotes the file: {text}");
    }
    // SAFETY: as above.
    unsafe { std::env::remove_var(trusty_common::console_token::TOKEN_FILE_ENV) };
    drop(held);
}
