//! Tests for palace delete, update, list-counts, and dream status.

use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

/// Why: Issue #180 — `?force=true` is the explicit destructive opt-in;
/// the conflict guard must yield and the palace must vanish even with
/// drawers present.
/// What: Same setup as the conflict test, but pass `?force=true` and
/// assert the 204 + 404 follow-up shape.
/// Test: This test itself.
#[tokio::test]
async fn delete_palace_force_removes_populated_palace() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "force-delete"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/force-delete/drawers")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"content": "Sacrificial drawer for the force-delete path.", "tags": []})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/force-delete?force=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/force-delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Why: Issue #180 — deleting a missing palace must yield 404 so
/// idempotent retries on the client are distinguishable from the
/// "drawers present" precondition failure.
/// What: DELETE against a never-created id and assert 404.
/// Test: This test itself.
#[tokio::test]
async fn delete_palace_returns_not_found_for_missing_id() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/never-existed")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Regression test for issue #5231.
///
/// Why: `DELETE /palaces/{id}/drawers/{drawer_id}` answered `204 No Content`
/// for a drawer id that was never stored — identical to a real delete — because
/// `PalaceHandle::forget` returned `Ok(())` either way. `delete_palace` has
/// answered 404 for a missing id since #180; the drawer route now matches.
/// What: creates a palace, DELETEs a well-formed but unknown drawer UUID, and
/// asserts 404. A malformed id stays a 400.
/// Test: This test itself.
#[tokio::test]
async fn delete_drawer_404s_for_an_unknown_drawer_id() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "ghost-drawer"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/ghost-drawer/drawers/deadbeef-0000-4000-8000-000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/ghost-drawer/drawers/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Why: Issue #180 follow-up — verify the happy path of `PATCH
/// /api/v1/palaces/{id}`: create a palace, rename it, and confirm
/// `GET /api/v1/palaces/{id}` returns the new display name. The id
/// (which is the on-disk directory) must stay stable.
/// What: POST a palace named "rename-me", PATCH with a new display
/// name, expect 200 + payload showing the rename, then GET to confirm
/// persistence to disk.
/// Test: This test itself.
#[tokio::test]
async fn update_palace_name_renames_palace() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "rename-me"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/palaces/rename-me")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "New Display Name"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["id"].as_str(), Some("rename-me"));
    assert_eq!(v["name"].as_str(), Some("New Display Name"));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/rename-me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["id"].as_str(), Some("rename-me"));
    assert_eq!(v["name"].as_str(), Some("New Display Name"));
}

/// Why: Issue #180 follow-up — empty / whitespace-only names would
/// break the dashboard label. Reject with 400 so the caller knows the
/// request was well-formed but the value is invalid.
/// What: Create a palace, PATCH with `{"name": "   "}`, expect 400.
/// Test: This test itself.
#[tokio::test]
async fn update_palace_name_rejects_empty_name() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "keep-name"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/palaces/keep-name")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "   "}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Why: Issue #180 follow-up — patching a non-existent palace must
/// yield 404 so retries against the wrong id surface the real problem
/// rather than silently no-op'ing.
/// What: PATCH against a never-created id and assert 404.
/// Test: This test itself.
#[tokio::test]
async fn update_palace_name_returns_not_found_for_missing_id() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/palaces/no-such-palace")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "irrelevant"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Restore a directory's mode on drop, including while unwinding.
///
/// Why: the denial test below strips a palace directory to mode 000. The
/// fixture's tempdir is deliberately leaked, so a mode-000 directory left
/// behind by a failed assertion is a permanent undeletable leak rather than a
/// transient one.
#[cfg(unix)]
struct RestoreMode(std::path::PathBuf);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

/// Why (#5549): hardening `load_palace` to distinguish absent from
/// undeterminable buys nothing if the caller flattens the distinction again.
/// `update_palace_name_typed` mapped EVERY `PalaceStoreError` through
/// `ServiceError::not_found`, so a palace whose `palace.json` could not be
/// stat'd was reported to the HTTP client as 404 — "this palace does not
/// exist" — for a denial or a transient `EIO` that established no such thing.
/// What: creates a palace, strips its directory to mode 000 so stat of the
/// metadata inside is denied, and PATCHes it. Asserts the response is 500 and
/// specifically NOT 404. Panics rather than passing vacuously if the denial
/// does not take hold.
/// Test: This test itself.
#[cfg(unix)]
#[tokio::test]
async fn update_palace_name_reports_an_unstattable_palace_as_internal() {
    use std::os::unix::fs::PermissionsExt;

    let state = test_state();
    let palace_dir = state.data_root.join("locked-palace");
    let app = router().with_state(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "locked-palace"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    std::fs::set_permissions(&palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(palace_dir.clone());

    // Root bypasses the mode bits outright, and some filesystems ignore them,
    // so confirm the denial actually took hold. A vacuous pass on a fail-open
    // guard is worse than no test at all.
    let target = palace_dir.join("palace.json");
    match std::fs::metadata(&target) {
        Ok(_) => panic!(
            "cannot exercise #5549: stat of {} still succeeds with its parent at mode 000. \
             Run this suite as a non-root user on a filesystem that honours POSIX \
             permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked palace dir to deny stat of its metadata, got {e}"
        ),
    }

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/palaces/locked-palace")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "New Display Name"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a palace whose metadata cannot be stat'd was reported as absent — that is the \
         #5549 coercion re-created one crate up, at the caller"
    );
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Why: The operator TUI's MEMORY tab reads `node_count`, `edge_count`,
/// `community_count`, and `is_compacting` straight off the
/// `/api/v1/palaces` payload. If any of those fields disappear or change
/// type the spinner / counters break silently. Pin the shape here.
/// What: Creates a palace, lists `/api/v1/palaces`, and asserts every new
/// field is present and typed as expected (numbers default to 0, the
/// compacting flag defaults to false on a freshly-opened palace).
/// Test: This test itself.
#[tokio::test]
async fn palace_list_includes_graph_counts() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let body = json!({"name": "graph-counts", "description": null}).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("array");
    let row = arr
        .iter()
        .find(|p| p["id"] == "graph-counts")
        .expect("created palace must appear in list");
    assert_eq!(row["node_count"].as_u64(), Some(0));
    assert_eq!(row["edge_count"].as_u64(), Some(0));
    assert_eq!(row["community_count"].as_u64(), Some(0));
    assert_eq!(row["is_compacting"].as_bool(), Some(false));
}

/// Why: The enriched status payload backs the dashboard's top-row stats;
/// it must always include the new total_* counters, even on an empty data
/// root, so the UI can render zeros without special-casing missing fields.
/// What: Hit `/api/v1/status` on a fresh state and assert the new fields
/// are present and set to 0.
/// Test: This test itself.
#[tokio::test]
async fn status_includes_total_counters() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["total_drawers"], 0);
    assert_eq!(v["total_vectors"], 0);
    assert_eq!(v["total_kg_triples"], 0);
}

/// Why: `/api/v1/dream/status` must return a well-shaped payload even
/// when no palace has ever run a dream cycle (so the dashboard's first
/// load doesn't error).
/// What: Hit the endpoint on a fresh state and assert `last_run_at` is
/// null and the counters are zero.
/// Test: This test itself.
#[tokio::test]
async fn dream_status_empty_returns_nulls() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dream/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["last_run_at"].is_null());
    assert_eq!(v["merged"], 0);
    assert_eq!(v["pruned"], 0);
}
