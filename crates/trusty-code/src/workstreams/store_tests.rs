//! Tests for `workstreams::store` (DOC-48 §3, AC-1.2, AC-1.4, AC-6.1, AC-6.2).

use super::*;
use tempfile::TempDir;

fn store_file(dir: &TempDir) -> PathBuf {
    dir.path().join("workstreams-test.json")
}

#[tokio::test]
async fn store_load_save_round_trip() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);

    let mut store = WorkstreamStore::load(&path).await.expect("load empty");
    assert!(store.list().await.expect("list").is_empty());

    let id = store
        .create("Token rotation hardening")
        .await
        .expect("create");

    let mut store2 = WorkstreamStore::load(&path).await.expect("reload");
    let ws = store2.get(id).await.expect("get after reload");
    assert_eq!(ws.id, id);
    assert_eq!(ws.name, "Token rotation hardening");
}

#[tokio::test]
async fn store_load_for_binding_derives_filename() {
    let dir = TempDir::new().expect("tempdir");
    let binding = ProjectBinding::None;
    let mut store = WorkstreamStore::load_for_binding(dir.path(), &binding)
        .await
        .expect("load_for_binding");
    store.create("x").await.expect("create");
    assert!(dir.path().join("workstreams-projectless.json").exists());
}

#[tokio::test]
async fn create_persists_and_is_gettable() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");

    let id = store.create("Auth refactoring").await.expect("create");
    let ws = store.get(id).await.expect("get");
    assert_eq!(ws.name, "Auth refactoring");
    assert_eq!(store.list().await.expect("list").len(), 1);
}

#[tokio::test]
async fn get_missing_id_returns_not_found() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let missing = WorkstreamId::new();
    assert!(matches!(
        store.get(missing).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn list_returns_all_records() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    store.create("A").await.expect("create A");
    store.create("B").await.expect("create B");
    let all = store.list().await.expect("list");
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn set_active_persists_pointer() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");

    store.set_active(Some(id)).await.expect("set_active");
    assert_eq!(
        store.active_workstream_id().await.expect("active"),
        Some(id)
    );

    store.set_active(None).await.expect("clear active");
    assert_eq!(store.active_workstream_id().await.expect("active"), None);
}

#[tokio::test]
async fn set_active_rejects_unknown_id() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let unknown = WorkstreamId::new();
    assert!(matches!(
        store.set_active(Some(unknown)).await,
        Err(StoreError::NotFound(_))
    ));
}

/// AC-1.4/AC-6.2: on daemon restart, the persisted active pointer is
/// restored (its workstream still exists).
#[tokio::test]
async fn reconcile_restores_active_pointer() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);

    let mut writer = WorkstreamStore::load(&path).await.expect("load writer");
    let id = writer.create("A").await.expect("create");
    writer.set_active(Some(id)).await.expect("activate");
    drop(writer);

    // A fresh store handle stands in for the daemon reloading after restart.
    let mut restarted = WorkstreamStore::load(&path).await.expect("load restarted");
    let outcome = restarted.reconcile_on_boot().await.expect("reconcile");
    assert_eq!(outcome.workstream_count, 1);
    assert_eq!(outcome.active_restored, Some(id));
    assert!(!outcome.active_cleared);
    assert_eq!(
        restarted.active_workstream_id().await.expect("active"),
        Some(id),
        "restart must not silently change which workstream is active"
    );
}

/// AC-6.1: reconciliation never deletes a workstream record, even the one
/// referenced by a since-vanished active pointer.
#[tokio::test]
async fn reconcile_clears_active_when_target_missing() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);
    let dangling_id = WorkstreamId::new();

    // Simulate a store file whose active pointer refers to a workstream that
    // is no longer present (e.g. hand-edited, or a future prune tool).
    let raw = serde_json::json!({
        "version": "1.0",
        "active_workstream_id": dangling_id,
        "workstreams": [],
    });
    tokio::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap())
        .await
        .expect("seed raw file");

    let mut store = WorkstreamStore::load(&path).await.expect("load");
    assert_eq!(
        store.active_workstream_id().await.expect("active"),
        Some(dangling_id),
        "precondition: dangling pointer is present before reconciliation"
    );

    let outcome = store.reconcile_on_boot().await.expect("reconcile");
    assert!(outcome.active_cleared);
    assert_eq!(outcome.active_restored, None);
    assert_eq!(store.active_workstream_id().await.expect("active"), None);

    // Persisted to disk, not just in-memory.
    let mut reloaded = WorkstreamStore::load(&path).await.expect("reload");
    assert_eq!(
        reloaded.active_workstream_id().await.expect("active"),
        None,
        "cleared pointer must be persisted"
    );
}

