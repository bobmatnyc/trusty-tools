//! Regression tests for the delete-time root-path re-check (#6380).
//!
//! Why: an index id is derived from its `root_path`, so a path deleted and
//! recreated between a census and the delete that acts on it produces the same
//! id for a different, live index. `DELETE /indexes/{id}` compared nothing, so
//! it removed whatever now held that id. These tests pin the four arms of
//! `delete_guard::refuse_unless_root_matches`: a moved root refuses, a matching
//! root proceeds, and the two arms that could not make the comparison at all —
//! an absent registration and an unreadable registry — refuse rather than
//! proceed. One more pins the comparison's ATOMICITY: a relocate that lands
//! while the delete waits to quiesce must refuse it, because the pre-lock
//! comparison and the teardown it authorises are not one step.
//!
//! What: most tests drive the real router with `oneshot`, so the
//! `?expected_root_path=` extractor and the status code are exercised alongside
//! the guard. The interleaving test calls `delete_index_report` directly instead,
//! because it needs one `SearchAppState` shared between the delete and the
//! relocate. Every test points `TRUSTY_DATA_DIR` at a `TempDir` and is
//! `#[serial_test::serial]`, because that env var is process-wide.
//!
//! Test: this module. Run with `cargo test -p trusty-search tests_6380`.

use super::build_router;
use super::search::{delete_index_report, DeleteIndexParams};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence;
use crate::service::reindex::{index_cancel_flag, index_teardown_lock};
use crate::service::server::SearchAppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tower::ServiceExt;

/// Id used throughout. `[a-z0-9-]` only, so `persistence::sanitize_id` is the
/// identity function and the data-dir name is predictable.
const INDEX_ID: &str = "recreated-6380";

/// Seed `id` in `indexes.toml` pointing at `root`, plus a data dir with a
/// marker file, through the daemon's own persistence path.
fn register(id: &str, root: &Path, data_dir: &Path) -> PathBuf {
    persistence::upsert_index_registry_entry(persistence::PersistedIndex::new(id, root))
        .expect("seed indexes.toml");
    let index_data_dir = data_dir.join("indexes").join(id);
    std::fs::create_dir_all(&index_data_dir).expect("create index data dir");
    std::fs::write(index_data_dir.join("corpus.marker"), b"real corpus bytes")
        .expect("write marker");
    index_data_dir
}

/// True iff `id` still has a row in the persisted registry.
fn row_exists(id: &str) -> bool {
    persistence::load_index_registry()
        .expect("load registry")
        .iter()
        .any(|e| e.id == id)
}

/// Issue the DELETE and return `(status, body)`.
async fn send_delete(uri: &str) -> (StatusCode, serde_json::Value) {
    let router = build_router(SearchAppState::new(IndexRegistry::new()));
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
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

/// The delete URI carrying an expectation, percent-encoded.
fn delete_uri(id: &str, expected_root: &Path, delete_data: bool) -> String {
    let encoded: String = expected_root
        .display()
        .to_string()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    format!("/indexes/{id}?delete_data={delete_data}&expected_root_path={encoded}")
}

/// #6380: the reported hazard. A registration whose root moved between the
/// census and the delete must NOT be removed.
///
/// Why: this is the whole issue. The census listed `<tmp>/gone` as a stale
/// root; by the time the delete arrived the id had been re-registered against
/// `<tmp>/live`. Against `origin/main` the `expected_root_path` param is
/// ignored entirely, so the delete removes the row and destroys the marker
/// file: both assertions below fail, as does the 409.
/// What: seeds the registration at `live`, deletes it naming `gone`, then
/// re-reads the registry and the data dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_delete_whose_expected_root_moved_is_refused() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let live = isolated.path().join("live");
    let gone = isolated.path().join("gone");
    let data_dir = register(INDEX_ID, &live, isolated.path());

    let (status, body) = send_delete(&delete_uri(INDEX_ID, &gone, true)).await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "#6380: a registration that moved must refuse the delete. Body: {body}"
    );
    assert_eq!(body["ok"], false, "body: {body}");
    assert_eq!(
        body["removed"], false,
        "a refusal must state that nothing was removed. Body: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains(&live.display().to_string())),
        "the refusal must name the root the registration actually has. Body: {body}"
    );
    assert!(
        row_exists(INDEX_ID),
        "#6380: the row must survive — deleting it is the bug"
    );
    assert!(
        data_dir.join("corpus.marker").exists(),
        "#6380: `delete_data=true` must not reach the data dir behind a refused \
         delete: {}",
        data_dir.display()
    );
}

