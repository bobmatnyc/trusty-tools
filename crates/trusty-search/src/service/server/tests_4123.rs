//! Regression tests for safe deregistration on `DELETE /indexes/{id}` (#4123).
//!
//! Why: the handler hardcoded `delete_data=true`, so every `DELETE` destroyed
//! the on-disk data directory and the HTTP surface offered no way to merely
//! deregister. Registry hygiene was therefore impossible through the API — an
//! operator clearing 49 stale entries had to stop the daemon and hand-edit
//! `indexes.toml`, because one mis-typed id would have destroyed a real
//! corpus. Two callers had already documented the safe semantics that did not
//! exist (the UI's "On-disk data is preserved." confirm dialog and `trusty-search
//! index remove`'s help), so the destructive default was also a live
//! documentation lie. These tests pin BOTH halves of the fix: the new default
//! preserves, and the destructive capability is still reachable.
//! What: drives the real router with `oneshot` so the `?delete_data=` query
//! extractor and the route wiring are exercised end-to-end, not just the
//! handler function. `TRUSTY_DATA_DIR` is redirected at a `TempDir` so
//! `remove_index_data_dir` / `remove_index_registry_entry` / `remove_root` all
//! act inside the sandbox; `#[serial_test::serial]` guards that process-wide
//! env var against the other tests that mutate it.
//! Test: this module. Run with `cargo test -p trusty-search tests_4123`.

use super::build_router;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::server::SearchAppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt;

/// Index id used by both tests. Deliberately `[A-Za-z0-9-]` only so
/// `persistence::sanitize_id` is the identity function and the on-disk
/// directory name is predictable.
const INDEX_ID: &str = "delete-data-4123";

/// Register `INDEX_ID` in a fresh router and materialise its app-data
/// directory with a marker file.
///
/// Returns the router plus the path of the data directory the DELETE may or
/// may not destroy. The marker file is what makes the assertions meaningful:
/// an empty directory could vanish for unrelated reasons, a directory holding
/// a known byte string cannot.
fn router_with_index_and_data(data_root: &std::path::Path) -> (axum::Router, std::path::PathBuf) {
    let registry = IndexRegistry::new();
    let root_path = data_root.join("corpus-root");
    std::fs::create_dir_all(&root_path).expect("create index root");
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(CodeIndexer::new(INDEX_ID, root_path.clone()))),
        root_path,
    ));
    let index_data_dir = data_root.join("indexes").join(INDEX_ID);
    std::fs::create_dir_all(&index_data_dir).expect("create index data dir");
    std::fs::write(index_data_dir.join("corpus.marker"), b"real corpus bytes")
        .expect("write marker");
    (build_router(SearchAppState::new(registry)), index_data_dir)
}

/// Issue the DELETE and return the decoded JSON body.
/// Deliberately NOT named `delete_index`: that name is an allowlisted dangling
/// doc-pointer in `mcp/tools/http.rs` (`Test: delete_index arm exercises this
/// path`), and a helper by that name silently satisfies it — converting a
/// KNOWN-dangling pointer into a falsely-resolved one and dropping the lint's
/// signal. This helper drives the HTTP route; it does not exercise that arm.
async fn send_delete_request(router: axum::Router, uri: &str) -> serde_json::Value {
    let resp = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "DELETE {uri} must succeed");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// Issue #4123: `DELETE /indexes/{id}` with NO parameter must deregister the
/// index while leaving its on-disk data directory intact.
///
/// Why: this is the safe deregistration verb the API never had. It is the
/// whole point of the issue — before the fix this test fails because the
/// marker file is destroyed along with the registration.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn delete_index_without_param_preserves_data() {
    // RAII rather than a bare set_var/remove_var pair: the assertions below
    // panic on failure, and an early unwind must not leave `TRUSTY_DATA_DIR`
    // pointing at a deleted tempdir — that would cascade this one failure into
    // every later serial test in the binary.
    let isolated = super::tests_components::IsolatedDataDir::new();
    let (router, index_data_dir) = router_with_index_and_data(isolated.path());
    let marker = index_data_dir.join("corpus.marker");

    let body = send_delete_request(router, &format!("/indexes/{INDEX_ID}")).await;

    assert_eq!(
        body["removed"], true,
        "the registration must still be removed: {body}"
    );
    assert_eq!(
        body["data_deleted"], false,
        "a bare DELETE must report that it preserved data: {body}"
    );
    assert!(
        marker.exists(),
        "bare DELETE must NOT destroy on-disk data; {} is gone",
        marker.display()
    );
    assert_eq!(
        std::fs::read(&marker).expect("read marker"),
        b"real corpus bytes",
        "preserved data must be byte-identical, not merely present"
    );
}

/// Issue #4123: the destructive behaviour must remain available and reachable
/// over HTTP via `?delete_data=true`.
///
/// Why: making the default safe is only half the fix — callers that genuinely
/// reclaim disk (the `delete_index` MCP tool, `trusty-search cleanup`,
/// trusty-mpm's worktree index GC) must still be able to opt in, or the fix
/// trades a data-loss bug for a disk-leak bug. Driving the real router proves
/// the flag survives routing and query extraction, not just the function
/// signature.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn delete_index_with_delete_data_true_destroys_data() {
    // RAII: see `delete_index_without_param_preserves_data`.
    let isolated = super::tests_components::IsolatedDataDir::new();
    let (router, index_data_dir) = router_with_index_and_data(isolated.path());

    let body = send_delete_request(router, &format!("/indexes/{INDEX_ID}?delete_data=true")).await;

    assert_eq!(body["removed"], true, "registration removed: {body}");
    assert_eq!(
        body["data_deleted"], true,
        "an opted-in DELETE must report that it destroyed data: {body}"
    );
    assert!(
        !index_data_dir.exists(),
        "?delete_data=true must destroy the data dir; {} survived",
        index_data_dir.display()
    );
}