#[tokio::test]
async fn reconcile_is_noop_with_no_active_pointer() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    store.create("A").await.expect("create");

    let outcome = store.reconcile_on_boot().await.expect("reconcile");
    assert_eq!(outcome.workstream_count, 1);
    assert_eq!(outcome.active_restored, None);
    assert!(!outcome.active_cleared);
}

/// AC-6.1: reconciliation is non-destructive even across multiple
/// workstreams — every record must survive.
#[tokio::test]
async fn reconcile_preserves_every_workstream_record() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);
    let mut store = WorkstreamStore::load(&path).await.expect("load");
    let a = store.create("A").await.expect("create A");
    let b = store.create("B").await.expect("create B");
    store.set_active(Some(a)).await.expect("activate A");
    drop(store);

    let mut restarted = WorkstreamStore::load(&path).await.expect("reload");
    restarted.reconcile_on_boot().await.expect("reconcile");
    let all = restarted.list().await.expect("list");
    assert_eq!(
        all.len(),
        2,
        "no workstream may be deleted by reconciliation"
    );
    assert!(all.iter().any(|w| w.id == a));
    assert!(all.iter().any(|w| w.id == b));
}

#[tokio::test]
async fn load_returns_error_on_corrupt_file() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);
    tokio::fs::write(&path, b"{ not valid json ]")
        .await
        .expect("write corrupt file");

    let result = WorkstreamStore::load(&path).await;
    assert!(
        matches!(result, Err(StoreError::Serialize(_))),
        "a corrupt file must surface a structured error, not silently reset to empty"
    );
}

/// Why: another process (or a second store handle, standing in for one)
/// writing the shared file must be picked up on the next read rather than
/// served stale state forever (mirrors `SessionStore`'s
/// `store_reload_picks_up_external_write`).
#[tokio::test]
async fn store_reload_picks_up_external_write() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);

    // Store A seeds one workstream and reads it into memory.
    let mut store_a = WorkstreamStore::load(&path).await.expect("load A");
    let id_a = store_a.create("A").await.expect("create in A");
    assert_eq!(store_a.list().await.expect("list A before").len(), 1);

    // Store B (a different process's view) adds a second workstream and
    // activates it.
    let mut store_b = WorkstreamStore::load(&path).await.expect("load B");
    let id_b = store_b.create("B").await.expect("create in B");
    store_b.set_active(Some(id_b)).await.expect("activate B");

    // Store A must now observe both records and B's activation, not its
    // stale one-record view.
    let all = store_a.list().await.expect("list A after");
    assert_eq!(all.len(), 2, "A must reload B's externally-written record");
    assert!(all.iter().any(|w| w.id == id_a));
    assert!(all.iter().any(|w| w.id == id_b));
    assert_eq!(
        store_a.active_workstream_id().await.expect("active"),
        Some(id_b)
    );
}

/// §4.4: closing a non-active workstream just flips the metadata flag and
/// persists it — the active pointer (if any, pointing elsewhere) is
/// untouched.
#[tokio::test]
async fn close_marks_closed_and_persists() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);
    let mut store = WorkstreamStore::load(&path).await.expect("load");
    let a = store.create("A").await.expect("create A");
    let b = store.create("B").await.expect("create B");
    store.set_active(Some(a)).await.expect("activate A");

    let closed = store.close(b).await.expect("close B");
    assert!(closed.is_closed());
    assert_eq!(
        store.active_workstream_id().await.expect("active"),
        Some(a),
        "closing a non-active workstream must not touch the active pointer"
    );

    let mut reloaded = WorkstreamStore::load(&path).await.expect("reload");
    let ws = reloaded.get(b).await.expect("get after reload");
    assert!(ws.is_closed(), "closed flag must be persisted");
}

/// §4.4: closing the ACTIVE workstream must auto-deactivate it (clear
/// `active_workstream_id` to `null`).
#[tokio::test]
async fn close_active_workstream_clears_active_pointer() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");
    store.set_active(Some(id)).await.expect("activate");

    store.close(id).await.expect("close active workstream");

    assert_eq!(
        store.active_workstream_id().await.expect("active"),
        None,
        "closing the active workstream must clear the active pointer"
    );
}

