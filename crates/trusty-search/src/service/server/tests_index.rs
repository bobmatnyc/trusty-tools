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
    // #4706: a root with no `.trusty-search/` either — neither layout exists.
    let root = std::path::PathBuf::from(format!("/nonexistent-root-{}", std::process::id()));
    let (disk, mtime) = index_disk_and_mtime(&id, &root);
    assert!(disk.is_none(), "missing dir yields no disk_bytes");
    assert!(mtime.is_none(), "missing dir yields no last_indexed");
}

/// Build a colocated `<root>/.trusty-search/` holding `bytes` bytes of corpus.
///
/// Deliberately writes the real filenames (`index.redb`, `hnsw.usearch`) so the
/// fixture matches what issue #403's layout actually produces.
fn colocated_root_with(bytes: usize) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp
        .path()
        .join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&dir).expect("create .trusty-search");
    std::fs::write(dir.join("index.redb"), vec![b'x'; bytes]).expect("write index.redb");
    tmp
}

/// #4706 — a populated colocated index reports its real bytes, not 0/null.
///
/// Why: this is the whole defect. `index_disk_and_mtime` measured only the
/// legacy global dir, which since #403 holds metadata while the live corpus
/// sits at `<root_path>/.trusty-search/`. Because the global dir still exists,
/// the helper took its `Some(dir_size_bytes(..))` branch and returned `0` — a
/// healthy 527 MB index reporting empty. Eleven such indexes were diagnosed as
/// broken and nearly deleted. Pre-fix this test fails behaviorally: the helper
/// ignored `root_path` entirely, so the colocated bytes were never in the sum
/// no matter which branch it took.
/// What: a root whose `.trusty-search/` holds a known payload, under an index
/// id with no global dir, must report at least that payload's size.
/// Test: this test.
#[test]
fn disk_bytes_sums_colocated_storage_not_just_the_legacy_dir() {
    const PAYLOAD: usize = 4096;
    let tmp = colocated_root_with(PAYLOAD);
    let id = format!("colocated-4706-{}", std::process::id());

    let (disk, mtime) = index_disk_and_mtime(&id, tmp.path());

    let bytes = disk.expect("a populated colocated index has a measurable size");
    assert!(
        bytes >= PAYLOAD as u64,
        "colocated corpus must be counted: reported {bytes} for a {PAYLOAD}-byte index \
         (0 or a legacy-only total is the #4706 defect)"
    );
    assert!(
        mtime.is_some(),
        "the colocated index.redb mtime is the freshness signal — null here is #4706's \
         other half, the one #878 papered over with an in-memory timestamp"
    );
}

/// #4706 — `None` is reserved for "neither layout exists".
///
/// Why: `null` must keep meaning "nothing on disk yet". If the fix had made the
/// helper always return `Some(sum)`, callers would lose the one honest signal
/// for a never-written index — trading a misleading `0` for a misleading `0`.
/// Test: this test.
#[test]
fn disk_bytes_is_none_only_when_neither_layout_exists() {
    let empty = tempfile::tempdir().expect("tempdir");
    let id = format!("neither-4706-{}", std::process::id());
    let (disk, mtime) = index_disk_and_mtime(&id, empty.path());
    assert!(
        disk.is_none(),
        "no global dir and no .trusty-search/ ⇒ null, not 0"
    );
    assert!(mtime.is_none());
}

/// #4706 — the newer of the two layouts wins the freshness reading.
///
/// Why: a migrated index has files in both places, and the stale global copy
/// must not shadow the live colocated write — that would report an index as
/// older than it is, which is the same class of misinformation as the size bug.
/// What: writes a colocated `index.redb` strictly later than a bare tempdir
/// stand-in and asserts the helper's per-directory selector is applied across
/// both, taking the max rather than the first.
/// Test: this test.
#[test]
#[serial_test::serial]
fn last_indexed_takes_the_newer_of_the_two_layouts() {
    const LEGACY_BYTES: usize = 128;
    const COLOCATED_BYTES: usize = 512;

    // Both layouts, for real: the legacy dir needs `TRUSTY_DATA_DIR` (the var
    // `persistence::data_dir` honours), which is process-wide — hence `#[serial]`. Without both present this test can
    // only prove the single-directory case its name does not claim.
    let data_dir = tempfile::tempdir().expect("data dir");
    // SAFETY: `#[serial]` serialises every env-mutating test in this binary.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir.path());
    }

    let id = format!("newer-4706-{}", std::process::id());
    let legacy = data_dir
        .path()
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(&id));
    std::fs::create_dir_all(&legacy).expect("create legacy dir");
    std::fs::write(legacy.join("index.redb"), vec![b'L'; LEGACY_BYTES]).expect("legacy redb");

    // Strictly newer colocated write — the state a migrated index is in, where
    // the stale global copy must NOT win the freshness reading.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let tmp = colocated_root_with(COLOCATED_BYTES);
    let colocated_redb = tmp
        .path()
        .join(crate::service::colocated_storage::COLOCATED_DIR_NAME)
        .join("index.redb");
    let expected = std::fs::metadata(&colocated_redb)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .expect("colocated redb mtime");

    let (disk, mtime) = index_disk_and_mtime(&id, tmp.path());

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    assert_eq!(
        mtime.as_deref(),
        Some(expected.as_str()),
        "the NEWER colocated write must win over the older legacy copy"
    );
    assert_eq!(
        disk,
        Some((LEGACY_BYTES + COLOCATED_BYTES) as u64),
        "both layouts must be summed, not picked between"
    );
}

