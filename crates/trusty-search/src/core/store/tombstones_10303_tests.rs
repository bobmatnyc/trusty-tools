//! Key-based tombstones for copy-free removal from a view-mode index (#10303).
//!
//! Why: the closure conditions of #10303 — a view-mode removal writes
//! tombstones and copies nothing, search filters them, a crash between the
//! tombstone write and the graph apply loses nothing, an old reader still
//! loads the directory, and an untrustworthy tombstone file is an error.
//! What: each test drives the real `UsearchStore` against a tempdir snapshot.
//! Test: this module. Run with `cargo test -p trusty-search tombstones_10303`.

use std::sync::atomic::Ordering;

use super::staging_reap::reap_orphan_staging_files;
use super::types::VectorStore;
use super::usearch_store::{staging_path, UsearchStore};
use super::usearch_tombstones::tombstones_path;

/// `kill(pid, 0)` answers ESRCH for this pid on every platform (see `tests_2936`).
const DEAD_PID: u32 = 2_000_000_000;

const ALPHA: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

/// Save `alpha`, `beta`, `gamma` to `<dir>/hnsw.usearch` and load it as a view.
async fn view_of_three(dir: &std::path::Path) -> (std::path::PathBuf, UsearchStore) {
    let path = dir.join("hnsw.usearch");
    let store = UsearchStore::new(4).unwrap();
    store.upsert("alpha", ALPHA.to_vec()).await.unwrap();
    store
        .upsert("beta", vec![0.9, 0.1, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("gamma", vec![0.0, 0.0, 1.0, 0.0])
        .await
        .unwrap();
    store.save(&path).await.unwrap();
    drop(store);
    let view = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert!(view.in_view_mode());
    (path, view)
}

fn hit_ids(hits: &[super::types::VectorHit]) -> Vec<&str> {
    hits.iter().map(|h| h.chunk_id.as_str()).collect()
}

/// Why: the point of #10303 — a view-mode removal must not copy the graph to
/// the heap, and search must never answer with the removed key or let it
/// take a result slot. Fails on the pre-#10303 code, where `remove` promotes.
/// What: removes the best match from a view, then checks the mode, the
/// promotion count, the tombstone file, the untouched pair, and a top-2 search.
/// Test: this test.
#[tokio::test]
async fn a_view_remove_writes_tombstones_and_copies_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    assert!(
        !tombstones_path(&path).exists(),
        "an old index has no tombstone file"
    );
    let binary = std::fs::read(&path).unwrap();
    let sidecar = std::fs::read(path.with_extension("keys.json")).unwrap();

    view.remove("alpha").await.unwrap();

    assert!(view.in_view_mode(), "a view-mode removal must not promote");
    assert_eq!(view.promotions(), 0, "no heap copy was made");
    assert!(tombstones_path(&path).exists(), "the removal is durable");
    assert_eq!(std::fs::read(&path).unwrap(), binary, "binary untouched");
    assert_eq!(
        std::fs::read(path.with_extension("keys.json")).unwrap(),
        sidecar,
        "sidecar untouched until the next mutable save"
    );
    assert!(!view.contains("alpha").await);
    assert_eq!(view.len().await.unwrap(), 2);
    let hits = view.search(&ALPHA, 2).await.unwrap();
    assert_eq!(
        hit_ids(&hits),
        vec!["beta", "gamma"],
        "the slot is not wasted"
    );
}

/// Why: a crash right after the tombstone write must not resurrect the key.
/// Fails on the pre-#10303 code: the promoted removal was never saved, so a
/// reload brings `alpha` back.
/// What: view remove, drop the store unsaved, reload.
/// Test: this test.
#[tokio::test]
async fn a_view_remove_survives_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    view.remove("alpha").await.unwrap();
    drop(view);

    let reloaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert!(reloaded.in_view_mode());
    assert!(
        !reloaded.contains("alpha").await,
        "tombstoned id stays removed"
    );
    assert!(reloaded.contains("beta").await && reloaded.contains("gamma").await);
    assert_eq!(reloaded.len().await.unwrap(), 2);
    let hits = reloaded.search(&ALPHA, 3).await.unwrap();
    assert!(!hit_ids(&hits).contains(&"alpha"));
}

/// Why: closure condition "crash between tombstone write and graph apply".
/// The vector is still in the graph at the crash; the next promote must
/// unlink it, and the next save must publish a pair without it and delete
/// the file. Fails on the pre-#10303 code at the first `contains` check.
/// What: view remove, crash (drop unsaved), reload, write (promotes and
/// applies), save, reload, then inspect the graph, sidecar and file.
/// Test: this test.
#[tokio::test]
async fn a_crash_between_tombstone_write_and_graph_apply_loses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    view.remove("alpha").await.unwrap();
    drop(view);

    let store = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert!(!store.contains("alpha").await);
    store
        .upsert("delta", vec![0.0, 0.0, 0.0, 1.0])
        .await
        .unwrap();
    assert!(!store.in_view_mode());
    assert_eq!(store.index.read().await.size(), 3, "alpha left the graph");
    assert!(store.dirty.load(Ordering::Acquire), "the unlink is unsaved");
    store.save(&path).await.unwrap();
    assert!(
        !tombstones_path(&path).exists(),
        "a publish deletes the file"
    );
    drop(store);

    let sidecar = std::fs::read_to_string(path.with_extension("keys.json")).unwrap();
    assert!(
        !sidecar.contains("\"alpha\""),
        "the new sidecar drops alpha"
    );
    let reloaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert_eq!(reloaded.len().await.unwrap(), 3);
    assert_eq!(reloaded.index.read().await.size(), 3);
    for id in ["beta", "gamma", "delta"] {
        assert!(reloaded.contains(id).await, "{id} must survive");
    }
}

