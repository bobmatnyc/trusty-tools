//! Tests for the passkey enrol page and its relay (#8878 PR2b-2). A stub tm
//! daemon on a scratch Unix socket stands in for `mpm.presence.*`; it records
//! every method it is asked, so each refusal can show that no registration
//! call reached it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use super::*;

const ORIGIN: &str = "https://studio.example.ts.net";
const HOST: &str = "studio.example.ts.net";

/// Every `(method, params)` the stub saw.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// Serve a stub daemon at `<dir>/mpm.sock`: `status` answers `enrol_origin`,
/// `register.*` answer `register` (a whole response frame body).
fn daemon(dir: &Path, enrol_origin: Option<&str>, register: Value) -> (PathBuf, Seen) {
    let socket = dir.join("mpm.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    let status = json!({ "result": { "enrol_origin": enrol_origin } });
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        while let Ok((mut conn, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            let request: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let body = if method == "mpm.presence.status" {
                status.clone()
            } else {
                register.clone()
            };
            if let Ok(mut seen) = log.lock() {
                seen.push((method, request["params"].clone()));
            }
            let mut frame = json!({ "jsonrpc": "2.0", "id": 1 });
            if let (Some(f), Some(b)) = (frame.as_object_mut(), body.as_object()) {
                f.extend(b.clone());
            }
            let _ = conn.write_all(frame.to_string().as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
        }
    });
    (socket, seen)
}

fn methods(seen: &Seen) -> Vec<String> {
    seen.lock()
        .expect("seen")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

fn get_page(host: &str) -> HttpRequest<Body> {
    HttpRequest::get(ENROL_PAGE_PATH)
        .header(header::HOST, host)
        .body(Body::empty())
        .expect("request")
}

fn post_json(path: &str, host: &str, origin: Option<&str>, body: Value) -> HttpRequest<Body> {
    let mut builder = HttpRequest::post(path)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn send(router: &Router, request: HttpRequest<Body>) -> (StatusCode, HeaderMap, String) {
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn ticket() -> Value {
    json!({ "purpose": "enrol", "subject": "iphone", "nonce": "00", "issued_at": 1,
        "expires_at": 2, "mac": "00" })
}

/// Q4: with `browser_enrol` off the daemon reports no origin, so the page
/// and both calls are 404 and nothing is relayed; with no daemon they are
/// 503, never served.
#[tokio::test]
async fn the_enrol_routes_are_absent_while_the_switch_is_off_8878() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, seen) = daemon(dir.path(), None, json!({ "result": {} }));
    let app = router(PresenceBridge::with_socket(socket));
    let (status, _, body) = send(&app, get_page(HOST)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("not enabled"), "{body}");
    for path in [ENROL_START_PATH, ENROL_FINISH_PATH] {
        let body = json!({ "ticket": ticket(), "credential": {} });
        let body = if path == ENROL_START_PATH {
            json!({ "ticket": ticket() })
        } else {
            body
        };
        let (status, _, _) = send(&app, post_json(path, HOST, Some(ORIGIN), body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    assert!(methods(&seen).iter().all(|m| m == "mpm.presence.status"));
    let gone = router(PresenceBridge::with_socket(dir.path().join("none.sock")));
    let (status, _, _) = send(&gone, get_page(HOST)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// The page is served on its origin's host, with no caching or framing;
/// on the loopback address (where WebAuthn could not bind it anyway) it is 403.
#[tokio::test]
async fn the_enrol_page_is_served_only_on_its_origin_8878() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, _) = daemon(dir.path(), Some(ORIGIN), json!({ "result": {} }));
    let app = router(PresenceBridge::with_socket(socket));
    let (status, headers, body) = send(&app, get_page(HOST)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("navigator.credentials.create"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    for host in ["127.0.0.1:7890", "localhost:7890", "evil.example"] {
        let (status, _, _) = send(&app, get_page(host)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host}");
    }
}

/// Plan §2: the relay does not rest on the peer gate. A POST with no
/// `Origin`, a foreign `Origin`, a foreign `Host`, or no ticket is refused
/// before any registration call reaches the daemon.
#[tokio::test]
async fn a_request_not_addressed_to_the_enrol_origin_is_refused_8878() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (socket, seen) = daemon(dir.path(), Some(ORIGIN), json!({ "result": {} }));
    let app = router(PresenceBridge::with_socket(socket));
    let start = || json!({ "ticket": ticket() });
    for (host, origin) in [
        (HOST, None),
        (HOST, Some("https://evil.example")),
        (HOST, Some("http://studio.example.ts.net")),
        ("127.0.0.1:7890", Some(ORIGIN)),
    ] {
        let (status, _, _) = send(&app, post_json(ENROL_START_PATH, host, origin, start())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host} {origin:?}");
    }
    for body in [
        json!({}),
        json!({ "ticket": null }),
        json!({ "credential": {} }),
    ] {
        let (status, _, _) =
            send(&app, post_json(ENROL_START_PATH, HOST, Some(ORIGIN), body)).await;
        assert!(status.is_client_error(), "{status}");
    }
    let no_credential = json!({ "ticket": ticket() });
    let (status, _, _) = send(
        &app,
        post_json(ENROL_FINISH_PATH, HOST, Some(ORIGIN), no_credential),
    )
    .await;
    assert!(status.is_client_error(), "{status}");
    assert!(
        methods(&seen).iter().all(|m| m == "mpm.presence.status"),
        "{:?}",
        methods(&seen)
    );
}

/// A gated start and finish reach the daemon with the page's ticket and
/// credential unchanged; a daemon refusal (spent, expired) reaches the page
/// as an error, never as a success.
#[tokio::test]
async fn start_and_finish_relay_to_the_daemon_8878() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ok = json!({ "result": { "credential_id": "cred", "name": "iphone" } });
    let (socket, seen) = daemon(dir.path(), Some(ORIGIN), ok);
    let app = router(PresenceBridge::with_socket(socket));
    let credential = json!({ "id": "x", "rawId": "eA", "type": "public-key" });
    let finish = json!({ "ticket": ticket(), "credential": credential });
    let (status, _, body) = send(
        &app,
        post_json(ENROL_FINISH_PATH, HOST, Some(ORIGIN), finish),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("json")["credential_id"],
        "cred"
    );
    let relayed = seen.lock().expect("seen").last().cloned().expect("a call");
    assert_eq!(relayed.0, "mpm.presence.register.finish");
    assert_eq!(
        relayed.1,
        json!({ "ticket": ticket(), "credential": credential })
    );

    let dir2 = tempfile::tempdir().expect("tempdir");
    let spent =
        json!({ "error": { "code": -32022, "message": "the presence ticket was already used" } });
    let (socket, seen) = daemon(dir2.path(), Some(ORIGIN), spent);
    let app = router(PresenceBridge::with_socket(socket));
    let (status, _, body) = send(
        &app,
        post_json(
            ENROL_START_PATH,
            HOST,
            Some(ORIGIN),
            json!({ "ticket": ticket() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("already used"), "{body}");
    assert_eq!(
        methods(&seen).last().map(String::as_str),
        Some("mpm.presence.register.start")
    );
}

/// The page shows "Enrolled" only after a confirmed finish, and keeps the
/// ticket out of the address bar.
#[test]
fn the_enrol_page_reports_failure_unless_the_finish_confirms_8878() {
    assert!(ENROL_HTML.contains("history.replaceState(null, \"\", location.pathname)"));
    let success = ENROL_HTML.find("show(\"Enrolled").expect("success line");
    let check = ENROL_HTML
        .find("typeof done.credential_id !== \"string\"")
        .expect("check");
    assert!(
        check < success,
        "the confirmation check precedes the success line"
    );
    assert!(ENROL_HTML.contains("if (!r.ok) throw"));
}

/// No software authenticator reaches the console build in any form.
#[test]
fn no_software_authenticator_in_the_console_8878() {
    let manifest = include_str!("../../Cargo.toml");
    assert!(!manifest.contains("webauthn-authenticator"));
    assert!(!manifest.contains("softtoken"));
}
