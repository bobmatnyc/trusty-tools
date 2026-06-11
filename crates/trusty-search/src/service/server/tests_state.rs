//! Tests for embedder state, reindex cooldown, and index-status handlers.
use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
#[tokio::test]
async fn install_embedder_error_surfaces_in_health() {
    use crate::core::registry::IndexRegistry;

    let state = SearchAppState::new(IndexRegistry::new());
    state
        .install_embedder_error("init timed out after 60s")
        .await;
    let state_arc = Arc::new(state);
    let Json(resp) = health_handler(State(state_arc)).await;
    assert_eq!(resp.embedder, "error");
    assert_eq!(
        resp.embedder_error.as_deref(),
        Some("init timed out after 60s"),
    );
}

/// Issue #121: when the embedder init task recorded a permanent error,
/// `POST /indexes` must return a 503 carrying the error message rather
/// than the generic "initializing" reason. Callers (CLI, MCP) rely on
/// the message to surface the underlying cause to operators.
#[tokio::test]
async fn create_index_returns_503_with_error_when_embedder_failed() {
    use crate::core::registry::IndexRegistry;
    use axum::body::to_bytes;

    let state = SearchAppState::new(IndexRegistry::new());
    state
        .install_embedder_error("init timed out after 60s")
        .await;
    let state_arc = Arc::new(state);
    // Use a real non-denied directory so the `validate_root_path` guard
    // (issue #63 + index-denylist) accepts the path and the handler
    // proceeds to the embedder-error branch we're asserting on.
    // Note: `tempfile::tempdir()` creates dirs under /tmp which is now
    // in the sensitive-root denylist — use target/ under the workspace root.
    let base = std::env::current_dir().expect("cwd").join("target");
    std::fs::create_dir_all(&base).ok();
    let test_dir = tempfile::Builder::new()
        .prefix("ts-embedder-fail-")
        .tempdir_in(&base)
        .expect("create test_dir under target/");
    let resp = create_index_handler(
        State(state_arc),
        Json(CreateIndexRequest {
            id: "demo".to_string(),
            root_path: test_dir.path().to_path_buf(),
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
            defer_embed: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body_bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid json");
    let err_str = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        err_str.contains("embedder init failed"),
        "expected error message to mention init failure, got: {err_str}",
    );
    assert!(
        err_str.contains("init timed out after 60s"),
        "expected recorded timeout message to be surfaced, got: {err_str}",
    );
}

/// Issue #121: after the embedder is installed successfully, a previously
/// recorded error must be cleared so `/health` reports `"ready"` and not
/// `"error"` (e.g. if a retry succeeded after a transient failure).
#[tokio::test]
async fn install_embedder_clears_previous_error() {
    use crate::core::embed::MockEmbedder;
    use crate::core::registry::IndexRegistry;

    let state = SearchAppState::new(IndexRegistry::new());
    state.install_embedder_error("transient hang").await;
    // Verify the error is recorded.
    assert!(state.current_embedder_error().is_some());

    // Install a healthy embedder — the error must clear.
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    assert!(state.current_embedder_error().is_none());
    assert!(state.is_embedder_ready());

    let state_arc = Arc::new(state);
    let Json(resp) = health_handler(State(state_arc)).await;
    assert_eq!(resp.embedder, "ready");
    assert!(resp.embedder_error.is_none());
}

/// Issue #120: when the previous reindex for an index aborted at the
/// memory limit, a follow-up `POST /indexes/:id/reindex` request must be
/// refused with `429 Too Many Requests` for the duration of the cooldown.
///
/// Why: without the guard, an external caller (CLI watchdog, open-mpm)
/// that retries on abort would loop: each retry re-processes files that
/// had no content-hash entry yet, pushes RSS over the limit again, and
/// aborts again.
/// What: stages an index, records a memory-abort timestamp, calls
/// `reindex_handler` and asserts the 429 + JSON body shape. Then resets
/// the cooldown env to 0 s, removes the entry, and verifies the next
/// call queues successfully.
/// Test: this test.
#[tokio::test]
async fn reindex_handler_rejects_within_cooldown() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("cooldown-test");
    let tmp = tempfile::tempdir().expect("tempdir");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new("cooldown-test", tmp.path()))),
        tmp.path().to_path_buf(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    // Simulate a prior memory abort by writing a fresh timestamp.
    state
        .last_reindex_aborted_at
        .insert(id.clone(), std::time::Instant::now());

    // Default cooldown is 300 s — handler must refuse with 429.
    let result = reindex_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cooldown-test".to_string()),
        None,
    )
    .await;
    let err = result.expect_err("expected 429 inside cooldown window");
    assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
    let body = err.1 .0;
    assert!(body.get("retry_after_secs").is_some());
    assert!(body.get("hint").is_some());
    assert_eq!(body["index_id"], "cooldown-test");

    // Drop the abort entry and verify the next call queues successfully.
    state.last_reindex_aborted_at.remove(&id);
    let ok = reindex_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cooldown-test".to_string()),
        None,
    )
    .await
    .expect("queued");
    assert_eq!(ok.0["queued"], serde_json::Value::Bool(true));
}

