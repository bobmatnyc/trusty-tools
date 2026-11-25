//! The assistants routes on the tailnet-gated listener (#11748 items 5 and 7).
//!
//! Why: the integration tests reach the routes through the loopback guard; the
//! tailnet path (`serve_tailnet`) is crate-private, so its cases live here.
//! What: the production `serve_tailnet` on a loopback socket with a scripted
//! resolver that admits the peer as the host's own login, a fake ask daemon,
//! and the shared log capture.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};

use super::ask_route::peer_fields;
use crate::server::{AppState, build_router};
use crate::tailnet_peer::{
    AdmittedBy, PeerIdentity, PeerResolver, TailnetPeerGate, WhoisError, serve_tailnet,
};
use crate::test_log_capture::{capture_logs, logged};

const HOST_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1));
const OWNER: &str = "owner@example.com";
const PEER_NODE: &str = "phone.example.test";

/// Answers the host and the loopback peer as the same untagged login.
struct SameLogin;

impl PeerResolver for SameLogin {
    fn whois(&self, ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        Box::pin(async move {
            Ok(PeerIdentity {
                login: OWNER.to_owned(),
                tagged: false,
                node_name: ip.is_loopback().then(|| PEER_NODE.to_owned()),
            })
        })
    }
}

/// A root with instance `id`, a `0700` data dir, and a one-shot fake daemon
/// answering `answer` on `id`'s socket.
fn seed(id: &str, answer: &str) -> (tempfile::TempDir, tempfile::TempDir, AppState) {
    let root = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join(id)).expect("mkdir");
    std::fs::write(root.path().join(id).join("instructions.md"), "x").expect("write");
    std::fs::set_permissions(data.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = data.path().join(format!("trusty-agents-{id}.sock"));
    let listener = UnixListener::bind(&socket).expect("bind");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let mut reply =
        json!({"protocol": 1, "daemon_pid": 7, "outcome": "answer", "answer": answer}).to_string();
    reply.push('\n');
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read)
            .read_line(&mut line)
            .await
            .expect("read");
        write.write_all(reply.as_bytes()).await.expect("write");
    });
    let state = AppState::new(vec![])
        .with_assistants_root(root.path().to_path_buf())
        .with_assistants_data_dir(data.path().to_path_buf());
    (root, data, state)
}

/// Serve `state`'s router through the production tailnet path.
async fn spawn_tailnet(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let gate = Arc::new(TailnetPeerGate::new(Arc::new(SameLogin), HOST_IP));
    tokio::spawn(serve_tailnet(
        listener,
        build_router(state),
        gate,
        std::future::pending(),
    ));
    addr
}

/// The gated tailnet listener serves the routes, and the ask line names the
/// whois peer and never the text.
#[tokio::test]
async fn tailnet_ask_is_served_and_logs_the_whois_peer() {
    capture_logs();
    let (_root, _data, state) = seed("tailcase", "ANSWER-6d1e");
    let addr = spawn_tailnet(state).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let list = client
        .get(format!("{base}/api/console/assistants"))
        .send()
        .await
        .expect("list");
    assert_eq!(list.status().as_u16(), 200);

    let resp = client
        .post(format!("{base}/api/console/assistants/tailcase/ask"))
        .header("origin", base.clone())
        .json(&json!({ "question": "QUESTION-0a9c" }))
        .send()
        .await
        .expect("ask");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["answer"], "ANSWER-6d1e");

    let logs = logged();
    let line = logs
        .lines()
        .find(|l| l.contains("assistants: ask") && l.contains("instance=\"tailcase\""))
        .unwrap_or_else(|| panic!("no ask line in:\n{logs}"));
    for field in [
        "listener=\"tailnet\"",
        "peer_login=owner@example.com",
        "peer_node=phone.example.test",
        "question_bytes=13",
    ] {
        assert!(line.contains(field), "`{field}` missing from: {line}");
    }
    assert!(
        !logs.contains("QUESTION-0a9c"),
        "the question reached the log"
    );
    assert!(!logs.contains("ANSWER-6d1e"), "the answer reached the log");
}

/// AC5: a tailnet ask with no whois result still logs, marked unknown.
#[test]
fn a_tailnet_peer_without_whois_is_logged_unknown() {
    let peer: SocketAddr = "192.0.2.9:5555".parse().expect("addr");
    let admitted = AdmittedBy::Tailnet {
        peer,
        identity: None,
    };
    let (listener, addr, login, node) = peer_fields(Some(&admitted));
    assert_eq!(listener, "tailnet");
    assert_eq!(addr, "192.0.2.9:5555");
    assert_eq!(login, "unknown");
    assert_eq!(node, "unknown");
    let (listener, _, login, _) = peer_fields(Some(&AdmittedBy::Loopback));
    assert_eq!((listener, login.as_str()), ("loopback", "loopback"));
}

