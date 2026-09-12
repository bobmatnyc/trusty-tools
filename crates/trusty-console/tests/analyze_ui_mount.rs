//! The trusty-analyze SPA mount at `/tools/analyze/` (#6155).
//!
//! Why: #6287 deleted trusty-analyze's HTTP surface, and the `/ui` mount that
//! served this dashboard with it — the bundle stayed committed in a crate with
//! no Rust code referencing it. This mount is where it moves, so it has to be
//! proven reachable: shell, hashed assets, and the injected API base that
//! repoints every call at the console's `/api/analyze/` bridge.
//! What: drives `build_router` with the real embedded bundle and asserts each of
//! the three routes.
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
async fn analyze_ui_bare_path_redirects() {
    let (status, headers, _) = get("/tools/analyze").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    let location = headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .expect("redirect carries a Location");
    assert_eq!(location, "/tools/analyze/");
}

#[tokio::test]
async fn analyze_ui_index_serves_the_spa_with_the_bridge_base_injected() {
    let (status, headers, body) = get("/tools/analyze/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == header::CONTENT_TYPE.as_str() && v.starts_with("text/html")),
        "expected an HTML content type, got {headers:?}"
    );
    // #7589 branded this title; it stays the mount's identity check.
    assert!(body.contains("<title>Trusty Analyzer</title>"), "{body}");
    assert!(
        body.contains(r#"window.__ANALYZE_BASE__ = new URL("/api/analyze/", document.baseURI)"#),
        "the API base must be repointed at the console bridge; got:\n{body}"
    );
    // The injected classic script has to be inside <head>; the bundle's own
    // entry point is a deferred module, so it evaluates after either way.
    let injected = body.find("__ANALYZE_BASE__").expect("global present");
    let head_close = body.find("</head>").expect("head close present");
    assert!(injected < head_close);
}

/// The shell references its assets by relative path; every one of them must be
/// served from this mount, or the page renders blank.
#[tokio::test]
async fn analyze_ui_serves_every_asset_the_shell_references() {
    let (_, _, shell) = get("/tools/analyze/").await;
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
        let (status, headers, body) = get(&format!("/tools/analyze/{r}")).await;
        assert_eq!(status, StatusCode::OK, "asset {r} must be served");
        assert!(!body.is_empty(), "asset {r} came back empty");
        // #7590: only `assets/` carries a content hash in its filename, so only
        // it may be cached forever. The shell also references `favicon.svg`,
        // whose name never changes when its bytes do — an immutable header
        // there would pin a stale icon in every browser that saw the old one.
        if r.starts_with("assets/") {
            assert!(
                headers
                    .iter()
                    .any(|(k, v)| k == header::CACHE_CONTROL.as_str() && v.contains("immutable")),
                "content-hashed asset {r} should be cacheable; got {headers:?}"
            );
        } else {
            assert!(
                headers
                    .iter()
                    .any(|(k, v)| k == header::CACHE_CONTROL.as_str() && v.contains("no-cache")),
                "un-hashed asset {r} must revalidate; got {headers:?}"
            );
        }
    }
}

/// A path that is not in the bundle falls back to the shell, so a stale bookmark
/// lands on the app rather than a 404.
#[tokio::test]
async fn analyze_ui_unknown_path_falls_back_to_the_shell() {
    let (status, _, body) = get("/tools/analyze/no/such/file").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>Trusty Analyzer</title>"));
}

/// Why: three mounts serving from one binary is the whole risk this file exists
/// to close — a wrong `SpaMount` constant would serve one dashboard at another's
/// URL, and every API call would then reach the wrong daemon.
/// Test: this is the test.
#[tokio::test]
async fn the_three_tool_mounts_do_not_shadow_each_other() {
    let (_, _, analyze) = get("/tools/analyze/").await;
    let (_, _, memory) = get("/tools/memory/").await;
    let (_, _, search) = get("/tools/search/").await;
    assert!(analyze.contains("trusty-analyzer"));
    assert!(!analyze.contains("Trusty Memory") && !analyze.contains("Trusty Search"));
    assert!(analyze.contains("__ANALYZE_BASE__"));
    assert!(!analyze.contains("__MEMORY_BASE__") && !analyze.contains("__SEARCH_BASE__"));
    assert!(memory.contains("__MEMORY_BASE__") && !memory.contains("__ANALYZE_BASE__"));
    assert!(search.contains("__SEARCH_BASE__") && !search.contains("__ANALYZE_BASE__"));
}

/// The console's own SPA keeps `/`; the new mount must not have swallowed it.
#[tokio::test]
async fn console_spa_still_owns_its_own_routes() {
    let (status, _, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<title>trusty-analyzer</title>"),
        "/ must still serve the console SPA, not the analyze one"
    );
}