/// Why: a crash after a publish but before the file's deletion leaves a
/// tombstone file beside a pair it does not describe. It must never drop a
/// live key, even one that reuses a tombstoned key number (a fresh store).
/// What: tombstone key 1 (`alpha`), keep the file, publish a fresh store whose
/// key 1 is `fresh`, put the old file back, reload.
/// Test: this test.
#[tokio::test]
async fn a_stale_tombstone_file_never_drops_a_live_key() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    view.remove("alpha").await.unwrap();
    let stale = std::fs::read(tombstones_path(&path)).unwrap();
    drop(view);

    let fresh = UsearchStore::new(4).unwrap();
    fresh.upsert("fresh", ALPHA.to_vec()).await.unwrap();
    fresh.save(&path).await.unwrap();
    drop(fresh);
    std::fs::write(tombstones_path(&path), stale).unwrap();

    let reloaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert!(reloaded.contains("fresh").await, "a stale file is ignored");
    assert_eq!(reloaded.len().await.unwrap(), 1);
}

/// Why (Fail-Open): a present but unreadable or corrupt tombstone file must
/// fail the load, never read as "no tombstones" — that would resurrect every
/// removal. Fails on the pre-#10303 code, which ignores the file.
/// What: corrupt JSON, then an unknown schema version; both loads are `Err`.
/// Test: this test.
#[tokio::test]
async fn a_corrupt_tombstone_file_fails_the_load() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    drop(view);

    std::fs::write(tombstones_path(&path), b"{not json").unwrap();
    let err = UsearchStore::load_from(&path)
        .await
        .err()
        .expect("corrupt is an error");
    assert!(err.to_string().contains("corrupt"), "{err}");

    std::fs::write(
        tombstones_path(&path),
        br#"{"version":99,"sidecar_sha256":"x","keys":[1]}"#,
    )
    .unwrap();
    let err = UsearchStore::load_from(&path)
        .await
        .err()
        .expect("version is an error");
    assert!(err.to_string().contains("schema version 99"), "{err}");
}

/// Why: closure condition "old-reader fixture loading a tombstone-bearing
/// directory". A pre-#10303 binary reads only `hnsw.usearch` and
/// `hnsw.keys.json`; the #3970 torn-pairing guard must not fire on them.
/// What: after a view removal, load exactly the two files an old reader
/// knows (copied without the tombstone file) through the ordinary loader.
/// Test: this test.
#[tokio::test]
async fn an_old_reader_loads_a_tombstone_bearing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    view.remove("alpha").await.unwrap();
    drop(view);
    assert!(tombstones_path(&path).exists());

    let old = tempfile::tempdir().unwrap();
    let old_path = old.path().join("hnsw.usearch");
    std::fs::copy(&path, &old_path).unwrap();
    std::fs::copy(
        path.with_extension("keys.json"),
        old_path.with_extension("keys.json"),
    )
    .unwrap();
    let old_view = UsearchStore::load_from(&old_path)
        .await
        .unwrap()
        .expect("the old reader's guards pass");
    assert_eq!(
        old_view.len().await.unwrap(),
        3,
        "it sees the stale id, as #10303 records"
    );
}

/// Why: a tombstone write that fails must leave the id in place, so the
/// caller's retry removes it.
/// What: make the snapshot directory read-only, remove, expect `Err`.
/// Test: this test.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_tombstone_write_keeps_the_id() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let result = view.remove("alpha").await;

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "a failed write is reported");
    assert!(view.contains("alpha").await, "the id stays for a retry");
    assert!(view.in_view_mode());
    assert!(!tombstones_path(&path).exists());
}

/// Why: a batch removal must cost one tombstone write, not one per id.
/// What: `remove_many` with two known ids and one unknown id.
/// Test: this test.
#[tokio::test]
async fn remove_many_writes_one_tombstone_file_for_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (path, view) = view_of_three(dir.path()).await;
    let ids = ["alpha", "beta", "nope"].map(String::from);

    view.remove_many(&ids).await.unwrap();

    assert!(view.in_view_mode());
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tombstones_path(&path)).unwrap()).unwrap();
    assert_eq!(file["keys"].as_array().unwrap().len(), 2);
    assert_eq!(view.len().await.unwrap(), 1);
    assert!(view.contains("gamma").await);
}

/// Why: closure condition "staging_reap covers its tmp files". A process
/// killed between the tombstone write and its rename leaves one behind.
/// Fails on the pre-#10303 code, which reaps only the pair's staging files.
/// What: plant a dead-pid tombstone staging file and run the reaper.
/// Test: this test.
#[tokio::test]
async fn reap_removes_an_orphaned_tombstone_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    // The name `write_tombstones` stages under, with a dead pid in it.
    let ours = staging_path(&tombstones_path(&path), "json");
    let pid = std::process::id().to_string();
    let orphan = dir.path().join(
        ours.file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .replace(&pid, &DEAD_PID.to_string()),
    );
    std::fs::write(&orphan, b"x").unwrap();

    assert_eq!(reap_orphan_staging_files(&path), 1);
    assert!(!orphan.exists());
}
