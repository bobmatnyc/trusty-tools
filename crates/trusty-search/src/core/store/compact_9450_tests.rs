//! Issue #9450 fix round — compaction under concurrent writes, queued
//! callers, and a crash between the snapshot renames.
//!
//! Why: the first #9450 compaction held the store's mutation gate for the
//! whole rebuild, let two queued callers rebuild twice, and trusted a heal
//! marker that a crash could publish beside the old graph.
//! What: one test per defect, each red on the pre-fix code. Shares the
//! corpus helpers of `super::tests_9450`.
//! Test: this module. Run with `cargo test -p trusty-search --lib compact_9450_tests`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;

use super::tests_9450::{assert_every_key_present, legacy_snapshot, load, seeded_store, sidecar};
use super::types::{CompactMode, VectorStore};
use super::usearch_compact::GRAPH_HEAL_EPOCH;

/// #9450 fix round: a write that arrives while a compaction builds its new
/// graph waits only for the vector copy, never for the build. The raced
/// compaction is abandoned: the churn count is not reset, the heal marker is
/// not advanced, and no key is lost.
/// Red before the fix: the build ran under `save_lock`, so the write below
/// waited out the whole (held) build and timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_during_a_compaction_waits_only_for_the_copy() {
    let (store, mut items) = seeded_store(5_000, 17).await;
    let store = Arc::new(store);
    for (id, _) in items.drain(4_000..4_100) {
        store.remove(&id).await.expect("remove");
    }
    // A pre-#9450 marker, so an advance would show.
    store.compact.restore(store.churn_since_compact(), 0);
    let churn_before = store.churn_since_compact();

    // Hold the build at its first add until the write below has finished.
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let hold = std::sync::Mutex::new((Some(started_tx), release_rx));
    let fault: super::usearch_requant::RebuildFault = Arc::new(move |_| {
        let mut hold = hold.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(started) = hold.0.take() {
            let _ = started.send(());
            let _ = hold.1.recv_timeout(Duration::from_secs(60));
        }
        Ok(())
    });
    let compaction = tokio::spawn({
        let store = store.clone();
        async move { store.compact_with_fault(fault).await }
    });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(60)))
        .await
        .expect("join")
        .expect("the build started");

    let started = std::time::Instant::now();
    let write = tokio::time::timeout(Duration::from_secs(3), store.remove("c0")).await;
    let waited = started.elapsed();
    let _ = release_tx.send(());
    let compacted = compaction.await.expect("compaction task");

    eprintln!("#9450: a write during the build waited {waited:?}");
    let write = write.unwrap_or_else(|_| {
        panic!(
            "#9450: the write waited {waited:?} on a held build — it must wait only for the copy"
        )
    });
    write.expect("remove");
    assert!(
        waited < Duration::from_secs(1),
        "#9450: the write waited {waited:?}"
    );
    let err = compacted.expect_err("a write during the build abandons the swap");
    assert!(err.to_string().contains("abandoned"), "{err}");
    assert_eq!(
        store.churn_since_compact(),
        churn_before + 1,
        "the raced compaction must not reset churn"
    );
    assert_eq!(store.graph_heal_epoch(), 0, "no marker advanced");
    items.retain(|(id, _)| id != "c0");
    assert_every_key_present(&store, &items, "after a raced compaction").await;
}

/// #9450 fix round: two `IfDue` callers queued on the heal gate rebuild the
/// graph once; the second finds churn already cleared.
/// Red before the fix: both passed the threshold check before the gate, and
/// the second rebuilt a freshly compacted graph.
#[tokio::test]
async fn two_queued_compactions_rebuild_once() {
    let (store, mut items) = seeded_store(1_000, 19).await;
    for (id, _) in items.drain(900..) {
        store.remove(&id).await.expect("remove");
    }
    // 100 churn against a threshold of max(900 / 10, 32) = 90.
    let (first, second) = tokio::join!(
        store.compact_graph_now(CompactMode::IfDue),
        store.compact_graph_now(CompactMode::IfDue)
    );
    let rebuilt = [first.expect("first"), second.expect("second")]
        .iter()
        .filter(|r| r.is_some())
        .count();
    assert_eq!(rebuilt, 1, "#9450: two queued callers must rebuild once");
    assert_eq!(store.churn_since_compact(), 0);
    assert_every_key_present(&store, &items, "after the compaction").await;
}

/// #9450 fix round: a crash after the sidecar rename and before the binary
/// rename leaves a "healed" sidecar beside the old graph. The marker records
/// which graph file it describes, so the next load heals again.
/// Red before the fix: the reload trusted the marker and never healed.
#[tokio::test]
async fn a_crash_between_the_renames_heals_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, items) = legacy_snapshot(1_000, dir.path()).await;
    let store = load(&path).await;
    assert_eq!(
        store.graph_heal_epoch(),
        0,
        "a legacy sidecar has no marker"
    );
    store
        .compact_graph_now(CompactMode::Always)
        .await
        .expect("compact")
        .expect("report");
    assert_eq!(store.graph_heal_epoch(), GRAPH_HEAL_EPOCH);

    // The process dies between the two renames: the sidecar is published,
    // the binary is not.
    let crashed = store
        .save_with_publisher(&path, |path, key_map| {
            std::fs::write(
                path.with_extension("keys.json"),
                serde_json::to_vec(key_map)?,
            )?;
            Err(anyhow!("crash before the binary rename"))
        })
        .await;
    assert!(crashed.is_err());
    assert_eq!(sidecar(&path)["heal_epoch"], GRAPH_HEAL_EPOCH);
    drop(store);

    let reloaded = load(&path).await;
    assert_eq!(
        reloaded.graph_heal_epoch(),
        0,
        "#9450: the marker does not describe the old graph, so it heals again"
    );
    assert_every_key_present(&reloaded, &items, "after the crash").await;
    let healed = reloaded.heal_on_load(&|| false).await.expect("heal");
    assert!(healed.is_some(), "the old graph is compacted again");
    drop(reloaded);
    assert_eq!(load(&path).await.graph_heal_epoch(), GRAPH_HEAL_EPOCH);
}