/// #6380: the guard must not deny the feature. An expectation that still holds
/// deletes exactly as an unguarded delete does.
///
/// Why: a re-check that refuses everything is not a guard, and the prune route
/// now sends an expectation on every id.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_delete_whose_expected_root_matches_proceeds() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("still-here");
    let data_dir = register(INDEX_ID, &root, isolated.path());

    let (status, body) = send_delete(&delete_uri(INDEX_ID, &root, true)).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["removed"], true, "body: {body}");
    assert_eq!(body["data_deleted"], true, "body: {body}");
    assert!(!row_exists(INDEX_ID), "the row must be gone");
    assert!(!data_dir.exists(), "the data dir must be gone");
}

/// #6380 fail-closed: an expectation the daemon has no registration to compare
/// against is refused, and says so.
///
/// Why: "there is nothing here" and "it still matches" are different facts. The
/// pre-fix handler answered a bare `unknown index: <id>`, which a caller reading
/// only the status could not distinguish from a comparison that ran and passed.
/// Against `origin/main` the message assertion fails: the param is ignored, so
/// the 404 carries no mention of the expectation at all.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_delete_expectation_against_an_absent_registration_is_refused() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("never-registered");

    let (status, body) = send_delete(&delete_uri("never-existed-6380", &root, false)).await;

    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("expected root path")),
        "#6380: the refusal must say the comparison could not be made, not just \
         that the id is unknown. Body: {body}"
    );
}

/// #6380 fail-closed: a registry that cannot be READ must refuse an expectation
/// rather than delete without checking it.
///
/// Why: this is the arm that would be easiest to downgrade to proceed-anyway —
/// the id is in no store, so the "nothing to compare" branch is one `Ok(())`
/// away from letting the delete run unchecked. An unreadable registry is not
/// proof the root still matches. Against `origin/main` the message assertion
/// fails: the guard does not exist, so the 500 that arm produces names only the
/// rewrite, never the expected root path.
/// What: makes the data dir unreadable so `find_index_registry_entry` errors,
/// then asserts the refusal names the check it could not make. Permissions are
/// restored before the tempdir is dropped.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn an_unreadable_registry_refuses_an_expected_root_delete() {
    use std::os::unix::fs::PermissionsExt;

    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("unknowable");
    register(INDEX_ID, &root, isolated.path());

    // RAII: an assertion below panics on failure, and an unwind must not leave
    // an unreadable directory for `TempDir::drop` to fail on. Declared AFTER
    // `isolated` so it drops FIRST.
    struct RestoreMode(PathBuf);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let registry_dir = persistence::indexes_toml_path()
        .expect("registry path")
        .parent()
        .expect("registry parent")
        .to_path_buf();
    let _restore = RestoreMode(registry_dir.clone());
    std::fs::set_permissions(&registry_dir, std::fs::Permissions::from_mode(0o000))
        .expect("make the registry directory unreadable");

    let (status, body) = send_delete(&delete_uri(INDEX_ID, &root, true)).await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "#6380: a comparison that could not run must not answer 2xx. Body: {body}"
    );
    assert_eq!(body["ok"], false, "body: {body}");
    assert_eq!(body["removed"], false, "body: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("expected root path")),
        "#6380: the refusal must name the check it could not make. Body: {body}"
    );
    drop(_restore);
    assert!(
        row_exists(INDEX_ID),
        "#6380: nothing may be removed behind a comparison that never ran"
    );
}

/// The id the interleaving test below uses. Distinct from [`INDEX_ID`] on
/// purpose: the teardown lock and cancel flag are process-global maps keyed by
/// id, and that test's coordination reads the cancel flag as a happens-after
/// marker. A value another test in this binary already set would let the poll
/// return before this delete had even started.
const RACE_INDEX_ID: &str = "midquiesce-6380";

/// A registered handle at `root`, matching `tests_3049::state_with_index`.
fn bare_handle(id: &str, root: &Path) -> IndexHandle {
    IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, root.to_path_buf()))),
        root.to_path_buf(),
    )
}

/// Poll `id`'s cancel flag until the delete has signalled it, or give up.
///
/// Why: `unregister_index` signals the cancel AFTER `delete_index_report`'s
/// pre-lock comparison and BEFORE it parks on the exclusive teardown wait, so
/// the flag reading `true` is the one observable that proves the pre-lock
/// comparison already ran and passed. Swapping the root before that point would
/// be refused by the fast path instead, which proves nothing about the under-lock
/// check. The deadline is well inside the 1.5s `cfg(test)` quiesce timeout, so a
/// delete that is going to abandon itself is not what this waits for.
async fn await_cancel_signal(id: &IndexId) {
    let flag = index_cancel_flag(id);
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        if flag.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("the delete never signalled its cancel flag — it never reached the quiesce wait");
}

