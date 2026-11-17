//! Tests for `GET`/`PATCH /indexes/:id/config` (issue #1372).
//!
//! Why: pin the hygiene-config read/update contract — the GET shape the UI
//! consumes, the partial-PATCH merge semantics, input validation, and that the
//! update persists to `indexes.toml`.
//! What: tests driving `index_config_report` / `patch_index_config_report`
//! against an in-process registry.
//! Test: this module.

use super::index_config::{
    index_config_report, patch_index_config_report, IndexConfigView, PatchIndexConfigRequest,
};
use super::state::SearchAppState;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use axum::http::StatusCode;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Register a bare handle (default hygiene fields) under `id` and return the
/// shared state.
fn state_with_index(id: &str) -> Arc<SearchAppState> {
    let registry = IndexRegistry::new();
    let tmp = std::env::temp_dir();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, &tmp))),
        tmp,
    ));
    Arc::new(SearchAppState::new(registry))
}

/// GET returns the index's current hygiene config with the targeted defaults
/// surfaced (the data-export skip dirs + the 64 KiB cap).
#[tokio::test]
#[serial_test::parallel]
async fn get_returns_current_config() {
    let state = state_with_index("cfg-get");
    let cfg: IndexConfigView = index_config_report(&state, "cfg-get").expect("IndexConfigView");
    assert_eq!(
        cfg.data_file_max_bytes, 65_536,
        "default 64 KiB cap surfaced"
    );
    assert!(
        cfg.extra_skip_dirs.contains(&"data".to_string())
            && cfg.extra_skip_dirs.contains(&"reports".to_string()),
        "default extra_skip_dirs surfaced: {:?}",
        cfg.extra_skip_dirs
    );
    assert!(cfg.include_docs, "include_docs default true");
    assert!(cfg.respect_gitignore, "respect_gitignore default true");
}

/// GET on an unknown index id returns 404.
#[tokio::test]
#[serial_test::parallel]
async fn get_unknown_index_404() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let err = index_config_report(&state, "ghost").expect_err("unknown index");
    assert_eq!(err.0, StatusCode::NOT_FOUND);
}

