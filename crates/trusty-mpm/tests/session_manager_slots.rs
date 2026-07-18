//! Integration coverage for the `GET /api/v1/sessions/managed` stable slot
//! numbering (issue #3034).
//!
//! Why: split out of `session_manager_mvp.rs` (already at its 1500-SLOC test
//! cap) so neither file grows past its limit, mirroring the split precedent
//! used elsewhere in this crate's `tests/` directory (`session_control_api.rs`,
//! `test_session_lifecycle.rs`, `tm_sessions_alias_notice.rs` are all
//! independent files rather than one giant `session_manager_mvp.rs`). This is
//! the HTTP-layer proof of Bob's report: a session deleted mid-fleet must not
//! shift every later session's `slot`, and its own slot must render as a
//! `deleted: true` placeholder rather than vanishing or being reused.
//! What: one end-to-end test driving the real
//! [`trusty_mpm::daemon::managed_routes::list_managed_sessions`] handler
//! against an isolated in-memory `DaemonState` (fake tmux driver — no real
//! tmux/git required).
//! Test: this file IS the test; run with `cargo test -p trusty-mpm`.

use std::collections::HashMap;
use std::sync::Arc;

use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::list_managed_sessions;
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId;

/// Decode an axum `impl IntoResponse` into `(StatusCode, serde_json::Value)`.
///
/// Why: mirrors `session_manager_mvp.rs`'s helper of the same name — kept
/// local here rather than shared, since these test files intentionally stay
/// independent (no cross-`tests/*.rs` module).
async fn decode_response(
    resp: impl axum::response::IntoResponse,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = resp.into_response();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// `GET /api/v1/sessions/managed` assigns stable slot numbers and tombstones
/// a deleted one instead of letting its neighbor inherit the number (#3034).
///
/// Why: this is the exact HTTP-layer proof of Bob's report — a session
/// deleted mid-fleet must not shift every later session's `slot`, and its own
/// slot must render as a `deleted: true` placeholder rather than vanishing.
/// What: seeds two sessions, lists once and records both slots (asserting
/// they are distinct 1-based numbers), deletes the first via `delete_record`,
/// lists again, and asserts: the survivor keeps its EXACT original slot; a
/// tombstone row now exists at the deleted session's original slot with
/// `deleted: true` and a blank `id`.
/// Test: this function IS the test.
#[tokio::test]
async fn list_assigns_stable_slot_numbers_and_tombstones_deleted_one() {
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id_a = ManagedSessionId::new();
    let ws_a = root.path().join(format!("{id_a}-slot-a"));
    mgr.create_with_id(
        id_a,
        "slot-test-a".to_string(),
        Some(ws_a.clone()),
        None,
        Some(ws_a),
        Some("https://github.com/owner/repo".to_string()),
        Some("main".to_string()),
        RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session A");

    let id_b = ManagedSessionId::new();
    let ws_b = root.path().join(format!("{id_b}-slot-b"));
    mgr.create_with_id(
        id_b,
        "slot-test-b".to_string(),
        Some(ws_b.clone()),
        None,
        Some(ws_b),
        Some("https://github.com/owner/repo".to_string()),
        Some("main".to_string()),
        RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session B");

    let find_by_id = |body: &serde_json::Value, id: &str| -> Option<serde_json::Value> {
        body["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["id"].as_str() == Some(id))
            .cloned()
    };
    let find_tombstone_at = |body: &serde_json::Value, slot: u64| -> Option<serde_json::Value> {
        body["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["slot"].as_u64() == Some(slot) && s["deleted"].as_bool() == Some(true))
            .cloned()
    };

    let (status, body) = decode_response(
        list_managed_sessions(
            axum::extract::State(state.clone()),
            axum::extract::Query(HashMap::new()),
        )
        .await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let slot_a = find_by_id(&body, &id_a.to_string()).expect("session A present")["slot"]
        .as_u64()
        .expect("slot is a number");
    let slot_b = find_by_id(&body, &id_b.to_string()).expect("session B present")["slot"]
        .as_u64()
        .expect("slot is a number");
    assert_ne!(slot_a, slot_b, "distinct sessions must get distinct slots");
    assert!(slot_a >= 1 && slot_b >= 1, "slots are 1-based");

    // Delete session A; session B must keep its EXACT slot, and A's old slot
    // must reappear as a tombstone rather than being handed to B or vanishing.
    mgr.delete_record(&id_a, true).await.expect("delete A");

    let (status, body) = decode_response(
        list_managed_sessions(
            axum::extract::State(state.clone()),
            axum::extract::Query(HashMap::new()),
        )
        .await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        find_by_id(&body, &id_a.to_string()).is_none(),
        "the deleted session's real record must no longer be listed"
    );
    let slot_b_after =
        find_by_id(&body, &id_b.to_string()).expect("session B still present")["slot"]
            .as_u64()
            .expect("slot is a number");
    assert_eq!(
        slot_b_after, slot_b,
        "B must keep its number after A is deleted"
    );
    let tombstone =
        find_tombstone_at(&body, slot_a).expect("A's old slot must render as a tombstone");
    assert_eq!(
        tombstone["id"].as_str(),
        Some(""),
        "tombstone id must be blank"
    );
}