/// Issue #120: the `AbortedMemory` variant must serialize to the
/// kebab-case-but-lowercase form (`"abortedmemory"`) consistent with the
/// existing `Complete`/`Failed`/`Running` variants. External callers
/// parse the status string off the SSE stream, so the wire format is
/// load-bearing.
/// Test: this test.
#[tokio::test]
async fn reindex_status_aborted_memory_serializes_lowercase() {
    let status = crate::service::reindex::ReindexStatus::AbortedMemory;
    let json = serde_json::to_string(&status).expect("serialize");
    assert_eq!(json, "\"abortedmemory\"");
}

/// Issue #80 — `GET /indexes/:id/status` reports `"indexing"` while a
/// reindex is `Running` and `"ready"` once it reaches a terminal state.
///
/// Why: the admin UI / MCP `index_status` consumers relied on a `status`
/// field that previously did not exist, so a long-running reindex looked
/// identical to an idle index. Mapping the live `ReindexStatus` lets
/// callers show an "indexing…" spinner and avoids reporting `"ready"`
/// mid-reindex.
/// What: stages a bare index, drives the per-index `ReindexProgress`
/// through `Running` then `Complete`, and asserts the handler's `status`
/// field flips from `"indexing"` to `"ready"`.
/// Test: this test.
#[tokio::test]
async fn index_status_reports_indexing_then_ready() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::reindex::{ReindexProgress, ReindexStatus};
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("status-test");
    let tmp = tempfile::tempdir().expect("tempdir");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new("status-test", tmp.path()))),
        tmp.path().to_path_buf(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    // No progress entry yet → idle index reports "ready".
    let Json(idle) = index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("status-test".to_string()),
    )
    .await
    .expect("status 200");
    assert_eq!(idle["status"], "ready", "idle index must report ready");

    // A Running reindex must surface "indexing".
    let progress = Arc::new(ReindexProgress::new()); // defaults to Running
    state.reindex_progress.insert(id.clone(), progress.clone());
    let Json(running) = index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("status-test".to_string()),
    )
    .await
    .expect("status 200");
    assert_eq!(
        running["status"], "indexing",
        "running reindex must report indexing"
    );

    // A terminal state maps back to "ready".
    progress.status.store(ReindexStatus::Complete);
    let Json(done) = index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("status-test".to_string()),
    )
    .await
    .expect("status 200");
    assert_eq!(
        done["status"], "ready",
        "completed reindex must report ready"
    );
}

/// Issue #35 — `GET /health` carries the enriched resource fields
/// (`rss_mb`, `rss_limit_mb`, `disk_bytes`, `cpu_pct`).
///
/// Why: external probes and the admin UI render these; the JSON contract
/// must remain stable. `rss_mb` is sampled live so it is asserted only
/// for presence, not an exact value.
/// What: builds a bare `SearchAppState`, calls `health_handler`, and
/// asserts every new field deserialises with a plausible value.
/// Test: this test.
#[tokio::test]
async fn health_includes_resource_fields() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let Json(resp) = health_handler(State(state)).await;
    // rss_mb is sampled from the live test process; tolerate 0 only in
    // sandboxes where /proc is restricted, but it must be a sane unit.
    assert!(resp.rss_mb < 1024 * 1024, "rss_mb unit must be MB");
    // cpu_pct is a non-negative percentage (first sample may be 0.0).
    assert!(resp.cpu_pct >= 0.0, "cpu_pct must be non-negative");
    // disk_bytes / rss_limit_mb are u64 — presence is the contract here;
    // the disk ticker has not run yet so disk_bytes is 0.
    assert_eq!(resp.disk_bytes, 0, "disk ticker has not ticked yet");
    let _ = resp.rss_limit_mb;
}

// ---------------------------------------------------------------------------
// Issue #1006 — accept-loop starvation tests
// ---------------------------------------------------------------------------

/// Issue #1006 — Option B: `try_current_embedder()` must return `None`
/// immediately when another task holds a write lock on `embedder_slot`
/// (i.e. `install_embedder` is in progress, e.g. during a 30 s CoreML stall).
///
/// Why: the health handler uses `try_current_embedder()` instead of
/// `current_embedder().await` so it never blocks when the embedder slot is
/// write-locked. This test is the mechanical proof that `try_read()` returns
/// `None` under contention rather than deadlocking.
/// What: acquires a write lock on `embedder_slot`, then calls
/// `try_current_embedder()` — must return `None` without blocking.
/// Test: this test.
#[tokio::test]
async fn health_non_blocking_when_embedder_slot_write_locked() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));

    // Acquire a write lock to simulate an in-progress install_embedder.
    let _write_guard = state.embedder_slot.write().await;

    // try_current_embedder must return None without blocking — if this
    // call were to `.await` the lock it would deadlock in a single-threaded
    // test context. The test passing proves non-blocking semantics.
    let result = state.try_current_embedder();
    assert!(
        result.is_none(),
        "try_current_embedder must return None when write lock is held"
    );

    // Verify health_handler also completes without deadlock while
    // the write lock is held (embedder_info block will be absent / None).
    let Json(resp) = health_handler(State(Arc::clone(&state))).await;
    assert_eq!(
        resp.status, "ok",
        "health must return ok even when embedder slot is write-locked"
    );
    // embedder_info is None because try_current_embedder returned None.
    assert!(
        resp.embedder_info.is_none(),
        "embedder_info must be absent when slot is write-locked (non-blocking fallback)",
    );
}

