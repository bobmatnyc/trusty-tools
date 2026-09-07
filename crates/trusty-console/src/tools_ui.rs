//! The tool dashboards the console serves under `/tools/<tool>/` (#6155).
//!
//! Why: ADR-0032 makes the console the only HTTP surface in the workspace, so
//! every trusty-* dashboard has to be reachable from this binary or not at all.
//! #6285 deletes trusty-search's HTTP server — including the `/ui` mount that
//! was the only way to its dashboard — #6286 already deleted trusty-memory's,
//! and #6287 deleted trusty-analyze's, leaving each SPA in a crate with no
//! listener. All three are served here instead.
//! What: `rust_embed` embeds three bundles, each the Vite output of one of this
//! crate's own UI projects — build.rs builds `ui-search/` → `ui-search-dist/`,
//! `ui-memory/` → `ui-memory-dist/` and `ui-analyze/` → `ui-analyze-dist/`
//! alongside `ui/`. Each gets three routes:
//!   - `GET /tools/<tool>`         → redirect to `/tools/<tool>/`
//!   - `GET /tools/<tool>/`        → index.html with the API base injected
//!   - `GET /tools/<tool>/{*path}` → static asset, index.html on a miss
//!
//! Each SPA's own `lib/base.js` reads an injected `window.__<TOOL>_BASE__` first,
//! so pointing every API call at the console's bridge is one injected global —
//! no fork of either SPA and no build-time base-path knob.
//! Test: `tests` below cover the injection and the path/MIME resolution;
//! `tests/search_ui_mount.rs`, `tests/memory_ui_mount.rs` and
//! `tests/analyze_ui_mount.rs` drive the routes through the real router.

use axum::{
    body::Body,
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use rust_embed::{EmbeddedFile, RustEmbed};

/// The search dashboard bundle, committed under `ui-search-dist/`.
///
/// Why: a published `trusty-console` tarball ships only what the crate's
/// `include` list names, so the bytes have to be committed inside this crate.
/// What: rust-embed embeds every file under `ui-search-dist/` at compile time.
/// build.rs rebuilds it from `ui-search/` whenever that source moves, `make -C
/// crates/trusty-console search-ui` forces the rebuild, and
/// `scripts/check-ui-bundle-freshness.sh` gates it (manifest row
/// `trusty-console-search`).
/// Test: `search_index_is_embedded` below.
#[derive(RustEmbed)]
#[folder = "ui-search-dist/"]
struct SearchUiAssets;

/// The memory dashboard bundle, committed under `ui-memory-dist/`.
///
/// Same arrangement as [`SearchUiAssets`], one manifest row over:
/// `trusty-console-memory`, rebuilt by `make -C crates/trusty-console memory-ui`.
/// Test: `memory_index_is_embedded` below.
#[derive(RustEmbed)]
#[folder = "ui-memory-dist/"]
struct MemoryUiAssets;

/// The analyze dashboard bundle, committed under `ui-analyze-dist/`.
///
/// Same arrangement as [`SearchUiAssets`], one manifest row over:
/// `trusty-console-analyze`, rebuilt by `make -C crates/trusty-console
/// analyze-ui`.
/// Test: `analyze_index_is_embedded` below.
#[derive(RustEmbed)]
#[folder = "ui-analyze-dist/"]
struct AnalyzeUiAssets;

/// One dashboard's mount: where its bytes are and where its API calls go.
///
/// Why a struct rather than two copies of three handlers: the redirect, the
/// shell and the asset lookup differ between the two dashboards only in the
/// bundle they read, the global they set and the prefix they set it to. A second
/// copy is how one mount's cache headers or fallback quietly stop matching the
/// other's.
struct SpaMount {
    /// The embedded bundle's lookup fn — `Assets::get`.
    get: fn(&str) -> Option<EmbeddedFile>,
    /// The `window.<name>` the SPA's `base.js` reads before deriving a base.
    base_global: &'static str,
    /// The console prefix that SPA's API calls resolve against.
    api_base: &'static str,
    /// What to tell an operator whose bundle is missing.
    remedy: &'static str,
}

/// Where the search SPA's API calls go when the console serves it.
///
/// Every request resolves against this prefix, which `search_uds::routes`
/// translates into one `search.*` RPC call on the daemon's socket:
/// `/api/search/health` reaches `search.health`.
const SEARCH: SpaMount = SpaMount {
    get: SearchUiAssets::get,
    base_global: "__SEARCH_BASE__",
    api_base: "/api/search/",
    remedy: "search dashboard assets not bundled — run `make -C crates/trusty-console search-ui`.",
};

/// Where the memory SPA's API calls go when the console serves it.
///
/// Same shape as [`SEARCH`], over `memory_uds::routes`: `/api/memory/health`
/// reaches `memory.health` and `/api/memory/sse` reaches
/// `memory.activity_stream`.
const MEMORY: SpaMount = SpaMount {
    get: MemoryUiAssets::get,
    base_global: "__MEMORY_BASE__",
    api_base: "/api/memory/",
    remedy: "memory dashboard assets not bundled — run `make -C crates/trusty-console memory-ui`.",
};

/// Where the analyze SPA's API calls go when the console serves it.
///
/// Same shape as [`SEARCH`], over `analyze_uds::routes`: `/api/analyze/health`
/// reaches `analyze.health` and `/api/analyze/indexes` reaches
/// `analyze.list_indexes`.
const ANALYZE: SpaMount = SpaMount {
    get: AnalyzeUiAssets::get,
    base_global: "__ANALYZE_BASE__",
    api_base: "/api/analyze/",
    remedy: "analyze dashboard assets not bundled — run `make -C crates/trusty-console analyze-ui`.",
};

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
    serve_index(&SEARCH)
}