/// #6380: a relocate landing while the delete waits to quiesce must turn the
/// delete into a REFUSAL, not a success against the moved root.
///
/// Why: the pre-lock comparison and the teardown it authorises are not one
/// atomic step. `relocate_index_report` rewrites `root_path` while holding only
/// the SHARED side of the same teardown lock the delete waits on exclusively, so
/// a relocate fits entirely inside the window between the two. Against the branch
/// head this fails as a real assertion failure: the comparison is never re-run,
/// so the delete answers `200 removed:true` and destroys the data directory of a
/// live index at a root the caller never named.
/// What: holds the shared guard the way a relocate does, starts the delete with
/// an expectation naming the OLD root, waits on the cancel flag until the delete
/// is provably past its pre-lock check, then lands the relocate's two observable
/// effects — the `indexes.toml` rewrite and the hot-handle swap — and releases
/// the guard. The delete then acquires the exclusive side and must refuse.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[serial_test::serial]
async fn a_relocate_landing_mid_quiesce_refuses_the_delete() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let old_root = isolated.path().join("old-root");
    let new_root = isolated.path().join("new-root");
    std::fs::create_dir_all(&old_root).expect("create old root");
    std::fs::create_dir_all(&new_root).expect("create new root");
    let data_dir = register(RACE_INDEX_ID, &old_root, isolated.path());

    let registry = IndexRegistry::new();
    registry.register(bare_handle(RACE_INDEX_ID, &old_root));
    let state = Arc::new(SearchAppState::new(registry));
    let index_id = IndexId::new(RACE_INDEX_ID);

    // The relocate's own guard: `relocate_index_report` takes the SHARED side and
    // holds it across the rebuild, the registry rewrite and the handle swap.
    let relocate_guard = index_teardown_lock(&index_id).read_owned().await;

    let delete_state = Arc::clone(&state);
    let expected = old_root.display().to_string();
    let delete = tokio::spawn(async move {
        delete_index_report(
            &delete_state,
            RACE_INDEX_ID,
            DeleteIndexParams {
                delete_data: true,
                expected_root_path: Some(expected),
            },
        )
        .await
    });

    await_cancel_signal(&index_id).await;

    // The relocate lands, in the order `relocate_index_report` does it: registry
    // file first, hot handle second, guard released last.
    persistence::upsert_index_registry_entry(persistence::PersistedIndex::new(
        RACE_INDEX_ID,
        &new_root,
    ))
    .expect("relocate rewrites indexes.toml");
    state
        .registry
        .register(bare_handle(RACE_INDEX_ID, &new_root));
    drop(relocate_guard);

    let (status, body) = delete
        .await
        .expect("delete task")
        .expect_err("#6380: a delete whose root moved mid-quiesce must not succeed");

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "#6380: the under-lock re-check must refuse a root that moved while the \
         delete waited. Body: {body}"
    );
    assert_eq!(body["removed"], false, "body: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains(&new_root.display().to_string())),
        "the refusal must name the root the registration has NOW. Body: {body}"
    );
    assert!(
        row_exists(RACE_INDEX_ID),
        "#6380: the relocated row must survive a refused delete"
    );
    assert!(
        state.registry.get(&index_id).is_some(),
        "#6380: the live handle must survive a refused delete"
    );
    assert!(
        data_dir.join("corpus.marker").exists(),
        "#6380: `delete_data=true` must not reach the data dir behind a refusal: {}",
        data_dir.display()
    );
    assert!(
        !index_cancel_flag(&index_id).load(Ordering::Acquire),
        "#6380: a refused delete must clear the cancel it signalled, or the \
         surviving index aborts its next reindex"
    );
}

/// #6380: a delete that sends NO expectation is unchanged.
///
/// Why: every pre-#6380 caller sends none, and the guard must be opt-in rather
/// than a new precondition on the whole delete surface.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_delete_without_an_expectation_is_unguarded() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = isolated.path().join("whatever");
    register(INDEX_ID, &root, isolated.path());

    let (status, body) = send_delete(&format!("/indexes/{INDEX_ID}")).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["removed"], true, "body: {body}");
    assert!(!row_exists(INDEX_ID));
}
