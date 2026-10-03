//! Issue #8889: `POST /indexes/{id}/reindex` runs at most one reindex per index.
//!
//! Why: two overlapping reindexes of one index staged into one file, each
//! swapped the other's staging corpus away, and the index was left with no open
//! corpus. Before the fix a second request answered `queued: true`, replaced the
//! first run's progress entry, and queued behind it.
//! What: through the real HTTP handler. The index's per-index permit is held so
//! the first run cannot finish before the second request arrives; that makes
//! the overlap deterministic rather than a timing race. These tests use only
//! APIs that exist before the fix, so they fail at the assertion, not the build.
//! Test: this module IS the test.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use tokio::sync::RwLock;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::{index_semaphore, ReindexProgress, ReindexStatus};

use super::reindex_handlers::reindex_handler;
use super::state::SearchAppState;

/// Register one colocated index rooted at `root`, built by the production loader.
async fn state_with_index(id: &str, root: &Path) -> Arc<SearchAppState> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let mut entry = PersistedIndex::new(id.to_string(), root.to_path_buf());
    entry.colocated = true;
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    Arc::new(SearchAppState::new(registry))
}

/// One reindex request through the HTTP handler, flattened for assertions.
async fn request(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    reindex_handler(State(Arc::clone(state)), AxumPath(id.to_string()), None)
        .await
        .map(|axum::Json(body)| body)
        .map_err(|(status, axum::Json(body))| (status, body))
}

/// The progress entry the accepted run published.
fn published_progress(state: &SearchAppState, id: &IndexId) -> Arc<ReindexProgress> {
    state
        .reindex_progress
        .get(id)
        .map(|entry| Arc::clone(entry.value()))
        .expect("an accepted reindex publishes its progress entry")
}

/// Wait, bounded, for `progress` to leave `Running`.
async fn wait_terminal(progress: &ReindexProgress) -> ReindexStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = progress.status.load();
        if status != ReindexStatus::Running || Instant::now() > deadline {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Set `path`'s Unix permission bits.
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Assert `refused` is the #8889 conflict naming the HTTP run.
fn assert_refused_as_running(
    refused: Result<serde_json::Value, (StatusCode, serde_json::Value)>,
    id: &str,
) {
    let (status, body) = refused.expect_err("#8889: a second concurrent reindex must be refused");
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "reindex_already_running");
    assert_eq!(body["index_id"], id);
    assert_eq!(body["running"]["origin"], "http");
    assert_eq!(body["queued"], false);
    assert_eq!(body["retryable"], true);
    assert_eq!(body["stream_url"], format!("/indexes/{id}/reindex/stream"));
}

/// Why (#8889 a): exactly one reindex of an index may run.
/// What: with the first run parked on the per-index permit, a second request is
/// refused with `409` naming the running job, and the first run's progress
/// entry — its SSE stream — is left in place.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_reindex_request_is_refused_while_the_first_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha() -> u32 { 1 }\n").expect("fixture");
    let id = "overlap-8889";
    let state = state_with_index(id, dir.path()).await;
    let index_id = IndexId::new(id);
    let hold = index_semaphore(&index_id)
        .acquire_owned()
        .await
        .expect("permit");

    let first = request(&state, id)
        .await
        .expect("the first request is accepted");
    assert_eq!(first["queued"], true);
    let first_progress = published_progress(&state, &index_id);

    assert_refused_as_running(request(&state, id).await, id);
    assert!(
        Arc::ptr_eq(&published_progress(&state, &index_id), &first_progress),
        "#8889: a refused request must not orphan the running job's stream"
    );

    drop(hold);
    wait_terminal(&first_progress).await;
}

/// Why (#8889 c): a failed reindex must not leave its index locked.
/// What: the index's storage directory is made read-only after registration, so
/// the first run cannot open a staging corpus and its incremental carryover
/// fails; while it is parked a second request is refused; once it has failed,
/// a third request is accepted within a bounded wait.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_reindex_releases_the_index_for_the_next_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha() -> u32 { 1 }\n").expect("fixture");
    let id = "failed-8889";
    let state = state_with_index(id, dir.path()).await;
    let storage = dir.path().join(".trusty-search");
    set_mode(&storage, 0o555);
    let index_id = IndexId::new(id);
    let hold = index_semaphore(&index_id)
        .acquire_owned()
        .await
        .expect("permit");

    request(&state, id)
        .await
        .expect("the first request is accepted");
    let first_progress = published_progress(&state, &index_id);
    assert_refused_as_running(request(&state, id).await, id);

    drop(hold);
    assert_eq!(
        wait_terminal(&first_progress).await,
        ReindexStatus::Failed,
        "precondition: a read-only storage directory fails the run"
    );
    set_mode(&storage, 0o755);

    let deadline = Instant::now() + Duration::from_secs(30);
    let third = loop {
        match request(&state, id).await {
            Ok(body) => break body,
            Err((StatusCode::CONFLICT, body)) if Instant::now() < deadline => {
                let _ = body;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err((status, body)) => panic!("#8889: the claim was never released: {status} {body}"),
        }
    };
    assert_eq!(third["queued"], true);
    wait_terminal(&published_progress(&state, &index_id)).await;
}
