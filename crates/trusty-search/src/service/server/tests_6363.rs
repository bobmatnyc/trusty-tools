//! Regression tests for `DELETE /indexes/{id}` on a registration that exists
//! only in `indexes.toml` (#6363).
//!
//! Why: the #767 allowlist gate drops an unapproved root at warm boot
//! (`retain_approved_entries`) before it reaches the hot registry or the cold
//! store, and `unregister_index` decided "does this index exist?" from those two
//! stores alone. The row was therefore neither deletable nor unknown: the
//! handler answered `200 {"removed": false, "data_deleted": false}` and left the
//! `indexes.toml` row and the on-disk data dir exactly where they were. A live
//! 0.49.4 daemon accumulated 60 of them, each one keeping `warm_boot_degraded`
//! true on every boot, clearable only by stopping the daemon and hand-editing
//! the file. The console delete (#6360 / PR #6362) reads `removed: false` as a
//! failure and calls this same handler, so it could not clear them either.
//! What: drives the real router with `oneshot` so the route wiring, the
//! `?delete_data=` extractor and the status code are all exercised — the status
//! code is half of what #6363 changes. Every test points `TRUSTY_DATA_DIR` at a
//! `TempDir` (`IsolatedDataDir`) and is `#[serial_test::serial]`, because that
//! env var is process-wide.
//! Test: this module. Run with `cargo test -p trusty-search tests_6363`.

use super::build_router;
use crate::core::registry::IndexRegistry;
use crate::service::persistence;
use crate::service::server::SearchAppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

/// Id used throughout. `[a-z0-9-]` only, so `persistence::sanitize_id` is the
/// identity function and the data-dir name is predictable.
const INDEX_ID: &str = "excluded-6363";

/// A real directory outside `$TMPDIR` that the hard denylist accepts.
///
/// Why: a `tempfile::tempdir()` under `/var/folders` is denied by PREFIX, so an
/// exclusion there would prove nothing about the allowlist decision this fixture
/// is meant to reproduce — the same reason `tests_allowlist_gate_767::safe_root`
/// builds its roots under `$HOME`.
fn unapproved_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ts-6363-unapproved")
        .tempdir_in(dirs::home_dir().expect("HOME required"))
        .expect("tempdir")
}

/// Assert the allowlist genuinely REJECTS `root`, the way warm boot does.
///
/// Why: this is the fixture's precondition, not decoration. Without it the test
/// would only prove that a delete works when both stores happen to be empty,
/// which is a weaker claim than "the state the #767 gate leaves behind is
/// deletable". `retain_approved_entries` lives in the binary crate, so the
/// library test asserts on the same decision function it calls.
fn assert_allowlist_rejects(root: &Path, paths: &crate::allowlist::AllowlistPaths) {
    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    let verdict =
        crate::allowlist::sources::resolve_allow_source(&canonical, paths).expect("read allowlist");
    assert!(
        verdict.is_none(),
        "fixture precondition: the allowlist must REJECT {} — otherwise warm boot \
         would have registered it and this test exercises the wrong path",
        canonical.display()
    );
}

/// An empty but readable allowlist: nothing is approved, so every root is
/// excluded by the union rather than by the denylist.
fn empty_allowlist(dir: &Path) -> crate::allowlist::AllowlistPaths {
    let paths = crate::allowlist::AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"));
    crate::allowlist::AllowlistConfig::default()
        .save_to(&paths.allowlist_file())
        .expect("write empty allowlist");
    paths
}

