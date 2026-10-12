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

/// #9719: L1 serves each entry from its live drawer-table row.
///
/// Why: `l1_drawers` holds copies taken at the last write. A change made in
/// place to the drawer table (an expiry, a backdate) reached L2 but not L1, so
/// L1 served an expired slot and ranked a stale snapshot as fresh.
/// What: remembers a drawer, then edits its row in place: a new `created_at`
/// must reach the L1 result, and a past `expires_at` must drop it.
/// Test: this test.
#[tokio::test]
async fn l1_serves_the_live_row_not_its_cached_copy() {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = disk_palace(dir.path(), "l1-live-row");
    let handle = PalaceHandle::open(&palace).expect("open");
    let id = remember(&handle, "Axolotls regrow their limbs").await;
    let backdated = chrono::Utc::now() - chrono::Duration::days(30);
    edit_row(&handle, id, |d| d.created_at = backdated);

    let served = retrieve_l0_l1(&handle);
    let row = served.iter().find(|r| r.drawer.id == id).expect("in L1");
    assert_eq!(row.drawer.created_at, backdated, "L1 served a stale copy");

    edit_row(&handle, id, |d| {
        d.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
    });
    assert!(
        !l0_l1_ids(&handle).contains(&id),
        "#9719: L1 served a drawer whose live row has expired"
    );
}

fn edit_row(handle: &PalaceHandle, id: Uuid, edit: impl FnOnce(&mut Drawer)) {
    let mut drawers = handle.drawers.write();
    edit(
        drawers
            .iter_mut()
            .find(|d| d.id == id)
            .expect("row present"),
    );
}

/// #9719: a wing scope bounds L1; a room scope and no scope do not.
///
/// Why: the live L1 holds the newest drawers of every wing, so an unscoped L1
/// handed a wing-scoped recall another wing's drawer.
/// What: one drawer per wing in L1; `retrieve_l0_l1_scoped` under the
/// engineer wing serves only the engineer drawer, while `All` and a `Room`
/// scope serve both.
/// Test: this test.
#[test]
fn a_wing_scoped_recall_drops_another_wings_l1_drawer() {
    let dir = tempdir().expect("tempdir");
    let handle = super::tests::make_handle(dir.path());
    let (engineer, eng_room) = super::tests::wing_with_room(&handle, "engineer", "Planning");
    let (_pm, pm_room) = super::tests::wing_with_room(&handle, "pm", "Planning");
    let eng = Drawer::new(eng_room, "the ingest retry budget is three");
    let pm = Drawer::new(pm_room, "the launch review is on Thursday");
    let (eng_id, pm_id) = (eng.id, pm.id);
    handle.add_drawer(eng);
    handle.add_drawer(pm);
    handle.refresh_l1();

    let ids = |scope: &RecallScope| -> Vec<Uuid> {
        retrieve_l0_l1_scoped(&handle, scope)
            .iter()
            .map(|r| r.drawer.id)
            .collect()
    };
    let wing = ids(&RecallScope::Wing(engineer));
    assert!(wing.contains(&eng_id), "the wing's own drawer is served");
    assert!(
        !wing.contains(&pm_id),
        "#9719: another wing's L1 drawer leaked"
    );
    for scope in [RecallScope::All, RecallScope::Room(RoomType::Planning)] {
        let all = ids(&scope);
        assert!(all.contains(&eng_id) && all.contains(&pm_id), "{scope:?}");
    }
}

/// #9719: a query-lane hit replaces its L1 copy in the merged recall.
///
/// Why: with a live L1 most lane hits are also L1 entries; kept as layer 1
/// they escape the `min_score` floor and stop reading as query matches.
/// What: `dedup_extend` over an L1 entry and a lane hit on the same drawer
/// keeps the lane row; an L1 entry with no hit stays; a new hit is appended.
/// Test: this test.
#[test]
fn a_lane_hit_replaces_its_l1_copy() {
    let hit = Drawer::new(Uuid::new_v4(), "hit");
    let kept = Drawer::new(Uuid::new_v4(), "kept");
    let new = Drawer::new(Uuid::new_v4(), "new");
    let row = |d: &Drawer, score: f32, layer: u8| RecallResult {
        drawer: d.clone(),
        score,
        layer,
    };
    let mut base = vec![row(&hit, 0.9, 1), row(&kept, 0.1, 1)];
    super::layers::dedup_extend(&mut base, vec![row(&hit, 0.7, 2), row(&new, 0.6, 2)]);
    let got: Vec<(Uuid, u8)> = base.iter().map(|r| (r.drawer.id, r.layer)).collect();
    assert_eq!(got, vec![(hit.id, 2), (kept.id, 1), (new.id, 2)]);
}

/// An unstamped palace opened by a non-maintainer, which adopts nothing.
fn unstamped(dir: &std::path::Path, id: &str) -> (Palace, Arc<PalaceHandle>) {
    let palace = disk_palace(dir, id);
    let handle = PalaceHandle::open_with_intent_purging(&palace, OpenIntent::ReadOnlyClient, false)
        .expect("non-maintainer open");
    assert!(
        !handle
            .kg
            .store()
            .drawers_authoritative()
            .expect("read marker"),
        "precondition: the palace is unstamped"
    );
    (palace, handle)
}