/// A GET → PATCH → GET round-trip: the PATCH'd values are reflected on the
/// next read, and unspecified fields are preserved.
#[tokio::test]
#[serial_test::serial]
async fn get_then_patch_then_get_round_trips() {
    // Isolate persistence so the PATCH does not touch the real indexes.toml.
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };

    let state = state_with_index("cfg-rt");

    // Initial GET.
    let before = index_config_report(&state, "cfg-rt").unwrap();
    assert_eq!(before.data_file_max_bytes, 65_536);

    // PATCH: tighten the cap and replace the skip-dir list. Leave the booleans
    // and extensions untouched.
    let patched = patch_index_config_report(
        &state,
        "cfg-rt",
        PatchIndexConfigRequest {
            extra_skip_dirs: Some(vec!["data".into(), "tmp".into()]),
            data_file_max_bytes: Some(32_768),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH is accepted");
    assert_eq!(
        patched["reindex_required"].as_bool(),
        Some(true),
        "PATCH must hint a reindex is required"
    );

    // GET again — the new values are reflected; include_docs preserved.
    let after = index_config_report(&state, "cfg-rt").unwrap();
    assert_eq!(after.data_file_max_bytes, 32_768, "cap updated");
    assert_eq!(
        after.extra_skip_dirs,
        vec!["data".to_string(), "tmp".to_string()]
    );
    assert_eq!(
        after.include_docs, before.include_docs,
        "unspecified field preserved"
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// A PATCH only mutates the supplied fields and sanitises list entries
/// (trims blanks, strips a leading dot from extensions).
#[tokio::test]
#[serial_test::serial]
async fn patch_updates_only_supplied_fields() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };

    let state = state_with_index("cfg-partial");
    patch_index_config_report(
        &state,
        "cfg-partial",
        PatchIndexConfigRequest {
            extensions: Some(vec![".rs".into(), "py".into(), "  ".into()]),
            exclude_globs: Some(vec!["**/gen/**".into(), "".into()]),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH is accepted");

    let handle = state
        .registry
        .get(&IndexId::new("cfg-partial"))
        .expect("handle");
    assert_eq!(
        handle.extensions,
        vec!["rs".to_string(), "py".to_string()],
        "leading dot stripped + blank dropped"
    );
    assert_eq!(handle.exclude_globs, vec!["**/gen/**".to_string()]);
    // data cap untouched (default).
    assert_eq!(handle.data_file_max_bytes, 65_536);

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// A PATCH with `data_file_max_bytes = 0` is rejected with 400 — a zero cap
/// would prune every data file.
#[tokio::test]
#[serial_test::parallel]
async fn patch_rejects_zero_cap() {
    let state = state_with_index("cfg-zero");
    let err = patch_index_config_report(
        &state,
        "cfg-zero",
        PatchIndexConfigRequest {
            data_file_max_bytes: Some(0),
            ..Default::default()
        },
    )
    .await
    .expect_err("a zero cap is refused");
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
}

/// A persistence failure surfaces as 500 (not a silent 200) so the UI never
/// reports "saved" while `indexes.toml` actually went stale (review #1372).
///
/// We force the failure by pointing `TRUSTY_DATA_DIR` at a path whose parent is
/// a regular file: `data_dir()`'s `create_dir_all` then fails, so
/// `upsert_index_registry_entry` returns `Err`. The body carries
/// `persisted: false` and an error string.
#[tokio::test]
#[serial_test::serial]
async fn patch_persist_failure_returns_500() {
    // Create a regular file, then point the data dir *inside* it — the OS
    // refuses to mkdir under a non-directory, so persistence fails.
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").unwrap();
    let bad_data_dir = blocker.join("data"); // parent is a file → mkdir fails
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", &bad_data_dir) };

    let state = state_with_index("cfg-persist-fail");
    let (status, body) = patch_index_config_report(
        &state,
        "cfg-persist-fail",
        PatchIndexConfigRequest {
            data_file_max_bytes: Some(16_384),
            ..Default::default()
        },
    )
    .await
    .expect_err("a persist failure is an error, not a silent success");
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "persist failure must surface as 500, not a silent 200"
    );
    assert_eq!(
        body["persisted"].as_bool(),
        Some(false),
        "body must report persisted:false: {body}"
    );
    assert!(
        body["error"].as_str().is_some(),
        "body must carry an error message: {body}"
    );

    // The in-memory change is still applied (the daemon honours it until
    // restart) — the response is truthful about the persistence gap only.
    let handle = state
        .registry
        .get(&IndexId::new("cfg-persist-fail"))
        .expect("handle");
    assert_eq!(handle.data_file_max_bytes, 16_384);

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// PATCH on an unknown index id returns 404.
#[tokio::test]
#[serial_test::parallel]
async fn patch_unknown_index_404() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let err = patch_index_config_report(&state, "ghost", PatchIndexConfigRequest::default())
        .await
        .expect_err("unknown index");
    assert_eq!(err.0, StatusCode::NOT_FOUND);
}

/// The PATCH persists the hygiene update to `indexes.toml` so it survives a
/// daemon restart.
#[tokio::test]
#[serial_test::serial]
async fn patch_persists_to_toml() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };

    let state = state_with_index("cfg-persist");
    patch_index_config_report(
        &state,
        "cfg-persist",
        PatchIndexConfigRequest {
            data_file_max_bytes: Some(16_384),
            extra_skip_dirs: Some(vec!["archive".into()]),
            ..Default::default()
        },
    )
    .await
    .expect("PATCH is accepted");

    // Reload the persisted registry (simulating a restart) and confirm the
    // hygiene fields landed.
    let entries = crate::service::persistence::load_index_registry().expect("load registry");
    let entry = entries
        .iter()
        .find(|e| e.id == "cfg-persist")
        .expect("entry persisted");
    assert_eq!(entry.data_file_max_bytes, Some(16_384));
    assert_eq!(entry.extra_skip_dirs, vec!["archive".to_string()]);

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}
