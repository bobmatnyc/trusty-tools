//! Axum HTTP server for the trusty-console.
//!
//! Why: The console needs a lightweight HTTP server that serves the embedded
//! SPA and a single JSON API route for service detection.
//! What: Builds an axum `Router` with `GET /health` (liveness probe),
//! `GET /api/console/services` (detect all services, return JSON array),
//! `GET /` and `GET /ui/*path` (serve the embedded Svelte SPA).
//! All logs go to stderr; stdout is clean.
//! Test: The `tests` module at the bottom starts the router in a real axum
//! test client and asserts `/api/console/services` returns valid JSON.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use rust_embed::RustEmbed;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::connector::ServiceConnector;

// ─── embedded UI ─────────────────────────────────────────────────────────────

/// Embedded Svelte SPA assets compiled by `build.rs`.
///
/// Why: Shipping the UI inside the binary eliminates external file dependencies
/// and matches the pattern used by trusty-search, trusty-memory, and
/// trusty-analyze.
/// What: rust-embed embeds every file under `ui/dist/` at compile time.
/// Test: The server tests assert that `GET /` returns 200.
#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct UiAssets;

// ─── app state ───────────────────────────────────────────────────────────────

/// Shared application state injected into every route handler.
///
/// Why: Connectors are created once at startup and reused for every request
/// so that there is no per-request allocation of the Vec.
/// What: Wraps the connector list in an `Arc` for cheap cloning.
/// Test: Constructed in `build_router`; exercised by the integration test.
#[derive(Clone)]
pub struct AppState {
    connectors: Arc<Vec<Box<dyn ServiceConnector>>>,
}

impl AppState {
    /// Create a new `AppState` from a list of connectors.
    ///
    /// Why: Lets tests inject a custom connector list.
    /// What: Wraps `connectors` in `Arc`.
    /// Test: Used in `build_router` and directly in `tests`.
    pub fn new(connectors: Vec<Box<dyn ServiceConnector>>) -> Self {
        Self {
            connectors: Arc::new(connectors),
        }
    }
}

// ─── router ──────────────────────────────────────────────────────────────────

/// Build the axum `Router` with all routes wired.
///
/// Why: Extracting the router into its own function allows both `main` and the
/// test harness to share the same routing configuration without running a real
/// TCP server.
/// What: Returns a `Router<()>` with CORS, tracing middleware, and the four
/// routes.
/// Test: Called from `tests::test_services_route_returns_json` below.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/console/services", get(services_handler))
        .route("/", get(spa_index_handler))
        .route("/ui", get(spa_index_handler))
        .route("/ui/", get(spa_index_handler))
        .route("/ui/{*path}", get(spa_asset_handler))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

