//! The console's own authorities for the secrets routes (#9067 P2).
//!
//! Why: the page opened at `http://localhost:PORT/tools/secrets/` sends
//! `Host: localhost:PORT`; the guard knew only `127.0.0.1:PORT`, so every
//! call from that page was refused. `localhost` on the bound port is the
//! console's own name; no other host name is.
//! What: drives the real secrets router with the Host/Origin pairs a
//! browser sends.
//! Test: this file IS the test.

use std::sync::Arc;

use axum::http::StatusCode;

use super::tests::{LIST_BODY, OWN, bridge_at, family, ok_stub, own_request, send};

/// A request to `path` with `Host: host` and `Origin: origin`.
fn from(
    bridge: &super::SecretsBridge,
    path: &str,
    host: &str,
    origin: &str,
) -> axum::http::Request<axum::body::Body> {
    let mut request = own_request(bridge, path, LIST_BODY);
    let headers = request.headers_mut();
    headers.insert("host", host.parse().expect("header"));
    headers.insert("origin", origin.parse().expect("header"));
    request
}

/// Why: #9067 P2 — `localhost` on the bound port is the console itself, so
/// the page opened there may fetch its token and list keys.
/// Red until `localhost:PORT` is an own authority.
/// Test: itself.
#[tokio::test]
async fn localhost_on_the_bound_port_is_an_own_authority() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let port = OWN.rsplit(':').next().expect("port");
    let host = format!("localhost:{port}");
    let origin = format!("http://{host}");
    for path in ["/api/secrets/session", "/api/secrets/list"] {
        let (status, body) = send(&bridge, from(&bridge, path, &host, &origin)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    assert_eq!(seen.lock().expect("lock").len(), 1, "list dialled once");
}

/// Why: #9067 P2 — only `localhost` on the bound port is added: any other
/// name, `localhost` on another port, a name that merely starts with
/// `localhost`, and an Origin that names a different authority than Host
/// are all still refused, and none reaches the socket.
/// Test: itself.
#[tokio::test]
async fn every_other_host_name_is_still_refused() {
    let (_tmp, socket, seen) = ok_stub();
    let bridge = bridge_at(&socket, Arc::new(family()));
    let port = OWN.rsplit(':').next().expect("port");
    let cases = [
        (
            format!("evil.test:{port}"),
            format!("http://evil.test:{port}"),
        ),
        (
            "localhost:9999".to_owned(),
            "http://localhost:9999".to_owned(),
        ),
        (
            format!("localhost.evil.test:{port}"),
            format!("http://localhost.evil.test:{port}"),
        ),
        (format!("localhost:{port}"), format!("http://{OWN}")),
        (OWN.to_owned(), format!("http://localhost:{port}")),
        (
            format!("localhost:{port}"),
            format!("https://localhost:{port}"),
        ),
    ];
    for (host, origin) in &cases {
        for path in ["/api/secrets/session", "/api/secrets/list"] {
            let (status, _) = send(&bridge, from(&bridge, path, host, origin)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path} {host} {origin}");
        }
    }
    assert!(seen.lock().expect("lock").is_empty());
}
