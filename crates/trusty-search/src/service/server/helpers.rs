//! Shared helpers: root-path validation, chunk containment check, and
//! embedder status response builders.
//!
//! Why: These small helpers are called from multiple handler modules
//! (`indexes.rs`, `search.rs`, `reindex_handlers.rs`); centralising them
//! avoids duplication and keeps the 500-line cap on each handler file.
//! What: `validate_root_path`, `file_is_within_root`,
//! `embedder_initializing_response`, `embedder_error_response`.
//! Test: `file_is_within_root_*` and `create_index_canonicalizes_*` tests.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};

#[allow(clippy::result_large_err)]
pub(super) fn validate_root_path(path: &std::path::Path) -> Result<std::path::PathBuf, Response> {
    if path.as_os_str().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "root_path is required and must not be empty"
            })),
        )
            .into_response());
    }
    if !path.is_absolute() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path must be absolute (got {:?}); relative paths \
                     would be resolved against the daemon's CWD which is \
                     not the caller's CWD",
                    path.display().to_string()
                ),
            })),
        )
            .into_response());
    }
    if !path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path {:?} does not exist or is not a directory",
                    path.display().to_string()
                ),
            })),
        )
            .into_response());
    }
    // Resolve any symlink components so the registry, walker, and persistence
    // layer all agree on the project's canonical identity. `is_dir()` returned
    // true above, so `canonicalize` should succeed; on the off chance it fails
    // (e.g. a TOCTOU unlink between the `is_dir` check and this call) we surface
    // a 400 with the underlying I/O error rather than fall back to the
    // un-canonicalized path — half-resolved paths are exactly what produced the
    // mismatch in the first place.
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path {:?} could not be canonicalized: {}",
                    path.display().to_string(),
                    e
                ),
            })),
        )
            .into_response()),
    }
}

/// Determine whether a chunk's stored `file` field falls within an index's
/// registered root.
///
/// Why: issue #64 — even with `validate_root_path` (#63) preventing future
/// misregistrations, a daemon that previously indexed under the wrong root
/// can have persisted chunks whose `file` paths point at a different
/// project. The search handler post-filters with this predicate so cross-
/// index bleed cannot leak through to clients.
/// Why (issue #541 update): the warm-boot canonicalization in `restore_one_index`
/// prevents the stale-root problem going forward; this predicate adds a
/// canonicalize fallback for absolute paths so that any residual mismatch
/// (e.g. chunks indexed before the fix, volume mount alias, macOS /private/var
/// ↔ /var) also never causes a valid result to be dropped.
/// What: returns `true` when `file` is either (a) a clean relative path
/// (no leading `/`, no `..` segments) — the normal case, since the reindex
/// walker stores chunk paths relative to the index root — or (b) an
/// absolute path that starts with `root` (cheap lexical check). If (b) fails
/// and the file path exists on disk, falls back to a canonicalized comparison
/// so symlink aliases never cause a false drop (approach (b) from issue #541
/// — only results that fail the cheap check pay the `canonicalize` syscall
/// cost). Everything else (relative path with `..`, absolute path pointing
/// genuinely elsewhere) returns `false`.
/// Test: `file_is_within_root_*` unit tests below; `file_is_within_root_symlinked_root`
/// covers the symlink-alias case added for #541.
pub(super) fn file_is_within_root(file: &str, root: &std::path::Path) -> bool {
    let p = std::path::Path::new(file);
    if p.is_absolute() {
        // Fast path: lexical prefix check — no syscalls.
        if p.starts_with(root) {
            return true;
        }
        // Slow-path fallback for symlink / alias mismatches (issue #541): only
        // pay the `canonicalize` cost for absolute-path results that failed the
        // cheap check (so the hot path for relative-path chunks is unaffected).
        //
        // Strategy: canonicalize the index root (resolves symlink aliases, macOS
        // /var ↔ /private/var, etc.), then check whether the stored file path
        // starts with that canonical root. We do NOT canonicalize the file path
        // itself because the file may have been deleted since indexing; we only
        // need the root to resolve correctly.
        let canonical_root = match std::fs::canonicalize(root) {
            Ok(r) => r,
            Err(_) => return false,
        };
        return p.starts_with(&canonical_root);
    }
    // Relative path: must not climb out via `..`. We accept `.` and any
    // forward-only sequence of components. Empty paths are rejected
    // defensively.
    if file.is_empty() {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Build a `503 Service Unavailable` response for handlers that require the
/// embedder before the background init task has finished.
///
/// Why: callers (CLI, MCP, integrators) need to distinguish "transient — try
/// again in a few seconds" from real failures. A standard 503 with a typed
/// JSON body lets `trusty-search index` retry, while exposing a clear
/// `embedder initializing` reason for human operators reading logs.
/// What: returns `(503, {"error": "embedder initializing, retry in a few seconds"})`.
/// Test: hit `POST /indexes` immediately after daemon boot; assert 503 and
/// JSON body shape.
pub(super) fn embedder_initializing_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "embedder initializing, retry in a few seconds"
        })),
    )
        .into_response()
}

/// Build a `503 Service Unavailable` response when the embedder background
/// init task has recorded a permanent failure (issue #121).
///
/// Why: previously a hung/failed init left the daemon stuck in
/// `"initializing"` forever, so retry loops in `trusty-search index` and
/// downstream clients spun indefinitely. Returning a typed error body with
/// the recorded message lets callers fail fast and surfaces the root cause
/// (e.g. "init timed out after 60s") in logs and CLI output.
/// What: returns `(503, {"error": "embedder init failed: <message>"})`.
/// Test: `create_index_returns_503_with_error_when_embedder_failed`.
pub(super) fn embedder_error_response(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": format!("embedder init failed: {message}"),
        })),
    )
        .into_response()
}
