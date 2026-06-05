//! Tests for disk/mtime helpers, resource fields, logs, admin, and create_index.
use super::admin::MAX_LOGS_TAIL_N;
use super::status::{first_existing_mtime_rfc3339, index_disk_and_mtime};
use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
#[test]
fn index_disk_and_mtime_handles_missing_dir() {
    let id = format!("nonexistent-index-{}", std::process::id());
    let (disk, mtime) = index_disk_and_mtime(&id);
    assert!(disk.is_none(), "missing dir yields no disk_bytes");
    assert!(mtime.is_none(), "missing dir yields no last_indexed");
}

/// Issue #80 — `first_existing_mtime_rfc3339` prefers `index.redb` over the
/// legacy `chunks.json`, and falls back to `chunks.json` when only it
/// exists.
///
/// Why: the redb cutover left `last_indexed` permanently `null` because the
/// selector read `chunks.json` (no longer rewritten) instead of the live
/// `index.redb`. This pins the precedence so a regression re-introducing
/// the JSON-only read is caught without standing up a daemon.
/// What: writes both files into a tempdir, asserts the returned mtime
/// matches `index.redb` (made strictly newer than `chunks.json`); then a
/// chunks.json-only dir returns that file's mtime.
/// Test: this test.
#[test]
fn last_indexed_prefers_redb_then_chunks_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // Legacy snapshot first (older), then the authoritative redb (newer).
    std::fs::write(dir.join("chunks.json"), b"[]").expect("write chunks.json");
    // Ensure a strictly later mtime for index.redb so the assertion that we
    // picked redb (not chunks.json) is unambiguous.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(dir.join("index.redb"), b"redb").expect("write index.redb");

    let redb_mtime = std::fs::metadata(dir.join("index.redb"))
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .expect("redb mtime");

    let got = first_existing_mtime_rfc3339(dir, &["index.redb", "chunks.json"]);
    assert_eq!(
        got.as_deref(),
        Some(redb_mtime.as_str()),
        "selector must prefer index.redb mtime over chunks.json"
    );

    // chunks.json-only fallback (un-migrated index).
    let tmp2 = tempfile::tempdir().expect("tempdir2");
    std::fs::write(tmp2.path().join("chunks.json"), b"[]").expect("write chunks.json");
    let fallback = first_existing_mtime_rfc3339(tmp2.path(), &["index.redb", "chunks.json"]);
    assert!(
        fallback.is_some(),
        "selector must fall back to chunks.json when index.redb is absent"
    );
}

/// Issue #80 — `first_existing_mtime_rfc3339` returns `None` when none of
/// the candidate files exist.
///
/// Why: a freshly-registered index has neither file; the selector must
/// degrade to `None` so the handler reports `last_indexed: null` rather
/// than panicking.
/// What: calls the selector against an empty tempdir and asserts `None`.
/// Test: this test.
#[test]
fn last_indexed_none_when_no_candidates_exist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let got = first_existing_mtime_rfc3339(tmp.path(), &["index.redb", "chunks.json"]);
    assert!(got.is_none(), "no candidate files → None");
}

/// Issue #38 — `/health` includes the `embedder_info` block once an
/// embedder is wired, and omits it otherwise.
///
/// Why: the admin UI's Health view renders the model dimension + provider
/// from this block; a BM25-only daemon (no embedder) must omit it so the
/// UI can show an honest "not available" state.
/// What: builds a BM25-only state, asserts `embedder_info` is `None`.
/// Test: this test.
#[tokio::test]
async fn health_omits_embedder_info_when_bm25_only() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let Json(resp) = health_handler(State(state)).await;
    assert!(
        resp.embedder_info.is_none(),
        "BM25-only daemon must omit embedder_info"
    );
}

/// Issue #35 — `GET /logs/tail` returns the most recent buffered lines.
///
/// Why: operators inspect a running daemon via this endpoint; it must
/// surface exactly what the shared `LogBuffer` holds and report `total`.
/// What: attaches a `LogBuffer`, pushes three lines, calls the handler
/// with `n=2`, and asserts the tail + `total` count.
/// Test: this test.
#[tokio::test]
async fn logs_tail_returns_recent_lines() {
    let buffer = trusty_common::log_buffer::LogBuffer::new(100);
    buffer.push("line one".to_string());
    buffer.push("line two".to_string());
    buffer.push("line three".to_string());
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_log_buffer(buffer));
    let Json(body) = logs_tail_handler(State(state), Query(LogsTailParams { n: 2 })).await;
    let lines = body["lines"].as_array().expect("lines array");
    assert_eq!(lines.len(), 2, "n=2 must return two lines");
    assert_eq!(lines[0].as_str(), Some("line two"));
    assert_eq!(lines[1].as_str(), Some("line three"));
    assert_eq!(body["total"].as_u64(), Some(3), "total counts all buffered");
}

