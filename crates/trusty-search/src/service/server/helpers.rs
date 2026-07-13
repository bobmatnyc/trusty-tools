//! Shared helpers: root-path validation, chunk containment check, root_path
//! collision detection, and embedder status response builders.
//!
//! Why: These small helpers are called from multiple handler modules
//! (`indexes.rs`, `indexes_relocate.rs`, `search.rs`, `reindex_handlers.rs`);
//! centralising them avoids duplication and keeps the 500-line cap on each
//! handler file.
//! What: `validate_root_path`, `file_is_within_root`,
//! `find_root_path_collision`, `root_path_collision_response`,
//! `embedder_initializing_response`, `embedder_error_response`.
//! Test: `file_is_within_root_*`, `create_index_canonicalizes_*`,
//! `validate_root_path_denylist_*`, and `tests_2336.rs` tests.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};

use crate::core::registry::{IndexHandle, IndexId};

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

/// Find a registered index whose canonical `root_path` collides with
/// `candidate`, excluding `exclude_id` (issue #2336).
///
/// Why: `create_index_handler` and `relocate_index_handler` previously
/// guarded only *id* collisions, never *root_path* collisions. Two live
/// registrations sharing one colocated root resolve to the SAME on-disk
/// `<root>/.trusty-search/index.redb` — redb is a single-open database, so
/// the second registration's `build_indexer_from_entry` call would silently
/// fail its corpus open (`corpus_open_failed`) while the handler still
/// returned `200 {"created": true}` for the broken handle. This is the exact
/// runtime recreation of the #2305 double-registration hazard the warm-boot
/// dedup fixed only for the boot path. This guard rejects the registration
/// up front instead of building a broken indexer first.
/// What: `candidate` must already be canonicalized — every caller passes the
/// output of `validate_root_path`, the same normalization every registered
/// handle's `root_path` went through (directly at create/relocate time, or
/// via `canonicalize_best_effort` at warm-boot). Adversarial review (#2519)
/// found that plain `PathBuf` equality on canonicalized paths is NOT a
/// reliable identity check on case-insensitive-but-case-preserving
/// filesystems (macOS APFS default): `/Volumes/Data/Proj` and
/// `/Volumes/Data/PROJ` canonicalize to two different-looking strings that
/// nonetheless resolve to the SAME inode, so string equality misses the
/// collision entirely. The fix compares filesystem-truth identity —
/// `(dev, ino)` from `std::fs::metadata` — whenever both paths currently
/// exist on disk; this catches case variants AND other alias forms (bind
/// mounts, hard-linked directories) that resolve to one inode, at the cost of
/// one extra `stat(2)` per registered handle per call. When either path
/// doesn't exist (metadata lookup fails), the check falls back to plain
/// canonicalized-`PathBuf` equality — the pre-fix behavior. This fallback is
/// a deliberate fail-open/fail-closed tradeoff: it stays fail-closed for the
/// dominant, security-relevant case (the candidate root always exists —
/// `validate_root_path` already required `is_dir()` before this is called),
/// and only degrades to string equality for the exotic case of a handle
/// whose `root_path` has since vanished from disk (e.g. a device was
/// unmounted after registration), where dev/ino cannot be computed for one
/// side. Linear scan of `handles` (the registry is small — tens to low
/// hundreds of entries); returns the first colliding id, skipping
/// `exclude_id` (used by `relocate_index_handler` so an index is never
/// compared against itself).
/// Test: `create_index_rejects_duplicate_root_path`,
/// `create_index_same_id_same_root_is_idempotent_not_a_collision`,
/// `relocate_index_rejects_root_path_owned_by_another_index`,
/// `relocate_index_onto_own_current_root_is_not_a_collision`,
/// `create_index_concurrent_same_root_only_one_wins` in `tests_2336.rs`; plus
/// the macOS-gated `create_index_rejects_case_variant_of_registered_root`
/// exercising the dev/ino path directly.
pub(super) fn find_root_path_collision(
    handles: &[std::sync::Arc<IndexHandle>],
    candidate: &std::path::Path,
    exclude_id: Option<&IndexId>,
) -> Option<IndexId> {
    handles
        .iter()
        .find(|h| exclude_id != Some(&h.id) && identifies_same_root(&h.root_path, candidate))
        .map(|h| h.id.clone())
}

/// Decide whether `a` and `b` name the same on-disk root (issue #2519).
///
/// Why: see `find_root_path_collision`'s doc for the case-insensitive
/// filesystem hazard this closes.
/// What: prefers `(dev, ino)` identity via `same_filesystem_entry` when both
/// paths exist; falls back to canonicalized-`PathBuf` equality otherwise.
/// Test: covered transitively by `find_root_path_collision`'s test list.
fn identifies_same_root(a: &std::path::Path, b: &std::path::Path) -> bool {
    match same_filesystem_entry(a, b) {
        Some(same) => same,
        None => a == b,
    }
}

/// Compare `a` and `b` by `(dev, ino)`, returning `None` when either path's
/// metadata cannot be read (does not exist, permission denied, etc.) so the
/// caller can fall back to string equality.
///
/// Why: `stat(2)`-level identity is the only reliable way to tell whether two
/// path strings name the same file on a case-insensitive-but-case-preserving
/// filesystem (macOS APFS default) — canonicalize() preserves the case each
/// path was spelled with, it does not normalize case, so two differently-cased
/// spellings of the same directory canonicalize to two different strings.
/// What: unix-only (`dev`/`ino` via `std::os::unix::fs::MetadataExt`); no
/// portable equivalent is wired up for non-unix targets, so this always
/// returns `None` there and callers fall back to path equality.
/// Test: `create_index_rejects_case_variant_of_registered_root` (macOS-gated)
/// in `tests_2336.rs`.
#[cfg(unix)]
fn same_filesystem_entry(a: &std::path::Path, b: &std::path::Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let meta_a = std::fs::metadata(a).ok()?;
    let meta_b = std::fs::metadata(b).ok()?;
    Some(meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino())
}

#[cfg(not(unix))]
fn same_filesystem_entry(_a: &std::path::Path, _b: &std::path::Path) -> Option<bool> {
    None
}

/// Build a `409 Conflict` response naming the index that already owns
/// `root_path` (issue #2336).
///
/// Why: the caller must be told WHICH existing index it collided with so it
/// can decide whether to reuse that index, delete it first, or pick a
/// distinct root — a bare 409 with no context forces the operator to
/// cross-reference `GET /indexes?details=true` by hand.
/// What: `409 { "error": "...", "existing_id": "..." }`.
/// Test: see `find_root_path_collision` test list above.
pub(super) fn root_path_collision_response(
    existing_id: &IndexId,
    root_path: &std::path::Path,
) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": format!(
                "root_path {:?} is already registered to index '{}'; two indexes cannot \
                 share one on-disk corpus (issues #2305, #2336)",
                root_path.display(),
                existing_id,
            ),
            "existing_id": existing_id.0,
        })),
    )
        .into_response()
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