// ─── handlers ────────────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
///
/// Why: Required by process monitors and the `trusty-console status` CLI
/// subcommand. Returns a minimal JSON body so callers can confirm the server
/// is up and which version is running.
/// What: Returns `{"status":"ok","version":"<CARGO_PKG_VERSION>"}`.
/// Test: Tested by `test_health_route` below.
async fn health_handler() -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// `GET /api/console/services` — detect all services and return JSON array.
///
/// Why: The Svelte SPA fetches this endpoint on load to render service cards.
/// What: Iterates the connector list, calls `detect()` on each, serialises the
/// results to JSON. detect() is CPU-bound but fast (file reads + TCP probes);
/// run in a blocking task to avoid blocking the async runtime. A panic in the
/// blocking task is surfaced as HTTP 500 rather than silently returning an
/// empty list (which would be indistinguishable from "no services installed").
/// Test: `test_services_route_returns_json` and
/// `test_services_handler_returns_500_on_panic` below.
async fn services_handler(State(state): State<AppState>) -> axum::response::Response {
    let connectors = state.connectors.clone();
    match tokio::task::spawn_blocking(move || {
        connectors.iter().map(|c| c.detect()).collect::<Vec<_>>()
    })
    .await
    {
        Ok(infos) => axum::Json(infos).into_response(),
        Err(e) => {
            tracing::error!("service detection task panicked: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /` — serve the SPA index.html.
///
/// Why: The root path must return the SPA shell so the browser bootstraps.
/// What: Reads `index.html` from the embedded asset set.
/// Test: `test_spa_root_returns_html` below.
async fn spa_index_handler() -> impl IntoResponse {
    serve_asset("index.html")
}

/// `GET /ui/*path` — serve SPA static assets.
///
/// Why: Vite emits JS/CSS/assets under hashed filenames; all are embedded and
/// served from the `/ui/*` prefix.
/// What: Strips the leading `/ui/` from `path` and serves the matching asset.
/// Test: Indirectly covered by `test_spa_root_returns_html`.
async fn spa_asset_handler(Path(path): Path<String>) -> impl IntoResponse {
    let path = path.trim_start_matches('/');
    serve_asset(path)
}

/// Serve one asset from the embedded `UiAssets`.
///
/// Why: Centralises asset serving so both the index and asset routes share the
/// same content-type detection and 404 handling.
/// What: Looks up the path in `UiAssets`, infers the MIME type via
/// `mime_guess`, returns the bytes with the appropriate `Content-Type` header.
/// On a 404 serves `index.html` (SPA client-side routing).
/// Test: `test_spa_root_returns_html`.
fn serve_asset(path: &str) -> Response<Body> {
    match UiAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.to_vec()))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .expect("static response")
                })
        }
        None => {
            // SPA fallback: serve index.html for unknown paths so client-side
            // routing works when the user navigates directly to a subpath.
            match UiAssets::get("index.html") {
                Some(content) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html")
                    .body(Body::from(content.data.to_vec()))
                    .unwrap_or_else(|_| {
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::empty())
                            .expect("static response")
                    }),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("not found"))
                    .expect("static 404"),
            }
        }
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::connector::{ServiceInfo, ServiceStatus};

    /// A stub connector for tests — always returns a fixed `ServiceInfo`.
    struct StubConnector {
        id: &'static str,
        display_name: &'static str,
        status: ServiceStatus,
    }

    impl ServiceConnector for StubConnector {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.display_name
        }
        fn detect(&self) -> ServiceInfo {
            ServiceInfo {
                id: self.id.to_string(),
                display_name: self.display_name.to_string(),
                status: self.status.clone(),
                version: None,
                url: None,
            }
        }
    }

    fn make_test_state() -> AppState {
        AppState::new(vec![
            Box::new(StubConnector {
                id: "trusty-search",
                display_name: "Trusty Search",
                status: ServiceStatus::Running,
            }),
            Box::new(StubConnector {
                id: "trusty-memory",
                display_name: "Trusty Memory",
                status: ServiceStatus::Available,
            }),
            Box::new(StubConnector {
                id: "trusty-analyze",
                display_name: "Trusty Analyze",
                status: ServiceStatus::Absent,
            }),
        ])
    }

    async fn get_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
        resp.into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec()
    }

    /// Why: the services route must return a valid JSON array with one entry
    /// per connector, each containing `id`, `display_name`, and `status`.
    /// What: builds the router with stub connectors, issues GET
    /// /api/console/services, parses the response.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_route_returns_json() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = get_bytes(resp).await;
        let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body.len(), 3);

        assert_eq!(body[0]["id"], "trusty-search");
        assert_eq!(body[0]["status"], "running");
        assert_eq!(body[0]["display_name"], "Trusty Search");

        assert_eq!(body[1]["id"], "trusty-memory");
        assert_eq!(body[1]["status"], "available");

        assert_eq!(body[2]["id"], "trusty-analyze");
        assert_eq!(body[2]["status"], "absent");
    }

    /// Why: health endpoint must return 200 with `status: ok`.
    /// What: issues GET /health and checks the JSON body.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_health_route() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = get_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
    }

    /// Why: the root path must serve the embedded HTML (or placeholder).
    /// What: issues GET / and asserts 200 + text/html content-type.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_spa_root_returns_html() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
    }

    /// A connector whose `detect()` always panics — simulates a buggy plugin.
    struct PanicConnector;

    impl ServiceConnector for PanicConnector {
        fn id(&self) -> &'static str {
            "panic-svc"
        }
        fn display_name(&self) -> &'static str {
            "Panic Service"
        }
        fn detect(&self) -> ServiceInfo {
            panic!("intentional test panic from PanicConnector");
        }
    }

    /// Why: a panicking connector must not silently return HTTP 200 with an
    /// empty list — that is indistinguishable from "no services installed".
    /// The handler must return HTTP 500 so the UI can display an error state.
    /// What: builds the router with a PanicConnector, issues GET
    /// /api/console/services, asserts the response status is 500.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_handler_returns_500_on_panic() {
        let state = AppState::new(vec![Box::new(PanicConnector)]);
        let router = build_router(state);

        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
