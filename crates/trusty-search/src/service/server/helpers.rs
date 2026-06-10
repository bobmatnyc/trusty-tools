//! Shared helpers: root-path validation, chunk containment check, and
//! embedder status response builders.
//!
//! Why: These small helpers are called from multiple handler modules
//! (`indexes.rs`, `search.rs`, `reindex_handlers.rs`); centralising them
//! avoids duplication and keeps the 500-line cap on each handler file.
//! What: `validate_root_path`, `file_is_within_root`,
//! `embedder_initializing_response`, `embedder_error_response`.
//! Test: `file_is_within_root_*`, `create_index_canonicalizes_*`, and
//! `validate_root_path_denylist_*` tests.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};

/// Validate `path` as a safe, canonical root for indexing.
///
/// Why: Defense-in-depth for the daemon — even when the CLI-side check
/// (`commands/index.rs`) is bypassed (direct HTTP calls, MCP tools, scripts),
/// the daemon must refuse sensitive roots. The hard denylist in
/// `crate::allowlist::is_denied` is the authoritative gate; this function
/// applies it **after** canonicalization so symlink tricks or `..` traversals
/// cannot bypass the check.
///
/// Issue #829 (blocking canonicalize): the previous sync version called
/// `std::fs::canonicalize` and `path.is_dir()` directly on the tokio async
/// thread. Both are blocking syscalls that park the executor thread for the
/// duration of the kernel operation. Under load (many concurrent `POST /indexes`
/// requests or a network-backed filesystem that is slow to respond) this starves
/// the runtime. The fix: this function is now `async` and uses
/// `tokio::fs::canonicalize` (non-blocking, runs on the blocking pool) and
/// `tokio::fs::metadata` for the directory check.
///
/// What: in order — (1) rejects empty/non-absolute paths (no I/O); (2)
/// checks `is_dir` via `tokio::fs::metadata`; (3) canonicalizes via
/// `tokio::fs::canonicalize`; (4) calls `crate::allowlist::is_denied` on the
/// canonical path and returns 400 with the denial reason when matched.
/// Test: `validate_root_path_denylist_rejects_ssh`, `_rejects_home`,
/// `_rejects_tmp`, `_accepts_project_dir` in `tests_denylist.rs`.
#[allow(clippy::result_large_err)]
pub(super) async fn validate_root_path(
    path: &std::path::Path,
) -> Result<std::path::PathBuf, Response> {
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
                    path.display()
                ),
            })),
        )
            .into_response());
    }
    // Issue #829: use tokio::fs::metadata instead of path.is_dir() to avoid
    // blocking the async executor on a potentially slow filesystem probe.
    let is_dir = tokio::fs::metadata(path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false);
    if !is_dir {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "root_path {:?} does not exist or is not a directory",
                    path.display()
                ),
            })),
        )
            .into_response());
    }
    // Issue #829: use tokio::fs::canonicalize instead of std::fs::canonicalize.
    // This resolves symlinks on the blocking pool without parking an async thread.
    // `metadata` succeeded above so `canonicalize` should succeed; on the off
    // chance it fails (e.g. TOCTOU unlink) we surface a 400 instead of
    // falling back to the un-canonicalized path.
    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "root_path {:?} could not be canonicalized: {}",
                        path.display(),
                        e
                    ),
                })),
            )
                .into_response());
        }
    };
    // Hard denylist check on the canonical path — symlinks and `..` traversals
    // are already resolved above, so no bypass is possible.
    // Note (issue #822): this is the authoritative guard. The CLI-side
    // `is_denied` call in `commands/index.rs` is defense-in-depth that gives
    // a friendly early error before the daemon is contacted, but this check
    // enforces the policy for direct HTTP callers, MCP tool invocations, and
    // scripts that bypass the CLI. Both checks are intentional — neither is
    // redundant.
    if let Some(reason) = crate::allowlist::is_denied(&canonical) {
        tracing::warn!(
            path = %canonical.display(),
            %reason,
            "indexing refused: path matched hard denylist (issue #822)"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "indexing refused: {reason}"
                ),
            })),
        )
            .into_response());
    }
    Ok(canonical)
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
        // Issue #829 (blocking canonicalize): `std::fs::canonicalize` is a
        // blocking syscall. When called from an async handler (e.g. the search
        // retain loop) it parks the tokio executor thread. We use
        // `tokio::task::block_in_place` to move the blocking work off the
        // async scheduler cooperatively — this is safe inside a multi-thread
        // tokio runtime and avoids spawning a new OS thread for each call.
        // The result is the same; only the scheduling is non-blocking.
        //
        // Strategy: canonicalize the index root (resolves symlink aliases, macOS
        // /var ↔ /private/var, etc.), then check whether the stored file path
        // starts with that canonical root. We do NOT canonicalize the file path
        // itself because the file may have been deleted since indexing; we only
        // need the root to resolve correctly.
        let root_owned = root.to_path_buf();
        let canonical_root = tokio::task::block_in_place(|| std::fs::canonicalize(root_owned));
        let canonical_root = match canonical_root {
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