/// #9722 critic: a degraded drawer load adopts nothing.
///
/// Why: a row that failed to load may be live in redb with newer content than
/// its snapshot copy; adopting would overwrite it and stamp the marker, after
/// which a forgotten drawer revived this way stays live.
/// What: `maybe_adopt` on an unstamped, writable palace with `degraded` set
/// writes no row and leaves the marker unstamped.
/// Test: this test.
#[test]
fn a_degraded_load_adopts_nothing() {
    let dir = tempdir().expect("tempdir");
    let (palace, handle) = unstamped(dir.path(), "l1-degraded-adopt");
    let snap = Drawer::new(Uuid::new_v4(), "a snapshot copy of an unreadable row");
    let gap_filled = std::collections::HashSet::from([snap.id]);

    let stamped = super::l1::maybe_adopt(
        &handle.kg,
        &Ok(false),
        true,
        true,
        std::slice::from_ref(&snap),
        &gap_filled,
        &palace.id,
    );

    assert!(!stamped, "a degraded load must not stamp the marker");
    assert!(handle.kg.load_drawer(snap.id).expect("load").is_none());
    assert!(
        !handle
            .kg
            .store()
            .drawers_authoritative()
            .expect("read marker")
    );
}

/// #9722 critic: a snapshot-only drawer carrying a `fact_key` is not adopted.
///
/// Why: `fact_key` postdates #32, so such a drawer is a stale id; adopting it
/// would also move the Tier C slot off its live claimant.
/// What: redb holds the live claimant of a key; the snapshot holds another
/// drawer on the same key. A maintainer open stamps the marker, writes no row
/// for the snapshot drawer, and the key still resolves to the claimant.
/// Test: this test.
#[test]
fn a_snapshot_only_fact_key_drawer_is_not_adopted() {
    let dir = tempdir().expect("tempdir");
    let (palace, first) = unstamped(dir.path(), "l1-fact-key-adopt");
    let mut claimant = Drawer::new(Uuid::new_v4(), "the live claimant of the slot");
    claimant.fact_key = Some("pr:1/state".into());
    first
        .kg
        .upsert_drawer_sync(&claimant)
        .expect("persist claimant");
    let mut stale = Drawer::new(Uuid::new_v4(), "a stale snapshot copy of the slot");
    stale.fact_key = claimant.fact_key.clone();
    L1Cache::save_l1_cache(std::slice::from_ref(&stale), &palace.data_dir).expect("seed");
    drop(first);

    let opened = PalaceHandle::open(&palace).expect("maintainer open");

    assert!(
        opened
            .kg
            .store()
            .drawers_authoritative()
            .expect("read marker")
    );
    assert!(
        opened.kg.load_drawer(stale.id).expect("load").is_none(),
        "#9722: a snapshot-only fact_key drawer was adopted"
    );
    assert_eq!(
        opened
            .kg
            .drawer_id_for_fact_key("pr:1/state")
            .expect("index"),
        Some(claimant.id),
        "#9722: adoption moved the Tier C slot"
    );
}

/// #9722 critic: an adoption that overran its budget never commits.
///
/// Why: an overrun write used to land when the redb writer let go. A `forget`
/// issued after the open returned could take the lock first and delete
/// nothing, and the late adoption then wrote the drawer back and stamped the
/// marker.
/// What: holds a raw write transaction, opens as maintainer (the adoption
/// overruns), releases the transaction, waits for the helper to finish, and
/// asserts the marker is unstamped and no snapshot-only row was written.
/// Test: this test.
#[test]
fn an_adoption_that_overran_its_budget_never_commits() {
    let dir = tempdir().expect("tempdir");
    let (palace, first) = unstamped(dir.path(), "l1-late-adopt");
    let legacy = Drawer::new(Uuid::new_v4(), "a pre-persistence drawer in the snapshot");
    let id = legacy.id;
    L1Cache::save_l1_cache(&[legacy], &palace.data_dir).expect("seed snapshot");
    let store = first.kg.store();
    let db = store.db_for_test();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let stuck = std::thread::spawn(move || {
        let wtx = db.begin_write().expect("hold the write lock");
        held_tx.send(()).expect("signal held");
        let _ = release_rx.recv();
        drop(wtx); // abort: never committed
    });
    held_rx.recv().expect("write lock held");

    let overran = PalaceHandle::open(&palace).expect("maintainer open behind the stuck write");
    assert!(
        overran.drawers.read().iter().any(|d| d.id == id),
        "the gap-fill still serves the snapshot-only drawer"
    );
    release_tx.send(()).expect("release");
    stuck.join().expect("stuck writer");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let slot = loop {
        if let Some(slot) = store.try_claim_open_write() {
            break slot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the adoption helper never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    drop(slot);

    assert!(
        !store.drawers_authoritative().expect("read marker"),
        "#9722: a late adoption stamped the marker"
    );
    assert!(
        first.kg.load_drawer(id).expect("load").is_none(),
        "#9722: a late adoption wrote a snapshot-only row"
    );
}
