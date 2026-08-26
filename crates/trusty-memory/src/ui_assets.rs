//! The embedded Svelte SPA, kept in the binary with nothing serving it yet
//! (#6286).
//!
//! Why the assets stay: ADR-0032 moves this dashboard onto `trusty-console`,
//! which mounts a sibling daemon's SPA by vendoring its `rust_embed` assets —
//! the disposition #6287 settled for trusty-analyze. That mount is a
//! fast-follow, and deleting `ui/dist/` now would mean restoring it then. The
//! files are git-tracked and `build.rs` regenerates them, so keeping the embed
//! costs binary size and nothing else.
//!
//! Why the handler is gone: `static_handler` was an axum fallback route and
//! `serve_embedded` built an `axum::response::Response`. Neither can exist
//! without the listener this crate no longer binds, and keeping them would have
//! meant keeping an axum dependency for code no request can reach.
//!
//! What is left: [`WebAssets`] and [`asset`], which answer "what is in the
//! bundle" and "give me these bytes and their content type" — the two questions
//! a console-side mount asks, with no framework in either.
//!
//! Test: `embedded_bundle_carries_an_index_html`.

use rust_embed::RustEmbed;

/// Embedded UI assets produced by `pnpm build` in `ui/`.
///
/// `build.rs` runs the Vite build before compilation, so the folder is always
/// populated. In this monorepo the UI lives inside the crate rather than at the
/// repo root, which is why the path is not the upstream `../../ui/dist/`.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/ui/dist/"]
pub struct WebAssets;

/// One embedded file: its bytes and the content type to serve it as.
#[derive(Debug, Clone)]
pub struct Asset {
    /// File contents.
    pub bytes: Vec<u8>,
    /// MIME type guessed from the extension.
    pub content_type: String,
}

/// Look up one embedded asset, defaulting an empty path to the SPA shell.
///
/// Why: hash-based routing lives client-side, so a mount serves `index.html`
/// for any path that is not a real file — but that fallback decision belongs to
/// whoever owns the route, not here. This answers only whether the file exists.
/// What: `None` when the path is not in the bundle; otherwise the bytes plus
/// the `mime_guess` content type.
/// Test: `embedded_bundle_carries_an_index_html`.
pub fn asset(path: &str) -> Option<Asset> {
    let path = if path.is_empty() { "index.html" } else { path };
    let file = WebAssets::get(path)?;
    Some(Asset {
        content_type: mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string(),
        bytes: file.data.into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the bundle is built by `build.rs` and embedded at compile time, so
    /// an empty `ui/dist/` produces a binary that compiles and serves nothing.
    /// The shell is what every SPA route falls back to, so its presence is the
    /// one thing worth asserting about the embed itself.
    /// Test: itself.
    #[test]
    fn embedded_bundle_carries_an_index_html() {
        let shell = asset("").expect("the SPA shell must be embedded");
        assert!(!shell.bytes.is_empty(), "index.html must not be empty");
        assert!(
            shell.content_type.starts_with("text/html"),
            "expected text/html, got {}",
            shell.content_type
        );
    }
}
