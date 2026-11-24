//! The Trusty Assistants page mount at `/tools/assistants/` (#11749).
//!
//! Why: the page is the console's sixth embedded bundle. It must be reachable
//! through the same mount pipeline as the other dashboards, its calls must go
//! to the gated `/api/console/assistants` routes, and it must not shadow or be
//! shadowed by another mount. Unlike the Secrets page it is served to a
//! tailnet peer too, so no loopback-only layer may sit on it.
//! What: drives `build_router` with the real embedded bundle: the bare-path
//! redirect, the shell with `__ASSISTANTS_BASE__` injected, every referenced
//! asset, the unknown-path fallback, a non-loopback peer and the other mounts.
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
async fn assistants_ui_bare_path_redirects() {
    let got = get("/tools/assistants").await;
    assert_eq!(got.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(got.header("location"), Some("/tools/assistants/"));
}

#[tokio::test]
async fn assistants_ui_index_serves_the_page_with_the_api_base_injected() {
    let got = get("/tools/assistants/").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(
        got.header(header::CONTENT_TYPE.as_str())
            .is_some_and(|v| v.starts_with("text/html")),
        "expected HTML, got {:?}",
        got.headers
    );
    assert!(got.body.contains("<title>Trusty Assistants</title>"));
    assert!(
        got.body.contains(
            r#"window.__ASSISTANTS_BASE__ = new URL("/api/console/assistants", document.baseURI)"#
        ),
        "the API base must point at the assistants routes; got:\n{}",
        got.body
    );
    let injected = got.body.find("__ASSISTANTS_BASE__").expect("global");
    assert!(injected < got.body.find("</head>").expect("head close"));
}

/// Every asset the shell references is served from this mount, or the page
/// renders blank.
#[tokio::test]
async fn assistants_ui_serves_every_asset_the_shell_references() {
    let shell = get("/tools/assistants/").await.body;
    let refs = local_refs(&shell);
    assert!(refs.iter().any(|r| r.starts_with("assets/")), "{refs:?}");
    for r in refs {
        let got = get(&format!("/tools/assistants/{r}")).await;
        assert_eq!(got.status, StatusCode::OK, "asset {r} must be served");
        assert!(!got.body.is_empty(), "asset {r} came back empty");
    }
}

/// A path not in the bundle falls back to the shell.
#[tokio::test]
async fn assistants_ui_unknown_path_falls_back_to_the_shell() {
    let got = get("/tools/assistants/no/such/file").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(got.body.contains("<title>Trusty Assistants</title>"));
}

/// Why: #11749 — the page is for a second device on the tailnet. A
/// loopback-only layer copied from the Secrets mount would refuse it there.
/// Test: this is the test.
#[tokio::test]
async fn assistants_ui_is_served_to_a_non_loopback_peer() {
    let mut request = Request::builder()
        .uri("/tools/assistants/")
        .body(Body::empty())
        .expect("request");
    let peer: std::net::SocketAddr = "192.0.2.7:52000".parse().expect("peer");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    let got = send(request).await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(got.body.contains("<title>Trusty Assistants</title>"));
}

/// Why: a wrong `SpaMount` constant would serve one page at another's URL,
/// and its calls would reach the wrong routes.
/// Test: this is the test.
#[tokio::test]
async fn the_assistants_mount_does_not_shadow_the_other_mounts() {
    let assistants = get("/tools/assistants/").await.body;
    assert!(assistants.contains("__ASSISTANTS_BASE__"));
    for other in [
        "__SECRETS_BASE__",
        "__ANALYZE_BASE__",
        "__MEMORY_BASE__",
        "__SEARCH_BASE__",
    ] {
        assert!(
            !assistants.contains(other),
            "assistants shell carries {other}"
        );
    }
    for path in [
        "/tools/secrets/",
        "/tools/analyze/",
        "/tools/memory/",
        "/tools/search/",
        "/",
    ] {
        let body = get(path).await.body;
        assert!(!body.contains("__ASSISTANTS_BASE__"), "{path}");
        assert!(!body.contains("<title>Trusty Assistants</title>"), "{path}");
    }
}