/// Issue #35 — `GET /logs/tail?n=` is clamped to `[1, MAX_LOGS_TAIL_N]`.
///
/// Why: a misconfigured client must not be able to request more lines
/// than the buffer holds, and `n=0` must still return at least one line.
/// What: pushes one line, requests `n=0` and an oversized `n`, asserting
/// both clamp to a valid result.
/// Test: this test.
#[tokio::test]
async fn logs_tail_clamps_n() {
    let buffer = trusty_common::log_buffer::LogBuffer::new(100);
    for i in 0..5 {
        buffer.push(format!("l{i}"));
    }
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_log_buffer(buffer));
    // n=0 clamps up to 1.
    let Json(zero) =
        logs_tail_handler(State(Arc::clone(&state)), Query(LogsTailParams { n: 0 })).await;
    assert_eq!(zero["lines"].as_array().expect("lines").len(), 1);
    // n past MAX clamps down to the buffer length (5 here).
    let Json(big) = logs_tail_handler(
        State(state),
        Query(LogsTailParams {
            n: MAX_LOGS_TAIL_N * 10,
        }),
    )
    .await;
    assert_eq!(big["lines"].as_array().expect("lines").len(), 5);
}

/// Issue #35 — `POST /admin/stop` acknowledges the shutdown request.
///
/// Why: the response shape `{ ok, message }` is the documented contract
/// for the admin UI's stop button.
/// What: calls `admin_stop_handler` and asserts the JSON body. It does
/// NOT await the spawned exit task — that would terminate the test
/// process — but the 200 ms delay before `process::exit` guarantees the
/// test returns first.
/// Test: this test.
#[tokio::test]
async fn admin_stop_returns_ok() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let Json(body) = admin_stop_handler(State(state)).await;
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert_eq!(body["message"].as_str(), Some("shutting down"));
}

// ── Issue #63 / #64: root_path validation + cross-index bleed guards ──

/// Issue #63: a relative `root_path` must be rejected with `400` and a
/// helpful message — silently resolving it against the daemon's CWD is
/// the exact bug we are fixing.
#[tokio::test]
async fn create_index_rejects_relative_root_path() {
    use crate::core::registry::IndexRegistry;
    use axum::body::to_bytes;

    let state = SearchAppState::new(IndexRegistry::new());
    // Install a working embedder so we get past the readiness gate and
    // actually exercise the path validator.
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);
    let resp = create_index_handler(
        State(state_arc),
        Json(CreateIndexRequest {
            id: "rel-bad".into(),
            root_path: std::path::PathBuf::from("claude-mpm"),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
    assert!(err.contains("absolute"), "got: {err}");
}

/// Issue #63: an absolute-but-nonexistent `root_path` must also be
/// rejected. Prevents creating an index that points at a directory that
/// has not been created yet (the reindex walker would see no files,
/// silently producing an empty index named after a real project).
#[tokio::test]
async fn create_index_rejects_nonexistent_root_path() {
    use crate::core::registry::IndexRegistry;
    use axum::body::to_bytes;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);
    let resp = create_index_handler(
        State(state_arc),
        Json(CreateIndexRequest {
            id: "ghost".into(),
            root_path: std::path::PathBuf::from(
                "/this/path/should/never/exist/trusty-search-test-xyz",
            ),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
    assert!(err.contains("does not exist"), "got: {err}");
}

/// Issue (indexed-paths-mismatch): when the caller supplies a `root_path`
/// that is a symlink to a real directory, the handler must canonicalise
/// it before storing on the `IndexHandle`. Otherwise the registry holds
/// the symlink alias, the walker emits file paths under the alias, and
/// search queries from the canonical mount point return zero hits because
/// `file_is_within_root` won't match.
#[cfg(unix)]
#[tokio::test]
async fn create_index_canonicalizes_symlinked_root_path() {
    use crate::core::registry::IndexId;
    use crate::core::registry::IndexRegistry;
    use std::os::unix::fs::symlink;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    let tmp = tempfile::tempdir().expect("tempdir");
    let real_root = std::fs::canonicalize(tmp.path()).expect("canonicalize real root");
    let parent = real_root.parent().expect("tempdir has parent");
    let link_path = parent.join(format!(
        "trusty-search-server-symlink-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&link_path);
    symlink(&real_root, &link_path).expect("create symlink");

    let resp = create_index_handler(
        State(Arc::clone(&state_arc)),
        Json(CreateIndexRequest {
            id: "symlinked".into(),
            // Register via the SYMLINK path — the registry should still
            // store the CANONICAL path so search queries from either
            // alias resolve identically.
            root_path: link_path.clone(),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
        }),
    )
    .await;
    let _ = std::fs::remove_file(&link_path); // best-effort cleanup
    assert_eq!(resp.status(), StatusCode::OK);

    let handle = state_arc
        .registry
        .get(&IndexId::new("symlinked"))
        .expect("registered handle");
    assert_eq!(
        handle.root_path, real_root,
        "registry stored the symlink alias instead of the canonical path",
    );
    assert_ne!(
        handle.root_path, link_path,
        "registry retained the symlink alias — downstream walkers will mismatch",
    );
}

/// Issue #63: an absolute, existing directory must be accepted.
#[tokio::test]
async fn create_index_accepts_valid_absolute_root_path() {
    use crate::core::registry::IndexRegistry;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);
    let tmp = tempfile::tempdir().expect("tempdir");
    let resp = create_index_handler(
        State(Arc::clone(&state_arc)),
        Json(CreateIndexRequest {
            id: "valid-abs".into(),
            root_path: tmp.path().to_path_buf(),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