/// Issue #1006 — Option B: once an embedder is installed, `/health`
/// surfaces the embedder_info block via the non-blocking `try_current_embedder()`.
///
/// Why: `health_handler` must still provide `embedder_info` when the
/// embedder slot is available (the common steady-state path). This test
/// guards against accidentally always returning `None`.
/// What: installs a `MockEmbedder` (384-dim), calls `/health`, and asserts
/// `embedder_info` is present (non-None) and the response reports "ready".
/// Test: this test.
#[tokio::test]
async fn health_includes_embedder_info_when_ready() {
    use crate::core::embed::MockEmbedder;

    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    let Json(resp) = health_handler(State(state_arc)).await;
    assert_eq!(resp.embedder, "ready");
    assert!(
        resp.embedder_info.is_some(),
        "embedder_info must be present when embedder is installed and slot is uncontended"
    );
}

/// Issue #1006 — Option A: the tokio runtime builder must configure at
/// least 16 worker threads so the accept loop has room to run even when
/// embed-pool workers saturate the default `num_cpus` thread count.
///
/// Why: with only `num_cpus` workers (e.g. 8 on a 4-core machine) and
/// embed-pool tasks blocking on 30 s sidecar calls, the axum accept loop
/// is scheduled too rarely, causing health probes to time out.
/// What: verifies the `worker_thread_count` helper enforces the 16-thread
/// floor — specifically that a single-CPU machine is lifted to 16, and
/// that a 32-CPU machine is NOT clamped (returns 32). The helper is also
/// used in `main()` so this test guards any future removal of the floor.
/// Test: this test.
#[test]
fn worker_thread_count_at_least_16() {
    use crate::worker_thread_count;

    // Floor: a 1-CPU machine must produce exactly 16 workers.
    assert_eq!(
        worker_thread_count(1),
        16,
        "worker_thread_count(1) must return 16 (floor enforced)"
    );

    // Pass-through: a 32-CPU machine must produce exactly 32 workers.
    assert_eq!(
        worker_thread_count(32),
        32,
        "worker_thread_count(32) must return 32 (no artificial cap)"
    );

    // Verify the runtime can actually be built with the floor count.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_thread_count(1))
        .enable_all()
        .build()
        .expect("runtime builder must succeed with worker_thread_count(1) == 16");
    // rt is intentionally dropped immediately — we only needed to verify it builds.
    drop(rt);
}

// ── issue #1097 / #1090 atomicity: IndexRegistry::remove_and_get ──────────

/// `IndexRegistry::remove_and_get` returns the `Arc<IndexHandle>` and `true`
/// in one atomic DashMap operation, and the entry is gone afterward.
///
/// Why: the DELETE handler needs root_path for roots.toml cleanup AND must
/// unregister the handle in one step to avoid a TOCTOU race with concurrent
/// PATCH. This test is the unit proof that `remove_and_get` satisfies both
/// requirements simultaneously.
///
/// What: registers a bare handle, calls `remove_and_get`, asserts the
/// returned handle carries the expected `root_path`, and verifies that a
/// subsequent `get` returns `None`.
///
/// Test: this test (issue #1097 atomicity fix).
#[test]
fn registry_remove_and_get_returns_handle_atomically() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("atomic-test");
    let root = PathBuf::from("/projects/atomic-test");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new("atomic-test", &root))),
        root.clone(),
    ));

    let (removed, handle_opt) = registry.remove_and_get(&id);
    assert!(removed, "remove_and_get must report the entry existed");
    let h = handle_opt.expect("remove_and_get must return the handle when entry exists");
    assert_eq!(
        h.root_path, root,
        "returned handle must carry the correct root_path"
    );
    // After removal the entry must be gone.
    assert!(
        registry.get(&id).is_none(),
        "registry must not contain the entry after remove_and_get"
    );
}

/// `IndexRegistry::remove_and_get` on an unknown id returns `(false, None)`
/// without panicking.
///
/// Why: DELETE of a non-registered id must degrade gracefully.
/// Test: this test.
#[test]
fn registry_remove_and_get_returns_none_for_missing_id() {
    use crate::core::registry::{IndexId, IndexRegistry};

    let registry = IndexRegistry::new();
    let (removed, got) = registry.remove_and_get(&IndexId::new("ghost-index"));
    assert!(!removed, "remove_and_get must return false for unknown id");
    assert!(
        got.is_none(),
        "remove_and_get must return None for unknown id"
    );
}