#[tokio::test]
async fn close_missing_id_returns_not_found() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let missing = WorkstreamId::new();
    assert!(matches!(
        store.close(missing).await,
        Err(StoreError::NotFound(_))
    ));
}

/// Closing an already-closed workstream is not an error (§4.4 does not
/// require re-close to fail).
#[tokio::test]
async fn close_is_idempotent() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");

    store.close(id).await.expect("first close");
    let second = store.close(id).await.expect("second close must not error");
    assert!(second.is_closed());
}

/// Renaming must overwrite `name`, refresh `updated_at`, and persist across
/// a reload.
#[tokio::test]
async fn rename_updates_name_and_persists() {
    let dir = TempDir::new().expect("tempdir");
    let path = store_file(&dir);
    let mut store = WorkstreamStore::load(&path).await.expect("load");
    let id = store.create("original").await.expect("create");
    let before = store.get(id).await.expect("get before rename");

    let renamed = store.rename(id, "renamed").await.expect("rename");
    assert_eq!(renamed.name, "renamed");
    assert!(renamed.updated_at >= before.updated_at);

    let mut reloaded = WorkstreamStore::load(&path).await.expect("reload");
    let ws = reloaded.get(id).await.expect("get after reload");
    assert_eq!(ws.name, "renamed", "rename must be persisted");
}

/// Renaming an unknown id must return `NotFound`.
#[tokio::test]
async fn rename_missing_id_returns_not_found() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let missing = WorkstreamId::new();
    assert!(matches!(
        store.rename(missing, "new name").await,
        Err(StoreError::NotFound(_))
    ));
}

/// A closed workstream is still renamable — closure only rejects new
/// session bindings (§4.4), not label edits.
#[tokio::test]
async fn rename_closed_workstream_succeeds() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("original").await.expect("create");
    store.close(id).await.expect("close");

    let renamed = store
        .rename(id, "renamed after close")
        .await
        .expect("rename closed");
    assert_eq!(renamed.name, "renamed after close");
    assert!(renamed.is_closed(), "rename must not clear the closed flag");
}

#[tokio::test]
async fn store_reload_noop_when_unchanged() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");

    store.reload_if_changed().await.expect("reload no-op");
    let ws = store.get(id).await.expect("get");
    assert_eq!(ws.id, id);
}

// -- Session binding (DOC-48 §4.1/§2, issue #3298) --

#[tokio::test]
async fn bind_session_appends_and_persists() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");

    store.bind_session(id, "s-1").await.expect("bind");
    let ws = store.get(id).await.expect("get");
    assert_eq!(ws.session_ids, vec!["s-1".to_string()]);

    // Persisted, not just in-memory.
    let mut reloaded = WorkstreamStore::load(store_file(&dir))
        .await
        .expect("reload");
    let ws = reloaded.get(id).await.expect("get after reload");
    assert_eq!(ws.session_ids, vec!["s-1".to_string()]);
}

#[tokio::test]
async fn bind_session_is_idempotent_for_same_workstream() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");

    store.bind_session(id, "s-1").await.expect("first bind");
    store
        .bind_session(id, "s-1")
        .await
        .expect("re-binding to the SAME workstream must not error");
    let ws = store.get(id).await.expect("get");
    assert_eq!(
        ws.session_ids,
        vec!["s-1".to_string()],
        "must not double-append on the idempotent path"
    );
}

#[tokio::test]
async fn bind_session_rejects_double_binding_to_different_workstream() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let a = store.create("A").await.expect("create a");
    let b = store.create("B").await.expect("create b");

    store.bind_session(a, "s-1").await.expect("bind to a");
    assert!(matches!(
        store.bind_session(b, "s-1").await,
        Err(BindError::AlreadyBound(id)) if id == "s-1"
    ));
}

#[tokio::test]
async fn bind_session_rejects_unknown_workstream() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let missing = WorkstreamId::new();

    assert!(matches!(
        store.bind_session(missing, "s-1").await,
        Err(BindError::NotFound(_))
    ));
}

#[tokio::test]
async fn bind_session_rejects_closed_workstream() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = WorkstreamStore::load(store_file(&dir)).await.expect("load");
    let id = store.create("A").await.expect("create");
    store.close(id).await.expect("close");

    assert!(matches!(
        store.bind_session(id, "s-1").await,
        Err(BindError::Closed(_))
    ));
}
