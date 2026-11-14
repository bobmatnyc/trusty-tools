//! Tests for `MemoryService::unload_palace` (#10989).
//!
//! Why: an unload must release the open palace and never remove its data,
//! and a refused unload must say so rather than report success.
//! What: one test per contract arm of `unload_palace`; the socket route is
//! covered by `rpc_palace_unload_releases_the_handle_over_the_socket` in
//! `transport::uds_tests`.
//! Test: this file IS the test module.

use std::time::Duration;

use serde_json::json;

use super::types::ServiceError;
use crate::AppState;
use crate::engine::palace::{Palace, PalaceId};
use crate::service::MemoryService;
use crate::tools::dispatch_tool;

fn fixture() -> (AppState, tempfile::TempDir) {
    crate::engine::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    (state, tmp)
}

fn make_palace(state: &AppState, id: &str) {
    state
        .registry
        .create_palace(
            &state.data_root,
            Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(id),
            },
        )
        .expect("create palace");
}

/// Why: acceptance 2 of #10989 — the unload releases the open palace and
/// leaves its directory and its drawers on disk; a reopen reads them back.
/// Test: this test. Fails if the palace directory or its data is deleted, or
/// if the handle stays resident.
#[tokio::test(flavor = "multi_thread")]
async fn unload_releases_the_handle_and_keeps_the_palace_on_disk() {
    let (state, tmp) = fixture();
    make_palace(&state, "decom");
    dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "decom", "text": "the unload keeps this drawer", "force": true}),
    )
    .await
    .expect("memory_remember");
    let id = PalaceId::new("decom");
    assert!(state.registry.peek(&id).is_some(), "premise: resident");
    let dir = tmp.path().join("decom");
    let files_before = std::fs::read_dir(&dir).expect("palace dir").count();

    let out = MemoryService::new(state.clone())
        .unload_palace("decom")
        .await
        .expect("unload");

    assert!(
        out.exists && out.was_resident && out.unloaded && out.data_kept,
        "{out:?}"
    );
    assert!(state.registry.peek(&id).is_none(), "the handle is released");
    assert!(
        dir.join("palace.json").exists(),
        "the palace directory is kept"
    );
    assert_eq!(
        std::fs::read_dir(&dir).expect("palace dir").count(),
        files_before,
        "no palace file was removed"
    );
    let reopened = state
        .registry
        .open_palace(&state.data_root, &id)
        .expect("the palace reopens from disk");
    let live = reopened.kg.load_drawer_ids().expect("drawer ids");
    assert_eq!(live.len(), 1, "the drawer survived the unload");
}

/// Why: #10989 — a project started again after its decommission uses its
/// palace with no explicit reload: the next tool call reopens it lazily.
/// Test: this test. Fails if an unloaded palace refuses a write, or if the
/// write lands anywhere but the palace's kept data.
#[tokio::test(flavor = "multi_thread")]
async fn an_unloaded_palace_reopens_on_its_next_tool_call() {
    let (state, _tmp) = fixture();
    make_palace(&state, "back");
    let remember = |text: &'static str| {
        dispatch_tool(
            &state,
            "memory_remember",
            json!({"palace": "back", "text": text, "force": true}),
        )
    };
    remember("written before the decommission")
        .await
        .expect("first write");
    MemoryService::new(state.clone())
        .unload_palace("back")
        .await
        .expect("unload");
    let id = PalaceId::new("back");
    assert!(state.registry.peek(&id).is_none(), "premise: unloaded");

    remember("written after the session started again")
        .await
        .expect("the unloaded palace accepts a write");

    let live = state
        .registry
        .open_palace(&state.data_root, &id)
        .expect("resident again")
        .kg
        .load_drawer_ids()
        .expect("drawer ids");
    assert_eq!(live.len(), 2, "both drawers are in the palace");
}

/// Why: a palace that is not on disk cannot be loaded, so tm's shutdown must
/// read "nothing loaded", not an error that blocks the decommission.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn unload_of_an_absent_palace_reports_nothing_loaded() {
    let (state, _tmp) = fixture();
    let out = MemoryService::new(state)
        .unload_palace("never-made")
        .await
        .expect("an absent palace is not an error");
    assert!(!out.exists && !out.was_resident && out.unloaded, "{out:?}");
}

/// Why: Fail-Open Check — an unload that cannot take the write lock must fail
/// by name and leave the handle resident, never report `unloaded`.
/// Test: this test. Fails if the lock-timeout arm is removed.
#[tokio::test(flavor = "multi_thread")]
async fn unload_while_a_write_holds_the_lock_is_busy_and_releases_nothing() {
    let (state, _tmp) = fixture();
    make_palace(&state, "busy");
    state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("busy"))
        .expect("open");
    let lock = state.palace_write_lock("busy");
    let _held = lock.lock().await;

    let err = MemoryService::new(state.clone())
        .unload_palace_within("busy", Duration::from_millis(50))
        .await
        .expect_err("a held write lock refuses the unload");

    assert!(
        matches!(&err, ServiceError::Conflict(m) if m.contains("busy")),
        "{err:?}"
    );
    assert!(
        state.registry.peek(&PalaceId::new("busy")).is_some(),
        "a refused unload releases nothing"
    );
}

/// Why: an id that cannot name a palace is a caller error, not "not loaded".
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn unload_refuses_an_invalid_id() {
    let (state, _tmp) = fixture();
    let err = MemoryService::new(state)
        .unload_palace("../escape")
        .await
        .expect_err("invalid id");
    assert!(matches!(err, ServiceError::BadRequest(_)), "{err:?}");
}