/// `GET /tools/search/{*path}` — one bundle file, or the shell on a miss.
///
/// Why: the SPA routes on the URL fragment, so a deep link is always
/// `/tools/search/#/indexes` and resolves here as the index. The fallback
/// still matters for a stale bookmark or a hand-typed path.
/// What: looks `path` up in the embedded bundle; falls back to the shell.
/// Test: `search_ui_serves_every_asset_the_shell_references` covers the
/// asset-found arm and `search_ui_unknown_path_falls_back_to_the_shell` the
/// fallback, both in `tests/search_ui_mount.rs`.
pub async fn search_ui_asset(Path(path): Path<String>) -> Response {
    serve_asset(&SEARCH, &path)
}

/// `GET /tools/memory` — redirect to the trailing-slash form.
///
/// Same reason as [`search_ui_redirect`]: the bundle's asset refs are relative.
/// Test: `memory_ui_bare_path_redirects` in `tests/memory_ui_mount.rs`.
pub async fn memory_ui_redirect() -> Redirect {
    Redirect::permanent("/tools/memory/")
}

/// `GET /tools/memory/` — the SPA shell.
pub async fn memory_ui_index() -> Response {
    serve_index(&MEMORY)
}

/// `GET /tools/memory/{*path}` — one bundle file, or the shell on a miss.
///
/// Test: `memory_ui_serves_every_asset_the_shell_references` and
/// `memory_ui_unknown_path_falls_back_to_the_shell` in
/// `tests/memory_ui_mount.rs`.
pub async fn memory_ui_asset(Path(path): Path<String>) -> Response {
    serve_asset(&MEMORY, &path)
}

/// `GET /tools/analyze` — redirect to the trailing-slash form.
///
/// Same reason as [`search_ui_redirect`]: the bundle's asset refs are relative.
/// Test: `analyze_ui_bare_path_redirects` in `tests/analyze_ui_mount.rs`.
pub async fn analyze_ui_redirect() -> Redirect {
    Redirect::permanent("/tools/analyze/")
}

/// `GET /tools/analyze/` — the SPA shell.
pub async fn analyze_ui_index() -> Response {
    serve_index(&ANALYZE)
}

/// `GET /tools/analyze/{*path}` — one bundle file, or the shell on a miss.
///
/// Test: `analyze_ui_serves_every_asset_the_shell_references` and
/// `analyze_ui_unknown_path_falls_back_to_the_shell` in
/// `tests/analyze_ui_mount.rs`.
pub async fn analyze_ui_asset(Path(path): Path<String>) -> Response {
    serve_asset(&ANALYZE, &path)
}

/// Serve one bundle file, falling back to the shell when it is not one.
fn serve_asset(mount: &SpaMount, path: &str) -> Response {
    let trimmed = path.trim_start_matches('/');
    match (mount.get)(trimmed) {
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
        None => serve_index(mount),
    }
}

/// Read `index.html` out of the bundle and inject the API base into it.
fn serve_index(mount: &SpaMount) -> Response {
    let Some(index) = (mount.get)("index.html") else {
        return (StatusCode::NOT_FOUND, mount.remedy).into_response();
    };
    let html = String::from_utf8_lossy(index.data.as_ref());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(inject_api_base(
            &html,
            mount.base_global,
            mount.api_base,
        )))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Point the SPA's API calls at the console's bridge.