/// Register `id` in `indexes.toml` through the daemon's own persistence path
/// and materialise its data dir with a marker file.
///
/// Deliberately NOT a hand-written TOML file: the assertions below re-read the
/// registry through the same loader, so a fixture that bypassed the writer
/// could pass against a fix that never wrote anything.
fn register_only_in_toml(id: &str, root: &Path, data_dir: &Path) -> PathBuf {
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

/// A router over EMPTY stores — exactly what warm boot leaves behind once
/// `retain_approved_entries` has dropped the entry.
fn router_over_empty_stores() -> axum::Router {
    build_router(SearchAppState::new(IndexRegistry::new()))
}

/// Issue the DELETE and return `(status, body)`.
async fn send_delete(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
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

/// #6363: an id the allowlist excluded at warm boot must be deletable — row
/// dropped from `indexes.toml`, data dir removed when asked, `removed: true`.
///
/// Why: this is the reported bug. Against `origin/main` the handler finds the id
/// in neither store, skips the whole durable-cleanup branch, and answers
/// `200 {"removed": false}` — so `body["removed"]` is `false`, the row is still
/// in the file and the marker file is still on disk. All three assertions fail.
/// What: seeds one registration whose root the allowlist rejects, drives
/// `DELETE /indexes/{id}?delete_data=true` over the real router, then re-reads
/// the persisted registry and the data dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn delete_of_an_allowlist_excluded_registration_removes_the_row() {
    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = unapproved_root();
    assert_allowlist_rejects(root.path(), &empty_allowlist(isolated.path()));
    let index_data_dir = register_only_in_toml(INDEX_ID, root.path(), isolated.path());
    assert!(row_exists(INDEX_ID), "fixture must seed the registry row");

    let (status, body) = send_delete(
        router_over_empty_stores(),
        &format!("/indexes/{INDEX_ID}?delete_data=true"),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["removed"], true,
        "#6363: a registration-only id IS a registration — the delete must report \
         it removed, not answer 200 with removed:false. Body: {body}"
    );
    assert_eq!(
        body["data_deleted"], true,
        "#6363: `?delete_data=true` reached the removal branch, so the response \
         must say the corpus was reclaimed. Body: {body}"
    );
    assert_eq!(body["ok"], true, "no durable step failed. Body: {body}");
    assert!(
        !row_exists(INDEX_ID),
        "#6363: the indexes.toml row must be gone — a surviving row is what keeps \
         warm_boot_degraded true on every boot"
    );
    assert!(
        !index_data_dir.exists(),
        "#6363: `?delete_data=true` must remove {}",
        index_data_dir.display()
    );
}

/// #6363: an id in NO store and NO registry row is a 404, not a 200 that
/// reports it did nothing.
///
/// Why: `200 {"removed": false}` is the same answer the pre-fix handler gave a
/// real-but-excluded registration, so a caller could not tell a typo from an
/// undeletable index — and that ambiguity is precisely what hid this bug on a
/// live daemon for 60 rows. Against `origin/main` this test fails on the status
/// code: the handler returns `200` for every id it does not find.
/// What: deletes an id nothing has ever registered.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn delete_of_an_id_in_no_store_and_no_registry_is_404() {
    let _isolated = super::tests_components::IsolatedDataDir::new();

    let (status, body) =
        send_delete(router_over_empty_stores(), "/indexes/never-existed-6363").await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "#6363: an id absent from every store must answer 404, not 200 with \
         removed:false. Body: {body}"
    );
    assert_eq!(
        body["error"], "unknown index: never-existed-6363",
        "the 404 must name the absent-everywhere verdict. Body: {body}"
    );
}

/// #6363 fail-closed: a delete whose `indexes.toml` rewrite FAILS must report
/// the failure rather than answer 200.
///
/// Why: the rewrite failure was a `warn!` with no representation on the wire, so
/// a delete that changed nothing durable answered exactly like one that
/// succeeded. That is the failure class this issue is an instance of — state
/// that did not advance, reported as if it had. With the registry-only path now
/// removing rows, swallowing this error would hand an operator a "removed" row
/// that comes back on the next boot.
/// What: makes the data dir read-only so the registry write cannot publish,
/// then asserts the response is a 500 carrying `ok: false` and an `error`, that
/// `removed` is false (a registry-only delete that failed removed NOTHING), and
/// that the row survives. Permissions are restored before the tempdir is
/// dropped.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_failed_indexes_toml_rewrite_is_reported_not_swallowed() {
    use std::os::unix::fs::PermissionsExt;

    let isolated = super::tests_components::IsolatedDataDir::new();
    let root = unapproved_root();
    assert_allowlist_rejects(root.path(), &empty_allowlist(isolated.path()));
    register_only_in_toml(INDEX_ID, root.path(), isolated.path());

    // RAII: an assertion below panics on failure, and an unwind must not leave a
    // read-only directory for `TempDir::drop` to fail on.
    struct ReadOnlyDir(PathBuf);
    impl Drop for ReadOnlyDir {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    // Declared AFTER `isolated`, so it drops FIRST and the tempdir is writable
    // again by the time `TempDir::drop` removes it.
    let _restore = ReadOnlyDir(isolated.path().to_path_buf());
    std::fs::set_permissions(isolated.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make data dir read-only");

    let (status, body) =
        send_delete(router_over_empty_stores(), &format!("/indexes/{INDEX_ID}")).await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "#6363: a delete whose durable rewrite failed must not answer 2xx. \
         Body: {body}"
    );
    assert_eq!(body["ok"], false, "body: {body}");
    assert_eq!(
        body["removed"], false,
        "#6363: nothing was removed — the row is still in indexes.toml. Body: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("indexes.toml")),
        "the error must name what failed. Body: {body}"
    );
    drop(_restore);
    assert!(
        row_exists(INDEX_ID),
        "the row must survive a failed rewrite — otherwise the response and the \
         file disagree in the other direction"
    );
}
