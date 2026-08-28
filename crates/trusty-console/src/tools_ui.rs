//! The trusty-search SPA, served from the console under `/tools/search/`.
//!
//! Why: ADR-0032 makes the console the only HTTP surface in the workspace, and
//! #6285 deletes trusty-search's HTTP server — including the `/ui` mount that
//! is the only way to reach its dashboard today. The dashboard has to be
//! reachable somewhere else BEFORE that deletion, so the console carries the
//! bundle and serves it (#6155).
//! What: `rust_embed` embeds `ui-search-dist/`, a committed copy of the Vite
//! output from `crates/trusty-search/ui`. Three routes serve it:
//!   - `GET /tools/search`        → redirect to `/tools/search/`
//!   - `GET /tools/search/`       → index.html with the API base injected
//!   - `GET /tools/search/{*path}` → static asset, index.html on a miss
//!
//! The SPA's own `src/lib/base.js` reads `window.__SEARCH_BASE__` first, so
//! pointing every API call at the console's reverse proxy is one injected
//! global — no fork of the SPA, no build-time base-path knob, no change to
//! trusty-search.
//! Test: `tests` below cover the injection and the path/MIME resolution;
//! `tests/search_ui_mount.rs` drives the three routes through the real router.

use axum::{
    body::Body,
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use rust_embed::RustEmbed;

/// The trusty-search SPA bundle, committed under `ui-search-dist/`.
///
/// Why: a published `trusty-console` tarball cannot reach into a sibling crate,
/// so the bytes have to live inside this crate. `ui-search-dist/` is refreshed
/// by `make -C crates/trusty-console search-ui` and gated by
/// `scripts/check-ui-bundle-freshness.sh` (manifest row `trusty-console-search`).
/// What: rust-embed embeds every file under `ui-search-dist/` at compile time.
/// Test: `search_index_is_embedded` below.
#[derive(RustEmbed)]
#[folder = "ui-search-dist/"]
struct SearchUiAssets;

/// Where the SPA's API calls go when the console serves it.
///
/// Every request resolves against this prefix, which `proxy::proxy_handler`
/// forwards to the running trusty-search daemon: `/api/search/health` reaches
/// the daemon's `/health`, `/api/search/api/chat/providers` reaches its
/// `/api/chat/providers`. The chat lane rides the same path as every other
/// call — it is not carved out.
const SEARCH_API_BASE: &str = "/api/search/";

/// `GET /tools/search` — redirect to the trailing-slash form.
///
/// Why: the bundle is built with Vite `base: './'`, so `index.html` references
/// `./assets/…`. Served at `/tools/search` (no trailing slash) the browser
/// resolves those against `/tools/`, and every asset 404s. The redirect makes
/// the working form the only one a browser ever renders.
/// What: 308 to `/tools/search/`.
/// Test: `search_ui_bare_path_redirects` in `tests/search_ui_mount.rs`.
pub async fn search_ui_redirect() -> Redirect {
    Redirect::permanent("/tools/search/")
}

/// `GET /tools/search/` — the SPA shell.
pub async fn search_ui_index() -> Response {
    serve_search_index()
}

/// `GET /tools/search/{*path}` — one bundle file, or the shell on a miss.
///
/// Why: the SPA routes on the URL fragment, so a deep link is always
/// `/tools/search/#/indexes` and resolves here as the index. The fallback
/// still matters for a stale bookmark or a hand-typed path.
/// What: looks `path` up in the embedded bundle; falls back to the shell.
/// Test: `search_ui_serves_hashed_asset` in `tests/search_ui_mount.rs`.
pub async fn search_ui_asset(Path(path): Path<String>) -> Response {
    let trimmed = path.trim_start_matches('/');
    match SearchUiAssets::get(trimmed) {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                mime_guess::from_path(trimmed)
                    .first_or_octet_stream()
                    .as_ref(),
            )
            .header(header::CACHE_CONTROL, cache_control_for(trimmed))
            .body(Body::from(content.data.to_vec()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => serve_search_index(),
    }
}

/// Read `index.html` out of the bundle and inject the API base into it.
fn serve_search_index() -> Response {
    let Some(index) = SearchUiAssets::get("index.html") else {
        return (
            StatusCode::NOT_FOUND,
            "trusty-search UI assets not bundled — run `make -C crates/trusty-console search-ui`.",
        )
            .into_response();
    };
    let html = String::from_utf8_lossy(index.data.as_ref());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(inject_api_base(&html, SEARCH_API_BASE)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Point the SPA's API calls at the console's reverse proxy.
///
/// Why: served by its own daemon the SPA derives the API root from
/// `document.baseURI`; served here that would resolve `/health` to
/// `/tools/search/health`, which is this SPA's own shell. `base.js` checks
/// `window.__SEARCH_BASE__` ahead of that derivation, so setting the global is
/// the whole repoint.
/// What: inserts a classic `<script>` before `</head>` that resolves
/// `api_base` against `document.baseURI`. Resolving in the browser rather than
/// baking in an origin keeps the value correct whatever host and port the
/// console was reached on. The bundle's own script tag is a deferred module,
/// so a classic script anywhere in the document runs before it — position
/// inside `<head>` is not load-bearing, but keeping it there matches how
/// trusty-search injects its own boot globals.
/// Test: `inject_api_base_lands_before_head_close` and
/// `inject_api_base_without_head` below.
fn inject_api_base(html: &str, api_base: &str) -> String {
    let script = format!(
        "<script>\n\
         // #6155: the console proxies the search API under this prefix.\n\
         window.__SEARCH_BASE__ = new URL({api_base:?}, document.baseURI).href;\n\
         </script>"
    );
    match html.find("</head>") {
        Some(idx) => {
            let mut out = String::with_capacity(html.len() + script.len());
            out.push_str(&html[..idx]);
            out.push_str(&script);
            out.push_str(&html[idx..]);
            out
        }
        None => format!("{script}{html}"),
    }
}

/// Vite content-hashes everything under `assets/`, so those are immutable.
fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_index_is_embedded() {
        let index = SearchUiAssets::get("index.html").expect("bundle carries index.html");
        let html = String::from_utf8_lossy(index.data.as_ref());
        assert!(html.contains("Trusty Search"), "wrong bundle embedded");
        assert!(
            html.contains("./assets/"),
            "bundle must use relative asset refs so the /tools/search/ mount resolves them"
        );
    }

    #[test]
    fn inject_api_base_lands_before_head_close() {
        let html = "<html><head><title>x</title></head><body></body></html>";
        let out = inject_api_base(html, "/api/search/");
        let script = out.find("__SEARCH_BASE__").expect("global injected");
        let head_close = out.find("</head>").expect("head close preserved");
        assert!(script < head_close, "script must sit inside <head>");
        assert!(out.contains(r#"new URL("/api/search/", document.baseURI)"#));
    }

    #[test]
    fn inject_api_base_without_head() {
        let out = inject_api_base("<html><body></body></html>", "/api/search/");
        assert!(out.starts_with("<script>"));
        assert!(out.contains("__SEARCH_BASE__"));
    }

    /// The injected literal is a Rust `{:?}` of the base, so a base carrying a
    /// quote cannot break out of the JS string.
    #[test]
    fn inject_api_base_escapes_the_base() {
        let out = inject_api_base("<html><head></head></html>", "/api/\"evil\"/");
        assert!(out.contains(r#""/api/\"evil\"/""#));
        assert!(!out.contains(r#""/api/"evil"/""#));
    }

    #[test]
    fn cache_control_hashed_assets_are_immutable() {
        assert!(cache_control_for("assets/index-abc.js").contains("immutable"));
        assert_eq!(cache_control_for("index.html"), "no-cache");
    }
}
