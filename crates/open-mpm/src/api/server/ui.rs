//! Embedded web UI asset serving.
//!
//! Why: Shipping a single self-contained binary simplifies deployment — users
//! run `open-mpm --serve` and immediately get both the REST API and the web UI
//! without managing a separate static-file server or CDN.
//! What: `rust-embed` bakes `ui/dist/` into the binary; `serve_index` and
//! `serve_asset` return those bytes with appropriate MIME + cache headers,
//! falling back to `index.html` for client-side SPA routing.
//! Test: `cargo build && ./target/debug/open-mpm --serve --port 7654 &` then
//! `curl -s http://localhost:7654/ | grep -c 'app'` should return > 0.

use axum::http::{StatusCode, header};
use axum::response::IntoResponse;

/// Embed the Vite-built `ui/dist/` directory directly into the binary.
///
/// Why: Shipping a single self-contained binary simplifies deployment — users
/// run `open-mpm --serve` and immediately get both the REST API and the web UI
/// without managing a separate static-file server or CDN.
/// What: `rust-embed` walks `ui/dist/` at compile time and bakes every file
/// into the binary. At runtime `UiAssets::get(path)` returns the bytes.
/// Test: After `cargo build && ./target/debug/open-mpm --serve --port 7654 &`,
/// `curl -s http://localhost:7654/ | grep -c 'app'` should return > 0.
#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct UiAssets;

/// Serve `index.html` for the root path.
///
/// Why: SPA entry point — the browser loads this and the JS router takes over.
/// What: Fetches `index.html` from the embedded bundle and returns it with the
/// correct `text/html` content-type. Returns 404 with a plain-text message
/// when the UI was not compiled (i.e. `pnpm build` was skipped).
/// Test: GET `/` must return 200 with HTML containing the app mount point.
pub(super) async fn serve_index() -> impl IntoResponse {
    match UiAssets::get("index.html") {
        Some(f) => {
            let mime = mime_guess::from_path("index.html").first_or_octet_stream();
            // `index.html` references content-hashed asset URLs. Use
            // `no-store` (not just `no-cache`) so browsers never serve a
            // stale entry point pointing at asset hashes that no longer exist
            // after a redeploy — the root cause of persistent blank-screen
            // regressions when the Rust binary is rebuilt with a new UI dist.
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                    (header::CACHE_CONTROL, "no-store".to_owned()),
                ],
                f.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "UI not built").into_response(),
    }
}

/// Serve a static asset by path, falling back to `index.html` for unknown
/// paths so client-side routing works correctly.
///
/// Why: Vite emits hashed assets under `assets/`; the SPA also uses
/// client-side routing, so any unrecognised path should return `index.html`
/// and let the JS router resolve it — the standard SPA fallback pattern.
/// What: Looks up `path` in the embedded bundle; if found, returns the file
/// with a guessed MIME type; otherwise delegates to `serve_index`.
/// Test: GET `/assets/index-<hash>.js` should return 200 with
/// `content-type: text/javascript`. GET `/unknown-route` should return the
/// `index.html` content.
pub(super) async fn serve_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> axum::response::Response {
    // Axum's `/*path` catch-all includes the leading slash; strip it so the
    // path matches the keys stored by rust-embed (e.g. "assets/index.js").
    let path = path.trim_start_matches('/');
    match UiAssets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // Vite emits content-hashed filenames under `assets/` (e.g.
            // `assets/index-abc123.js`), so the bytes at a given URL never
            // change — safe to cache forever. Other top-level assets (favicon,
            // manifest, robots.txt) keep the default no-explicit-cache policy
            // so they pick up updates on the next request.
            if path.starts_with("assets/") {
                (
                    [
                        (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                        (
                            header::CACHE_CONTROL,
                            "public, max-age=31536000, immutable".to_owned(),
                        ),
                    ],
                    f.data.into_owned(),
                )
                    .into_response()
            } else {
                (
                    [(header::CONTENT_TYPE, mime.as_ref().to_owned())],
                    f.data.into_owned(),
                )
                    .into_response()
            }
        }
        None => serve_index().await.into_response(),
    }
}
