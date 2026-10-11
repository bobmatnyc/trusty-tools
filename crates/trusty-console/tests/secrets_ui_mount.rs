//! The Secrets page mount at `/tools/secrets/` (#9067, DOC-74 §15.6).
//!
//! Why: the page is the console's fifth embedded bundle. It must be reachable
//! through the same mount pipeline as the other dashboards, its calls must go
//! to the guarded `/api/secrets/` bridge, and it must not shadow or be
//! shadowed by another mount.
//! What: drives `build_router` with the real embedded bundle: the bare-path
//! redirect, the shell with `__SECRETS_BASE__` injected, every referenced
//! asset, the unknown-path fallback and the other mounts.
//! Test: this file IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use trusty_console::server::{AppState, build_router};

/// A response's status, headers and body text.
struct Got {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

impl Got {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn send(request: Request<Body>) -> Got {
    let router = build_router(AppState::new(vec![]));
    let resp = router.oneshot(request).await.expect("router response");
    let status = resp.status();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    Got {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn get(path: &str) -> Got {
    send(
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request"),
    )
    .await
}

/// The local `src="./…"` and `href="./…"` references in a shell.
fn local_refs(shell: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for marker in ["src=\"./", "href=\"./"] {
        let mut rest = shell;
        while let Some(idx) = rest.find(marker) {
            let tail = &rest[idx + marker.len()..];
            let end = tail.find('"').expect("quoted attribute");
            refs.push(tail[..end].to_string());
            rest = &tail[end..];
        }
    }
    refs
}

/// Without the trailing slash the bundle's `./assets/…` refs would resolve
/// against `/tools/`, so the bare path redirects.
#[tokio::test]
async fn secrets_ui_bare_path_redirects() {
    let got = get("/tools/secrets").await;
    assert_eq!(got.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(got.header("location"), Some("/tools/secrets/"));
}

#[tokio::test]
async fn secrets_ui_index_serves_the_page_with_the_bridge_base_injected() {
    let got = get("/tools/secrets/").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(
        got.header(header::CONTENT_TYPE.as_str())
            .is_some_and(|v| v.starts_with("text/html")),
        "{:?}",
        got.headers
    );
    assert!(
        got.body.contains("<title>Trusty Secrets</title>"),
        "{}",
        got.body
    );
    assert!(
        got.body
            .contains(r#"window.__SECRETS_BASE__ = new URL("/api/secrets/", document.baseURI)"#),
        "the API base must point at the secrets bridge; got:\n{}",
        got.body
    );
    let injected = got.body.find("__SECRETS_BASE__").expect("global present");
    let head_close = got.body.find("</head>").expect("head close present");
    assert!(injected < head_close);
}

/// Every asset the shell references is served from this mount, or the page
/// renders blank.
#[tokio::test]
async fn secrets_ui_serves_every_asset_the_shell_references() {
    let shell = get("/tools/secrets/").await.body;
    let refs = local_refs(&shell);
    assert!(!refs.is_empty(), "the shell must reference a local asset");
    for r in refs {
        let got = get(&format!("/tools/secrets/{r}")).await;
        assert_eq!(got.status, StatusCode::OK, "asset {r} must be served");
        assert!(!got.body.is_empty(), "asset {r} came back empty");
    }
}

/// A path not in the bundle falls back to the shell.
#[tokio::test]
async fn secrets_ui_unknown_path_falls_back_to_the_shell() {
    let got = get("/tools/secrets/no/such/file").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(got.body.contains("<title>Trusty Secrets</title>"));
}

/// Why: a wrong `SpaMount` constant would serve one page at another's URL,
/// and its calls would reach the wrong bridge.
/// Test: this is the test.
#[tokio::test]
async fn the_secrets_mount_does_not_shadow_the_other_mounts() {
    let secrets = get("/tools/secrets/").await.body;
    let analyze = get("/tools/analyze/").await.body;
    let memory = get("/tools/memory/").await.body;
    let search = get("/tools/search/").await.body;
    let console = get("/").await.body;
    assert!(secrets.contains("__SECRETS_BASE__"));
    for other in ["__ANALYZE_BASE__", "__MEMORY_BASE__", "__SEARCH_BASE__"] {
        assert!(!secrets.contains(other), "secrets shell carries {other}");
    }
    for (name, body) in [
        ("analyze", &analyze),
        ("memory", &memory),
        ("search", &search),
        ("console", &console),
    ] {
        assert!(
            !body.contains("__SECRETS_BASE__"),
            "{name} carries the secrets base"
        );
        assert!(!body.contains("<title>Trusty Secrets</title>"), "{name}");
    }
    // The bridge keeps its own prefix: a page path is not an API route.
    let api = get("/api/secrets/session").await;
    assert_ne!(api.status, StatusCode::OK);
    assert!(!api.body.contains("Trusty Secrets"));
}

/// A GET of `path` from a client at `peer`, as the tailnet listener's
/// `ConnectInfo<SocketAddr>` reports it.
async fn get_from(path: &str, peer: &str) -> Got {
    let mut request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request");
    let peer: std::net::SocketAddr = peer.parse().expect("peer");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    send(request).await
}

/// Assert `got` forbids framing (#9067 P3).
fn assert_unframeable(path: &str, got: &Got) {
    assert_eq!(
        got.header("content-security-policy"),
        Some("frame-ancestors 'none'"),
        "{path}: {:?}",
        got.headers
    );
    assert_eq!(got.header("x-frame-options"), Some("DENY"), "{path}");
}

/// Why: #9067 P3 — another site must not frame the Secrets page, so every
/// `/tools/secrets/` response carries `frame-ancestors 'none'` and
/// `X-Frame-Options: DENY`: the redirect, the shell, each asset, the
/// fallback, a wrong method and the refusal page. Red until the page's
/// layer sets them.
/// Test: itself.
#[tokio::test]
async fn every_secrets_page_response_forbids_framing() {
    let shell = get("/tools/secrets/").await;
    let mut paths = vec![
        "/tools/secrets".to_owned(),
        "/tools/secrets/".to_owned(),
        "/tools/secrets/no/such/file".to_owned(),
    ];
    paths.extend(
        local_refs(&shell.body)
            .into_iter()
            .map(|r| format!("/tools/secrets/{r}")),
    );
    for path in &paths {
        assert_unframeable(path, &get(path).await);
    }
    let post = send(
        Request::builder()
            .method("POST")
            .uri("/tools/secrets/")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_unframeable("POST /tools/secrets/", &post);
    let refused = get_from("/tools/secrets/", "100.64.0.7:50000").await;
    assert_unframeable("tailnet /tools/secrets/", &refused);
}

/// Why: #9067 P5 — until S3b the page is refused to any client that is not
/// on loopback, before it is served: a fixed 403 page, with no page bytes
/// and no injected base. A loopback client still gets the page. Red until
/// the page's layer refuses it.
/// Test: itself.
#[tokio::test]
async fn a_non_loopback_client_gets_a_403_page_for_the_secrets_page() {
    for path in [
        "/tools/secrets",
        "/tools/secrets/",
        "/tools/secrets/index.html",
        "/tools/secrets/no/such/file",
    ] {
        for peer in [
            "100.64.0.7:50000",
            "192.168.1.20:50000",
            "[fd7a:115c::1]:50000",
        ] {
            let got = get_from(path, peer).await;
            assert_eq!(got.status, StatusCode::FORBIDDEN, "{path} from {peer}");
            assert!(
                got.header(header::CONTENT_TYPE.as_str())
                    .is_some_and(|v| v.starts_with("text/html")),
                "{path}: {:?}",
                got.headers
            );
            assert!(
                !got.body.contains("__SECRETS_BASE__"),
                "{path}: {}",
                got.body
            );
            assert!(!got.body.contains("./assets/"), "{path}: {}", got.body);
            assert!(got.body.contains("loopback"), "{path}: {}", got.body);
        }
    }
    let local = get_from("/tools/secrets/", "127.0.0.1:50000").await;
    assert_eq!(local.status, StatusCode::OK);
    assert!(local.body.contains("__SECRETS_BASE__"));
    // The other mounts are not refused by this layer.
    let analyze = get_from("/tools/analyze/", "100.64.0.7:50000").await;
    assert_eq!(analyze.status, StatusCode::OK);
}