/// #4706 review — a `.trusty-search/` with no `index.redb` is not a corpus.
///
/// Why: `$HOME/.trusty-search/` is the daemon's OWN runtime directory — it
/// holds `http_addr` and `mcp_http_addr`. An index rooted at `$HOME` would
/// otherwise report the daemon's runtime files as its corpus. The directory
/// name alone is a coincidence; `index.redb` is what makes it storage.
/// Test: this test.
#[test]
fn colocated_dir_without_a_redb_is_not_counted_as_a_corpus() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp
        .path()
        .join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&dir).expect("create .trusty-search");
    // Exactly the daemon runtime files, no corpus.
    std::fs::write(dir.join("http_addr"), b"127.0.0.1:7878").expect("http_addr");
    std::fs::write(dir.join("mcp_http_addr"), b"127.0.0.1:7879").expect("mcp_http_addr");

    let id = format!("runtime-dir-4706-{}", std::process::id());
    let (disk, mtime) = index_disk_and_mtime(&id, tmp.path());
    assert!(
        disk.is_none(),
        "the daemon's runtime dir must not be counted as an index corpus, got {disk:?}"
    );
    assert!(mtime.is_none());
}

/// #4706 review — the mtime-only helper agrees with the full one.
///
/// Why: `search_handler` was paying a recursive two-directory size walk per
/// query for a byte count it discarded. Splitting the mtime path out is only
/// safe if it reports the identical value; this pins that equivalence so the
/// two cannot drift.
/// Test: this test.
#[test]
fn last_indexed_only_matches_the_mtime_from_the_full_helper() {
    let tmp = colocated_root_with(256);
    let id = format!("split-4706-{}", std::process::id());

    let (_disk, full) = index_disk_and_mtime(&id, tmp.path());
    let mtime_only = crate::service::server::status::index_last_indexed(&id, tmp.path());

    assert!(full.is_some(), "fixture must produce an mtime");
    assert_eq!(
        mtime_only, full,
        "the search path's cheaper helper must not report a different freshness"
    );
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
            follow_links: None,
            lexical_only: None,
            skip_kg: None,
            skip_vector: None,
            defer_embed: None,
            extra_skip_dirs: None,
            data_file_max_bytes: None,
            allow_sensitive_path: false,
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
            follow_links: None,
            lexical_only: None,
            skip_kg: None,
            skip_vector: None,
            defer_embed: None,
            extra_skip_dirs: None,
            data_file_max_bytes: None,
            allow_sensitive_path: false,
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
///
/// Note: `tempfile::tempdir()` creates dirs under `/tmp/` which is now in
/// the sensitive-root denylist. This test uses
/// `super::test_support::allowlisted_index_root` (see that module's doc
/// comment for why a `target/`-relative dir alone is not enough) so that
/// `validate_root_path` accepts the directory while still exercising the
/// symlink-canonicalization logic. `TempDir` provides RAII cleanup even on
/// panic, ensuring no leaked directories.
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

    // Create the real directory under an allowlist-safe base. TempDir
    // provides RAII cleanup even on panic.
    let (real_dir, real_root) = super::test_support::allowlisted_index_root("ts-symlink-real-");

    // The symlink lives alongside the real dir (same allowlist-safe base).
    let base = real_dir
        .path()
        .parent()
        .expect("allowlisted root has a parent")
        .to_path_buf();
    let link_path = base.join(format!("ts-symlink-link-{}", std::process::id()));
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
            follow_links: None,
            lexical_only: None,
            skip_kg: None,
            skip_vector: None,
            defer_embed: None,
            extra_skip_dirs: None,
            data_file_max_bytes: None,
            allow_sensitive_path: false,
        }),
    )
    .await;
    let _ = std::fs::remove_file(&link_path); // cleanup symlink (TempDir drops real_dir)
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
///
/// Note: uses `super::test_support::allowlisted_index_root` instead of
/// `tempfile::tempdir()` (which creates dirs under `/tmp/`, now in the
/// sensitive-root denylist). `TempDir` provides RAII cleanup even on panic
/// — no leaked directories.
#[tokio::test]
async fn create_index_accepts_valid_absolute_root_path() {
    use crate::core::registry::IndexRegistry;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    // TempDir under an allowlist-safe base — RAII cleanup on drop.
    let (_test_dir, test_root) = super::test_support::allowlisted_index_root("ts-valid-abs-");

    let resp = create_index_handler(
        State(Arc::clone(&state_arc)),
        Json(CreateIndexRequest {
            id: "valid-abs".into(),
            root_path: test_root,
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            follow_links: None,
            lexical_only: None,
            skip_kg: None,
            skip_vector: None,
            defer_embed: None,
            extra_skip_dirs: None,
            data_file_max_bytes: None,
            allow_sensitive_path: false,
        }),
    )
    .await;
    // _test_dir is dropped here → RAII cleanup
    assert_eq!(resp.status(), StatusCode::OK);
}
// Denylist tests live in `tests_denylist.rs` (split to keep this file ≤ 500 lines).
