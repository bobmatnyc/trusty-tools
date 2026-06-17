//! Tests for `GET`/`PATCH /indexes/:id/config` (issue #1372).
//!
//! Why: pin the hygiene-config read/update contract — the GET shape the UI
//! consumes, the partial-PATCH merge semantics, input validation, and that the
//! update persists to `indexes.toml`.
//! What: handler-level tests driving `index_config_handler` /
//! `patch_index_config_handler` against an in-process registry.
//! Test: this module.

use super::index_config::{
    index_config_handler, patch_index_config_handler, IndexConfigView, PatchIndexConfigRequest,
};
use super::state::SearchAppState;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use axum::body::to_bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

/// GET returns the index's current hygiene config with the targeted defaults
/// surfaced (the data-export skip dirs + the 64 KiB cap).
#[tokio::test]
async fn get_returns_current_config() {
    let state = state_with_index("cfg-get");
    let resp = index_config_handler(State(state), Path("cfg-get".to_string())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let cfg: IndexConfigView = serde_json::from_value(v).expect("IndexConfigView");
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
async fn get_unknown_index_404() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let resp = index_config_handler(State(state), Path("ghost".to_string())).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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
    let resp = index_config_handler(State(Arc::clone(&state)), Path("cfg-rt".to_string())).await;
    let before: IndexConfigView = serde_json::from_value(body_json(resp).await).unwrap();
    assert_eq!(before.data_file_max_bytes, 65_536);

    // PATCH: tighten the cap and replace the skip-dir list. Leave the booleans
    // and extensions untouched.
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("cfg-rt".to_string()),
        Json(PatchIndexConfigRequest {
            extra_skip_dirs: Some(vec!["data".into(), "tmp".into()]),
            data_file_max_bytes: Some(32_768),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let patched = body_json(resp).await;
    assert_eq!(
        patched["reindex_required"].as_bool(),
        Some(true),
        "PATCH must hint a reindex is required"
    );

    // GET again — the new values are reflected; include_docs preserved.
    let resp = index_config_handler(State(Arc::clone(&state)), Path("cfg-rt".to_string())).await;
    let after: IndexConfigView = serde_json::from_value(body_json(resp).await).unwrap();
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
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("cfg-partial".to_string()),
        Json(PatchIndexConfigRequest {
            extensions: Some(vec![".rs".into(), "py".into(), "  ".into()]),
            exclude_globs: Some(vec!["**/gen/**".into(), "".into()]),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

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
async fn patch_rejects_zero_cap() {
    let state = state_with_index("cfg-zero");
    let resp = patch_index_config_handler(
        State(state),
        Path("cfg-zero".to_string()),
        Json(PatchIndexConfigRequest {
            data_file_max_bytes: Some(0),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("cfg-persist-fail".to_string()),
        Json(PatchIndexConfigRequest {
            data_file_max_bytes: Some(16_384),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "persist failure must surface as 500, not a silent 200"
    );
    let body = body_json(resp).await;
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
async fn patch_unknown_index_404() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let resp = patch_index_config_handler(
        State(state),
        Path("ghost".to_string()),
        Json(PatchIndexConfigRequest::default()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// The PATCH persists the hygiene update to `indexes.toml` so it survives a
/// daemon restart.
#[tokio::test]
#[serial_test::serial]
async fn patch_persists_to_toml() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };

    let state = state_with_index("cfg-persist");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("cfg-persist".to_string()),
        Json(PatchIndexConfigRequest {
            data_file_max_bytes: Some(16_384),
            extra_skip_dirs: Some(vec!["archive".into()]),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

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