///
/// Why: served by its own daemon the SPA derives the API root from
/// `document.baseURI`; served here that would resolve `/health` to
/// `/tools/search/health`, which is this SPA's own shell. Each `base.js` checks
/// its `window.__*_BASE__` global ahead of that derivation, so setting the
/// global is the whole repoint.
/// What: inserts a classic `<script>` before `</head>` that resolves `api_base`
/// against `document.baseURI`. Resolving in the browser rather than baking in an
/// origin keeps the value correct whatever host and port the console was reached
/// on. Each bundle's own script tag is a deferred module, so a classic script
/// anywhere in the document runs before it — position inside `<head>` is not
/// load-bearing, but it matches how the daemons injected their own boot globals.
/// Test: `inject_api_base_lands_before_head_close`,
/// `inject_api_base_without_head`, `inject_api_base_escapes_the_base`.
fn inject_api_base(html: &str, global: &str, api_base: &str) -> String {
    let script = format!(
        "<script>\n\
         // #6155: the console bridges this tool's API under this prefix.\n\
         window.{global} = new URL({api_base:?}, document.baseURI).href;\n\
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

    /// Why: the memory bundle is built from this crate's `ui-memory/` since
    /// #6155; embedding the wrong tree would serve the search dashboard at
    /// `/tools/memory/` and every API call would go to the wrong daemon.
    /// Test: this is the test.
    #[test]
    fn memory_index_is_embedded() {
        let index = MemoryUiAssets::get("index.html").expect("bundle carries index.html");
        let html = String::from_utf8_lossy(index.data.as_ref());
        assert!(html.contains("Trusty Memory"), "wrong bundle embedded");
        assert!(
            html.contains("./assets/"),
            "bundle must use relative asset refs so the /tools/memory/ mount resolves them"
        );
    }

    #[test]
    fn inject_api_base_lands_before_head_close() {
        let html = "<html><head><title>x</title></head><body></body></html>";
        let out = inject_api_base(html, "__SEARCH_BASE__", "/api/search/");
        let script = out.find("__SEARCH_BASE__").expect("global injected");
        let head_close = out.find("</head>").expect("head close preserved");
        assert!(script < head_close, "script must sit inside <head>");
        assert!(out.contains(r#"new URL("/api/search/", document.baseURI)"#));
    }

    /// Why: the analyze bundle is built from this crate's `ui-analyze/` since
    /// #6155; embedding the wrong tree would serve another dashboard at
    /// `/tools/analyze/` and every API call would go to the wrong daemon.
    /// Test: this is the test.
    #[test]
    fn analyze_index_is_embedded() {
        let index = AnalyzeUiAssets::get("index.html").expect("bundle carries index.html");
        let html = String::from_utf8_lossy(index.data.as_ref());
        assert!(html.contains("trusty-analyzer"), "wrong bundle embedded");
        assert!(
            html.contains("./assets/"),
            "bundle must use relative asset refs so the /tools/analyze/ mount resolves them"
        );
    }

    /// Why: the three mounts must set DIFFERENT globals at different prefixes,
    /// or one dashboard's API calls reach another's daemon.
    /// Test: this is the test.
    #[test]
    fn each_mount_injects_its_own_global_and_prefix() {
        let html = "<html><head></head></html>";
        let search = inject_api_base(html, SEARCH.base_global, SEARCH.api_base);
        let memory = inject_api_base(html, MEMORY.base_global, MEMORY.api_base);
        let analyze = inject_api_base(html, ANALYZE.base_global, ANALYZE.api_base);
        assert!(search.contains(r#"window.__SEARCH_BASE__ = new URL("/api/search/""#));
        assert!(memory.contains(r#"window.__MEMORY_BASE__ = new URL("/api/memory/""#));
        assert!(analyze.contains(r#"window.__ANALYZE_BASE__ = new URL("/api/analyze/""#));
        assert!(!memory.contains("__SEARCH_BASE__"));
        assert!(!analyze.contains("__MEMORY_BASE__"));
    }

    #[test]
    fn inject_api_base_without_head() {
        let out = inject_api_base(
            "<html><body></body></html>",
            "__SEARCH_BASE__",
            "/api/search/",
        );
        assert!(out.starts_with("<script>"));
        assert!(out.contains("__SEARCH_BASE__"));
    }

    /// The injected literal is a Rust `{:?}` of the base, so a base carrying a
    /// quote cannot break out of the JS string.
    #[test]
    fn inject_api_base_escapes_the_base() {
        let out = inject_api_base(
            "<html><head></head></html>",
            "__SEARCH_BASE__",
            "/api/\"evil\"/",
        );
        assert!(out.contains(r#""/api/\"evil\"/""#));
        assert!(!out.contains(r#""/api/"evil"/""#));
    }

    #[test]
    fn cache_control_hashed_assets_are_immutable() {
        assert!(cache_control_for("assets/index-abc.js").contains("immutable"));
        assert_eq!(cache_control_for("index.html"), "no-cache");
    }
}
