//! The trusty-search SPA mount at `/tools/search/` (#6155).
//!
//! Why: #6285 deletes trusty-search's HTTP surface, and its `/ui` mount with
//! it. This mount is what the dashboard moves to, so it has to be proven
//! reachable — shell, hashed assets, and the injected API base that repoints
//! every call at the console's `/api/search/` proxy.
//! What: drives `build_router` with the real embedded bundle and asserts each
//! of the three routes.
//! Test: this file IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use trusty_console::server::{AppState, build_router};

async fn get(path: &str) -> (StatusCode, Vec<(String, String)>, String) {
    let router = build_router(AppState::new(vec![]));
    let resp = router
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router response");
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
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

/// Without the trailing slash the bundle's `./assets/…` refs would resolve
/// against `/tools/`, so the bare path must redirect rather than render.
#[tokio::test]
async fn search_ui_bare_path_redirects() {
    let (status, headers, _) = get("/tools/search").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    let location = headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .expect("redirect carries a Location");
    assert_eq!(location, "/tools/search/");
}

#[tokio::test]
async fn search_ui_index_serves_the_spa_with_the_proxy_base_injected() {
    let (status, headers, body) = get("/tools/search/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == header::CONTENT_TYPE.as_str() && v.starts_with("text/html")),
        "expected an HTML content type, got {headers:?}"
    );
    assert!(body.contains("<title>Trusty Search</title>"), "{body}");
    assert!(
        body.contains(r#"window.__SEARCH_BASE__ = new URL("/api/search/", document.baseURI)"#),
        "the API base must be repointed at the console proxy; got:\n{body}"
    );
    // The injected classic script has to be inside <head>; the bundle's own
    // entry point is a deferred module, so it evaluates after either way.
    let injected = body.find("__SEARCH_BASE__").expect("global present");
    let head_close = body.find("</head>").expect("head close present");
    assert!(injected < head_close);
}

/// The shell references its assets by relative path; every one of them must be
/// served from this mount, or the page renders blank.
#[tokio::test]
async fn search_ui_serves_every_asset_the_shell_references() {
    let (_, _, shell) = get("/tools/search/").await;
    let mut refs = Vec::new();
    for marker in ["src=\"./", "href=\"./"] {
        let mut rest = shell.as_str();
        while let Some(idx) = rest.find(marker) {
            let tail = &rest[idx + marker.len()..];
            let end = tail.find('"').expect("quoted attribute");
            refs.push(tail[..end].to_string());
            rest = &tail[end..];
        }
    }
    assert!(
        !refs.is_empty(),
        "the shell must reference at least one local asset"
    );
    for r in refs {
        let (status, headers, body) = get(&format!("/tools/search/{r}")).await;
        assert_eq!(status, StatusCode::OK, "asset {r} must be served");
        assert!(!body.is_empty(), "asset {r} came back empty");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == header::CACHE_CONTROL.as_str() && v.contains("immutable")),
            "content-hashed asset {r} should be cacheable; got {headers:?}"
        );
    }
}

/// A path that is not in the bundle falls back to the shell, so a stale
/// bookmark lands on the app rather than a 404.
#[tokio::test]
async fn search_ui_unknown_path_falls_back_to_the_shell() {
    let (status, _, body) = get("/tools/search/no/such/file").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>Trusty Search</title>"));
}

/// The console's own SPA keeps `/` and `/ui/*`; the new mount must not have
/// swallowed either.
#[tokio::test]
async fn console_spa_still_owns_its_own_routes() {
    let (status, _, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<title>Trusty Search</title>"),
        "/ must still serve the console SPA, not the search one"
    );
}