/// #11748 review: the handler alone, mounted with no guard and no marker,
/// refuses before it dials.
#[tokio::test]
async fn the_handler_alone_refuses_an_unmarked_request() {
    use tower::ServiceExt;
    let (_root, data, state) = seed("bare", "LEAK");
    let app = axum::Router::new()
        .route(
            "/ask/{id}",
            axum::routing::post(super::ask_route::ask_handler),
        )
        .with_state(state);
    let resp = app
        .oneshot(
            axum::http::Request::post("/ask/bare")
                .header("origin", "http://127.0.0.1:1")
                .body(axum::body::Body::from(r#"{"question":"q"}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status().as_u16(), 403);
    // The one-shot fake daemon still answers its first connection: the
    // handler never dialled it.
    let socket = data.path().join("trusty-agents-bare.sock");
    let mut probe = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("the fake daemon was never dialled");
    probe.write_all(b"{}\n").await.expect("write");
    let mut reply = String::new();
    BufReader::new(probe)
        .read_line(&mut reply)
        .await
        .expect("read");
    assert!(
        reply.contains("LEAK"),
        "the daemon's one reply went elsewhere: {reply:?}"
    );
}

/// #11754: an ungated request is refused before its body stream is polled.
#[tokio::test]
async fn the_handler_alone_never_polls_an_unmarked_body() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;
    let (_root, _data, state) = seed("unpolled", "LEAK");
    let polled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&polled);
    let stream = futures_util::stream::once(async move {
        flag.store(true, Ordering::SeqCst);
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(br#"{"question":"q"}"#))
    });
    let app = axum::Router::new()
        .route(
            "/ask/{id}",
            axum::routing::post(super::ask_route::ask_handler),
        )
        .with_state(state);
    let resp = app
        .oneshot(
            axum::http::Request::post("/ask/unpolled")
                .header("origin", "http://127.0.0.1:1")
                .body(axum::body::Body::from_stream(stream))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status().as_u16(), 403);
    assert!(
        !polled.load(Ordering::SeqCst),
        "the body was read before the admission check"
    );
}

/// Post `body` to the bare handler as an admitted loopback request, with no
/// `DefaultBodyLimit` layer; the status and the JSON body.
async fn post_admitted(
    state: AppState,
    id: &str,
    body: axum::body::Body,
) -> (u16, serde_json::Value) {
    use tower::ServiceExt;
    let app = axum::Router::new()
        .route(
            "/ask/{id}",
            axum::routing::post(super::ask_route::ask_handler),
        )
        .with_state(state);
    let mut req = axum::http::Request::post(format!("/ask/{id}"))
        .header("origin", "http://127.0.0.1:1")
        .body(body)
        .expect("request");
    req.extensions_mut().insert(AdmittedBy::Loopback);
    let resp = app.oneshot(req).await.expect("response");
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

/// The error body a body-read refusal carries.
fn body_refusal() -> serde_json::Value {
    json!({ "error": format!(
        "the request body is over {} bytes or unreadable",
        super::ask_route::MAX_ASK_BODY_BYTES
    ) })
}

/// #11754: the handler's own read enforces the cap without the route layer.
#[tokio::test]
async fn an_admitted_body_over_the_cap_is_413() {
    let (_root, _data, state) = seed("overcap", "LEAK");
    let huge = format!(
        r#"{{"question":"{}"}}"#,
        "x".repeat(super::ask_route::MAX_ASK_BODY_BYTES)
    );
    let (status, body) = post_admitted(state, "overcap", axum::body::Body::from(huge)).await;
    assert_eq!(status, 413, "{body}");
    assert_eq!(body, body_refusal());
}

/// #11754 fail-open check: a body that errors mid-read is `400`, never an
/// answer or a default question.
#[tokio::test]
async fn an_admitted_body_read_error_is_400() {
    let (_root, _data, state) = seed("broken", "LEAK");
    let stream = futures_util::stream::iter([
        Ok(axum::body::Bytes::from_static(br#"{"question":"q"}"#)),
        Err(std::io::Error::other("reset")),
    ]);
    let (status, body) =
        post_admitted(state, "broken", axum::body::Body::from_stream(stream)).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body, body_refusal());
}
