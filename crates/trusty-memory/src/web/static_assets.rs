//! Embedded Svelte SPA static asset serving.
//!
//! Why: Single-binary deploys with no separate static-file dance. `build.rs`
//! runs the Vite build before compilation so the `ui/dist/` folder is always
//! populated and embedded.
//! What: `WebAssets` embed struct (via `rust_embed`), `static_handler` axum
//! fallback handler, and `serve_embedded` helper.
//! Test: `serves_index_html_fallback` in `web::tests`.

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Embedded UI assets produced by `pnpm build` in `ui/`.
///
/// Why: Single-binary deploys with no separate static-file dance. `build.rs`
/// runs the Vite build before compilation so this folder is always populated.
/// What: All files under `ui/dist/` are included in the binary.
/// Test: `serves_index_html_fallback` confirms the SPA shell loads.
#[derive(RustEmbed)]
// Monorepo migration: upstream trusty-memory put the Svelte UI at the repo
// root (`ui/dist/`), so the original path was `$CARGO_MANIFEST_DIR/../../ui/dist/`.
// In the trusty-tools monorepo we keep the UI inside the crate to avoid
// polluting the workspace root with per-crate asset directories.
#[folder = "$CARGO_MANIFEST_DIR/ui/dist/"]
pub(super) struct WebAssets;

/// Serve any embedded asset; fall back to `index.html` for SPA routes.
///
/// Why: Hash-based routing lives client-side, but `/assets/foo.js` etc. must
/// resolve to the embedded file directly.
/// What: Looks up the request path under `WebAssets`; if absent, returns
/// `index.html`. Unknown paths under `/api/` return 404.
/// Test: `serves_index_html_fallback`, `unknown_api_returns_404`.
pub(super) async fn static_handler(req: Request<Body>) -> Response {
    let path = req.uri().path().trim_start_matches('/').to_string();

    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    serve_embedded(&path).unwrap_or_else(|| {
        // SPA fallback.
        serve_embedded("index.html")
            .unwrap_or_else(|| (StatusCode::NOT_FOUND, "ui assets missing").into_response())
    })
}

/// Look up an embedded asset by path and build a `Response` with the correct
/// `Content-Type`.
///
/// Why: Both the direct-path lookup and the SPA fallback share the same
/// response-building logic; extracting it prevents duplication.
/// What: Returns `None` when the path is not found in `WebAssets`; otherwise
/// wraps the bytes in a `Response` with `Content-Type` derived from the
/// file extension via `mime_guess`.
/// Test: Exercised via `static_handler` in `serves_index_html_fallback`.
pub(super) fn serve_embedded(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(asset.data.into_owned());
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Some(resp)
}
