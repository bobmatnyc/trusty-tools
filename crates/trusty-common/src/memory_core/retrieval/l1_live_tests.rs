//! Regression tests for the live L1 cache (#9719) and the L1 snapshot
//! gap-fill (#9722).
//!
//! Why: L1 was frozen at open (#9719), and a stale second handle's snapshot
//! could revive a forgotten drawer at the next open (#9722).
//! What: drives real on-disk palaces through `PalaceHandle::open`.
//! Test: this file IS the tests.

use super::*;
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::l1_cache::L1Cache;
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

fn disk_palace(dir: &std::path::Path, id: &str) -> Palace {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("create palace dir");
    palace
}

async fn remember(handle: &Arc<PalaceHandle>, text: &str) -> Uuid {
    handle
        .remember(text.into(), RoomType::General, vec![], 0.9)
        .await
        .expect("remember")
}

fn l0_l1_ids(handle: &PalaceHandle) -> Vec<Uuid> {
    retrieve_l0_l1(handle).iter().map(|r| r.drawer.id).collect()
}

/// #9719: a drawer added to a live handle is served by L0/L1 without a reopen.
///
/// Why: `l1_drawers` was set only at open and `refresh_l1` had no production
/// caller, so a write never reached L1 until a restart.
/// What: opens an empty palace, remembers one drawer on the live handle, and
/// asserts `retrieve_l0_l1` serves it.
/// Test: this test.
#[tokio::test]
async fn a_drawer_added_after_open_is_served_by_l0_l1() {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-add-after-open");
    let handle = PalaceHandle::open(&palace).expect("open");

    let id = remember(&handle, "Tardigrades survive the vacuum of space").await;

    assert!(
        l0_l1_ids(&handle).contains(&id),
        "#9719: a drawer remembered on a live handle must enter L1"
    );
}

/// #9722: a stale second handle must not revive a forgotten drawer on reopen.
///
/// Why: handle B, opened before handle A forgets a drawer, still holds that
/// drawer in its table. B's next `remember` rewrote `l1_cache.json` with it,
/// and the open-time gap-fill copied it back into the drawer table as live.
/// What: two handles on one palace; A forgets, B remembers, both drop, the
/// palace reopens, and the forgotten id is absent from the drawer table, from
/// L0/L1, and from `list_drawers`.
/// Test: this test.
#[tokio::test]
async fn a_stale_second_handle_does_not_revive_a_forgotten_drawer_on_reopen() {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-stale-handle");
    let a = PalaceHandle::open(&palace).expect("open A");
    let gone = remember(&a, "Mantis shrimp see twelve colour channels").await;
    a.flush().expect("flush A");

    let b = PalaceHandle::open(&palace).expect("open B");
    assert!(
        b.drawers.read().iter().any(|d| d.id == gone),
        "precondition: B opened before the forget and holds the drawer"
    );

    assert_eq!(
        a.forget(gone).await.expect("forget"),
        ForgetOutcome::Deleted
    );
    let kept = remember(&b, "Octopuses taste with their suckers").await;
    drop(a);
    drop(b);

    let reopened = PalaceHandle::open(&palace).expect("reopen");
    assert!(
        !reopened.drawers.read().iter().any(|d| d.id == gone),
        "#9722: the forgotten drawer was revived into the drawer table"
    );
    assert!(
        !l0_l1_ids(&reopened).contains(&gone),
        "#9722: the forgotten drawer is served by L0/L1"
    );
    assert!(
        !reopened
            .list_drawers(None, None, 100)
            .iter()
            .any(|d| d.id == gone),
        "#9722: the forgotten drawer is listed"
    );
    assert!(
        reopened.drawers.read().iter().any(|d| d.id == kept),
        "B's own write must survive the reopen"
    );
}

/// #9722 fail-open arm: an unreadable authority marker keeps the gap-fill.
///
/// Why: the snapshot may be the only copy of a pre-#32 drawer; failing toward
/// "authoritative" would hide it.
/// What: `gap_fill_needed` with an `Err` marker read answers `true`.
/// Test: this test.
#[test]
fn gap_fill_runs_when_the_marker_is_unreadable() {
    let palace = PalaceId::new("marker-unreadable");
    assert!(super::l1::gap_fill_needed(
        &Err(anyhow::anyhow!("kg_schema unreadable")),
        false,
        &palace
    ));
}

/// #9722 fail-open arm: a degraded drawer load keeps the gap-fill.
///
/// Why: rows skipped as unreadable may survive only in the snapshot.
/// What: a stamped marker skips the gap-fill only on a complete load.
/// Test: this test.
#[test]
fn gap_fill_runs_on_a_degraded_load_even_when_stamped() {
    let palace = PalaceId::new("degraded");
    assert!(super::l1::gap_fill_needed(&Ok(true), true, &palace));
    assert!(
        !super::l1::gap_fill_needed(&Ok(true), false, &palace),
        "a stamped palace with a complete load skips the gap-fill"
    );
}

/// #9722: the #32 gap-fill still serves a snapshot-only drawer, and adoption
/// stamps the marker only once that drawer is durable.
///
/// Why: retiring the gap-fill must never lose a pre-persistence drawer, and a
/// skipped or failed adoption must leave the palace unstamped.
/// What: seeds `l1_cache.json` with a drawer that has no redb row. With the
/// open-write slot held, an open serves the drawer and stamps nothing; once
/// the slot frees, the next open adopts the row and stamps the marker.
/// Test: this test.
#[tokio::test]
async fn a_legacy_snapshot_drawer_is_adopted_and_kept_when_adoption_is_skipped() {
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-legacy");
    let mut legacy = Drawer::new(Uuid::new_v4(), "A pre-persistence drawer in the snapshot");
    legacy.importance = 0.9;
    let id = legacy.id;
    L1Cache::save_l1_cache(&[legacy], &palace.data_dir).expect("seed snapshot");

    // A non-maintainer open adopts nothing; its store is the shared one.
    let non_maintainer =
        PalaceHandle::open_with_intent_purging(&palace, OpenIntent::ReadOnlyClient, false)
            .expect("non-maintainer open");
    let store = non_maintainer.kg.store();
    let claim = store
        .try_claim_open_write()
        .expect("claim the open-write slot");
    let skipped = PalaceHandle::open(&palace).expect("open while adoption is skipped");
    assert!(
        skipped.drawers.read().iter().any(|d| d.id == id),
        "the gap-fill must still serve a snapshot-only drawer"
    );
    assert!(l0_l1_ids(&skipped).contains(&id), "and L1 must serve it");
    assert!(
        !store.drawers_authoritative().expect("read marker"),
        "a skipped adoption must not stamp the marker"
    );
    drop(claim);
    drop(skipped);

    let adopted = PalaceHandle::open(&palace).expect("open with the slot free");
    assert!(
        store.drawers_authoritative().expect("read marker"),
        "adoption stamps the marker"
    );
    assert!(
        adopted.kg.load_drawer(id).expect("load row").is_some(),
        "the snapshot-only drawer is durable once the marker is stamped"
    );
}
