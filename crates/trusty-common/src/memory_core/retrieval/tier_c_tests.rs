//! Behavioural tests for the ADR-0028 Tier C write path (#4886).
//!
//! Why: the admission grammar is unit-tested inside `tier_c.rs`; what needs
//! end-to-end coverage is the part a pure function cannot express — that a
//! second write to a slot actually retires the first, that the displaced drawer
//! survives it, and that N writers racing on one slot still leave exactly one
//! claimant. The last of those is the invariant a sequential test structurally
//! cannot prove.
//! What: exercises `PalaceHandle::remember_with_options` against a real redb
//! store, asserting on both the durable rows (`kg.load_drawers`,
//! `kg.drawer_id_for_fact_key`) and the in-memory mirror.
//! Test: this file IS the tests — run with:
//!   cargo test -p trusty-common --features memory-core,embedder-test-support tier_c

use super::*;
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use chrono::{DateTime, Duration, DurationRound, Utc};
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

const SLOT: &str = "pr:4818/state";

fn init_embedder() {
    seed_shared_embedder_with_mock();
}

fn make_handle(dir: &std::path::Path) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    PalaceHandle::new(PalaceId::new("tier-c"), "Tier C palace".to_string(), vs, kg)
}

/// A shared `Arc<PalaceHandle>` backed by a real on-disk palace, which the
/// concurrency test needs so every task writes through one handle (and so one
/// per-palace write mutex).
fn open_shared_palace(dir: &std::path::Path, id: &str) -> Arc<PalaceHandle> {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: dir.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    PalaceHandle::open(&palace).unwrap()
}

async fn write(
    handle: &PalaceHandle,
    content: &str,
    fact_key: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) -> anyhow::Result<Uuid> {
    handle
        .remember_with_options(
            content.to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions {
                fact_key: fact_key.map(str::to_string),
                expires_at,
                ..RememberOptions::forced()
            },
        )
        .await
}

fn stored(handle: &PalaceHandle, id: Uuid) -> Drawer {
    handle
        .kg
        .load_drawers()
        .unwrap()
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("drawer {id} missing from redb"))
}

// ── The retirement invariant (ADR-0028 D5) ──────────────────────────────────

/// Writing an occupied slot moves the slot to the newcomer and leaves the
/// incumbent on disk (D5 "one slot, one live fact"; D6 "demoted, never
/// deleted").
#[tokio::test]
async fn tier_c_write_retires_the_prior_slot_occupant() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let first = write(
        &handle,
        "PR #4818 is in flight at head d39638482bfe8de462c02c4f40e02b56b16897ff",
        Some(SLOT),
        None,
    )
    .await
    .expect("first tier C write");
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(first),
        "the first write should own the slot"
    );

    let second = write(
        &handle,
        "PR #4818 merged as squash 4c412ae1 at head 59ae50d8, superseding the earlier SHA",
        Some(SLOT),
        None,
    )
    .await
    .expect("second tier C write");

    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(second),
        "the newer write must hold the slot"
    );
    let rows = handle.kg.load_drawers().unwrap();
    assert_eq!(rows.len(), 2, "no drawer row may be deleted (D6)");
    assert!(
        rows.iter().any(|d| d.id == first),
        "the superseded fact must survive as a readable Tier E drawer"
    );
}

/// The displaced drawer stops claiming the slot on its OWN row, not just in the
/// index — otherwise `load_drawers()` shows two drawers claiming one slot.
#[tokio::test]
async fn tier_c_retirement_clears_the_displaced_drawers_own_fact_key() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let first = write(&handle, "the in-flight state of PR 4818", Some(SLOT), None)
        .await
        .unwrap();
    let second = write(&handle, "the merged state of PR 4818", Some(SLOT), None)
        .await
        .unwrap();

    let retired = stored(&handle, first);
    assert_eq!(
        retired.fact_key, None,
        "a displaced drawer must stop claiming the slot on its own row"
    );
    assert_eq!(
        retired.expires_at, None,
        "supersession discharges the retirement condition; leaving the TTL set \
         would make the demoted record self-destruct at the next sweep"
    );
    assert_eq!(stored(&handle, second).fact_key.as_deref(), Some(SLOT));

    // Exactly one claimant across every durable row.
    let claimants: Vec<Uuid> = handle
        .kg
        .load_drawers()
        .unwrap()
        .into_iter()
        .filter(|d| d.fact_key.as_deref() == Some(SLOT))
        .map(|d| d.id)
        .collect();
    assert_eq!(claimants, vec![second]);

    // And the in-memory mirror agrees with disk.
    let in_memory: Vec<Uuid> = handle
        .drawers
        .read()
        .iter()
        .filter(|d| d.fact_key.as_deref() == Some(SLOT))
        .map(|d| d.id)
        .collect();
    assert_eq!(in_memory, vec![second]);
}

/// The concurrency guarantee. N writers race on ONE slot; the read-decide-write
/// sequence must be serialised or several of them read "free"/"same incumbent"
/// and all claim the slot at once — the PR #4895 shape, where 16 concurrent
/// writers were all admitted into 1 free slot.
///
/// A sequential test cannot prove this: it would pass against an entirely
/// unserialised implementation.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_tier_c_writes_to_one_slot_leave_exactly_one_claimant() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = open_shared_palace(dir.path(), "tier-c-concurrent");

    const WRITERS: usize = 16;
    let mut tasks = Vec::with_capacity(WRITERS);
    for i in 0..WRITERS {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            write(
                &h,
                &format!("concurrent tier C claim number {i} on the PR 4818 state slot"),
                Some(SLOT),
                None,
            )
            .await
        }));
    }
    let mut ids = Vec::with_capacity(WRITERS);
    for t in tasks {
        ids.push(t.await.expect("task panicked").expect("write failed"));
    }
    assert_eq!(ids.len(), WRITERS);

    let rows = handle.kg.load_drawers().unwrap();
    assert_eq!(
        rows.len(),
        WRITERS,
        "every write must land; retirement demotes, it never deletes"
    );

    let claimants: Vec<Uuid> = rows
        .iter()
        .filter(|d| d.fact_key.as_deref() == Some(SLOT))
        .map(|d| d.id)
        .collect();
    assert_eq!(
        claimants.len(),
        1,
        "exactly one drawer may claim the slot; got {} — {claimants:?}",
        claimants.len()
    );
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(claimants[0]),
        "the index and the winning row must name the same drawer"
    );
    assert!(
        rows.iter()
            .filter(|d| d.id != claimants[0])
            .all(|d| d.expires_at.is_none()),
        "every retired drawer must have had its TTL discharged"
    );
}

// ── Fail-closed admission (ADR-0028 D4) ─────────────────────────────────────

/// A slot request the grammar refuses degrades to Tier E — it is not admitted
/// with a warning, and it does not take the slot.
#[tokio::test]
async fn malformed_fact_key_degrades_to_tier_e() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let id = write(
        &handle,
        "a bare unnamespaced slot name",
        Some("state"),
        None,
    )
    .await
    .expect("the write itself still succeeds — it degrades, it does not fail");

    let d = stored(&handle, id);
    assert_eq!(d.fact_key, None, "a refused key must not be stored");
    assert_eq!(
        d.expires_at, None,
        "a refused write must not pick up the Tier C default TTL"
    );
    assert_eq!(handle.kg.drawer_id_for_fact_key("state").unwrap(), None);
}

/// An `expires_at` that has already elapsed declares no live window, so the
/// write is refused Tier C rather than admitted — and critically, it does NOT
/// retire the slot's live incumbent on its way past.
#[tokio::test]
async fn already_elapsed_expiry_degrades_to_tier_e() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let live = write(&handle, "the live state of PR 4818", Some(SLOT), None)
        .await
        .unwrap();
    let born_expired = write(
        &handle,
        "a fact whose retirement condition had already fired",
        Some(SLOT),
        Some(Utc::now() - Duration::hours(1)),
    )
    .await
    .unwrap();

    assert_eq!(stored(&handle, born_expired).fact_key, None);
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(live),
        "a refused write must not evict the live occupant"
    );
    assert!(
        stored(&handle, live).fact_key.is_some(),
        "the incumbent keeps its slot"
    );
}

/// Naming a slot without an expiry takes the 24-hour default (D4 condition 3).
#[tokio::test]
async fn tier_c_write_without_expiry_gets_the_default_ttl() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let before = Utc::now();
    let id = write(&handle, "current state of the PR", Some(SLOT), None)
        .await
        .unwrap();
    let after = Utc::now();

    let ttl = stored(&handle, id).expires_at.expect("default TTL applied");
    assert!(
        ttl >= before + Duration::hours(TIER_C_DEFAULT_TTL_HOURS)
            && ttl <= after + Duration::hours(TIER_C_DEFAULT_TTL_HOURS),
        "expected ~24h from the write instant, got {ttl}"
    );
}

/// `expires_at` without a slot is the pre-ADR-0028 field doing what it always
/// did: a plain TTL on an ordinary drawer, no slot, no privilege.
#[tokio::test]
async fn explicit_expiry_is_honoured_without_a_fact_key() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    // Millisecond-aligned: `DrawerRecord` stores timestamps as epoch millis, so
    // a sub-millisecond `Utc::now()` would not survive the round-trip.
    let ttl = (Utc::now() + Duration::hours(3))
        .duration_trunc(Duration::milliseconds(1))
        .unwrap();
    let id = write(
        &handle,
        "an ordinary drawer with a hand-set TTL",
        None,
        Some(ttl),
    )
    .await
    .unwrap();

    let d = stored(&handle, id);
    assert_eq!(d.expires_at, Some(ttl));
    assert_eq!(d.fact_key, None);
}

/// A write that names no slot and no TTL is byte-identical to today: the
/// regression guard for every pre-ADR-0028 caller.
#[tokio::test]
async fn a_write_naming_nothing_is_unchanged() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let id = write(
        &handle,
        "an ordinary drawer written the old way",
        None,
        None,
    )
    .await
    .unwrap();
    let d = stored(&handle, id);
    assert_eq!(d.fact_key, None);
    assert_eq!(d.expires_at, None);
}

// ── D6: demoted, never deleted ──────────────────────────────────────────────

/// An expired Tier C drawer is skipped by the reclamation sweep. Its
/// `expires_at` is the retirement condition D4 demanded, not a lifetime — the
/// read-time filter (#4885) already stops it being served, and deleting the row
/// would destroy the record D6 preserves and orphan #4887's pointer.
#[tokio::test]
async fn purge_expired_leaves_tier_c_drawers_alone() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let room_id = Uuid::new_v4();

    let mut tier_c = Drawer::new(room_id, "an expired current fact");
    tier_c.fact_key = Some(SLOT.to_string());
    tier_c.expires_at = Some(Utc::now() - Duration::days(1));
    let tier_c_id = tier_c.id;

    let mut ordinary = Drawer::new(room_id, "an expired ordinary drawer");
    ordinary.expires_at = Some(Utc::now() - Duration::days(1));
    let ordinary_id = ordinary.id;

    handle.add_drawer(tier_c);
    handle.add_drawer(ordinary);

    let pruned = handle.purge_expired().await.expect("purge");
    assert_eq!(pruned, 1, "only the ordinary drawer is reclaimable");

    let remaining: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(remaining.contains(&tier_c_id), "D6: demoted, never deleted");
    assert!(!remaining.contains(&ordinary_id));
}

/// The same exemption on the OTHER sweep — the inline prune inside
/// `PalaceHandle::open_with_intent`.
///
/// Why this is the one that matters: `purge_expired` has no non-test call site,
/// while the open-time prune runs on every palace open and is the only sweep a
/// production daemon actually executes. It also deletes from redb directly
/// (`delete_drawer_sync`), so an unexempted Tier C fact would be gone from disk,
/// not merely dropped from the in-memory table. Covering only `purge_expired`
/// would leave the live deletion path untested (#4909 review).
#[tokio::test]
async fn expired_tier_c_drawer_survives_the_open_time_sweep() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("sweep-palace");
    std::fs::create_dir_all(&data_dir).unwrap();
    let room_id = Uuid::new_v4();

    // Seed redb directly so the drawers are already durable when the palace is
    // opened — the open-time prune reads `kg.load_drawers()`, so a drawer
    // written through a handle after the fact would never meet that sweep.
    let mut tier_c = Drawer::new(room_id, "an expired current fact");
    tier_c.fact_key = Some(SLOT.to_string());
    tier_c.expires_at = Some(Utc::now() - Duration::days(1));
    let tier_c_id = tier_c.id;

    let mut ordinary = Drawer::new(room_id, "an expired ordinary drawer");
    ordinary.expires_at = Some(Utc::now() - Duration::days(1));
    let ordinary_id = ordinary.id;

    {
        let kg = KnowledgeGraph::open(&data_dir.join("kg.db")).unwrap();
        kg.upsert_drawer_sync(&tier_c).unwrap();
        kg.upsert_drawer_sync(&ordinary).unwrap();
    }

    let palace = Palace {
        id: PalaceId::new("sweep-palace"),
        name: "Sweep".into(),
        description: None,
        created_at: Utc::now(),
        data_dir,
    };
    let handle = PalaceHandle::open(&palace).expect("open palace");

    let loaded: Vec<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
    assert!(
        loaded.contains(&tier_c_id),
        "D6: the open-time sweep must not delete a Tier C fact"
    );
    assert!(
        !loaded.contains(&ordinary_id),
        "an ordinary expired drawer is still reclaimed — the exemption is \
         narrow, not a disabling of the sweep"
    );

    // And the exemption is a real reprieve on disk, not just in memory.
    let rows = handle.kg.load_drawers().unwrap();
    assert!(rows.iter().any(|d| d.id == tier_c_id), "row must survive");
    assert!(!rows.iter().any(|d| d.id == ordinary_id));
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(tier_c_id),
        "the surviving fact keeps its slot"
    );
}

// ── The incumbent-absent fallback (#6438) ───────────────────────────────────

/// Every drawer row in `rows` whose own `fact_key` still claims [`SLOT`].
fn slot_claimants(rows: &[Drawer]) -> Vec<Uuid> {
    rows.iter()
        .filter(|d| d.fact_key.as_deref() == Some(SLOT))
        .map(|d| d.id)
        .collect()
}

/// Seed a `kg.db` that `make_handle` will later open, run `mutate` against the
/// raw redb file once the store is closed, and hand back the seeded drawer id.
///
/// Why: the two fallback edge cases — a dangling index entry, and an index
/// entry naming a row that exists but cannot be hydrated — are states no public
/// API produces, because every write path maintains `DRAWERS` and
/// `DRAWERS_BY_FACT_KEY` together. Reaching them means writing the row through
/// the store, then editing `DRAWERS` behind the index's back.
/// What: opens `KgStoreRedb` on `<dir>/kg.redb` — the file `KnowledgeGraph`
/// derives from the `kg.db` path `make_handle` passes — upserts a drawer
/// claiming [`SLOT`] (which also writes its index entry), drops the store so
/// the file lock releases, then calls `mutate` with the drawer's id and an
/// exclusive `Database`. The index entry is never touched, so it keeps naming
/// the id.
/// Test: `a_fact_key_index_naming_no_row_still_admits_the_newcomer`,
/// `an_undecodable_incumbent_row_fails_the_write_instead_of_admitting_a_second_claimant`.
fn seed_slot_then_edit_row(
    dir: &std::path::Path,
    mutate: impl FnOnce(Uuid, &mut redb::Table<'_, &[u8], &[u8]>),
) -> Uuid {
    use crate::memory_core::store::kg_redb::KgStoreRedb;
    use crate::memory_core::store::kg_store::DRAWERS;
    use redb::Database;

    let kg_path = dir.join("kg.redb");
    let mut seeded = Drawer::new(Uuid::new_v4(), "the incumbent the index still names");
    seeded.fact_key = Some(SLOT.to_string());
    let seeded_id = seeded.id;
    {
        let store = KgStoreRedb::open(&kg_path).expect("open kg store to seed the slot");
        store
            .upsert_drawer(&seeded)
            .expect("seed the incumbent row and its index entry");
    }
    {
        let db = Database::create(&kg_path).expect("reopen kg.db exclusively");
        let wtx = db
            .begin_write()
            .expect("begin write to edit the drawer row");
        {
            let mut table = wtx.open_table(DRAWERS).expect("open drawers table");
            mutate(seeded_id, &mut table);
        }
        wtx.commit().expect("commit the row edit");
    }
    seeded_id
}

/// The incumbent is on disk but missing from the in-memory table: the newcomer
/// must retire it, not join it (#6438).
///
/// Why: `persist_with_retirement` used to read the in-memory drawer table as
/// the whole truth about who holds a slot. Two production paths leave that
/// table short of a row redb still holds — an abandoned commit whose mirror
/// never ran (#6366), and a degraded open that skipped rows (#6201). On both,
/// the fallback wrote the newcomer without retiring anyone, so two drawer rows
/// carried a live `SLOT`. That is the state the module's one-live-claimant
/// invariant forbids, and no later write repairs it: the index names only the
/// newcomer, so the stranded row is never picked as an incumbent again.
/// What: writes the incumbent straight through `kg.upsert_drawer` — durable,
/// indexed, and never mirrored into `handle.drawers` — then runs a newcomer
/// through the real `remember_with_options` pipeline and asserts on the DURABLE
/// rows. Pre-fix this fails at `slot_claimants`, reporting both ids.
/// Test: this test.
#[tokio::test]
async fn an_on_disk_incumbent_absent_from_the_mirror_is_still_retired() {
    init_embedder();
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let mut incumbent = Drawer::new(Uuid::new_v4(), "PR #4818 is in flight at head d3963848");
    incumbent.fact_key = Some(SLOT.to_string());
    incumbent.expires_at = Some(Utc::now() + Duration::hours(24));
    let incumbent_id = incumbent.id;
    handle
        .kg
        .upsert_drawer(&incumbent)
        .await
        .expect("seed the durable incumbent");

    assert!(
        handle.drawers.read().is_empty(),
        "anti-vacuous guard: the incumbent must be absent from the in-memory \
         table, otherwise this exercises the ordinary retirement path"
    );
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(incumbent_id),
        "precondition: the index names the durable incumbent"
    );

    let newcomer = write(
        &handle,
        "PR #4818 merged as squash 4c412ae1, superseding the earlier SHA",
        Some(SLOT),
        None,
    )
    .await
    .expect("the newcomer write must land");

    let rows = handle.kg.load_drawers().unwrap();
    assert_eq!(
        slot_claimants(&rows),
        vec![newcomer],
        "#6438: exactly one drawer may claim {SLOT} durably; rows: {:?}",
        rows.iter()
            .map(|d| (d.id, d.fact_key.clone()))
            .collect::<Vec<_>>()
    );
    let retired = rows
        .iter()
        .find(|d| d.id == incumbent_id)
        .expect("the incumbent row survives retirement (ADR-0028 D6)");
    assert_eq!(
        retired.expires_at, None,
        "a demoted Tier C drawer must not keep the TTL that would delete it"
    );
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(newcomer),
        "the index and the drawer rows must name the same claimant"
    );
}

/// A `fact_key` index entry naming no row at all still admits the newcomer.
///
/// Why: the durable point-read must distinguish "no incumbent exists" from
/// "the in-memory table is short a row". Only the first is a free slot. If the
/// fix treated a dangling index entry as a reason to fail or to stall, every
/// write to a slot whose row was lost would be permanently blocked — a worse
/// outcome than the two-claimant bug, since the newcomer's own write is what
/// re-points the index and repairs the dangle.
/// What: seeds a row claiming the slot, deletes ONLY the `DRAWERS` row so the
/// index entry keeps naming it, then writes a newcomer. The write must succeed
/// and leave one claimant.
/// Test: this test.
#[tokio::test]
async fn a_fact_key_index_naming_no_row_still_admits_the_newcomer() {
    init_embedder();
    let dir = tempdir().unwrap();
    let ghost_id = seed_slot_then_edit_row(dir.path(), |id, table| {
        table
            .remove(id.into_bytes().as_slice())
            .expect("remove the drawer row, leaving the index entry dangling");
    });
    let handle = make_handle(dir.path());
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(ghost_id),
        "precondition: the index still names the removed row"
    );

    let newcomer = write(
        &handle,
        "the newcomer claiming a dangling slot",
        Some(SLOT),
        None,
    )
    .await
    .expect("#6438: a dangling index entry must not block the write");

    let rows = handle.kg.load_drawers().unwrap();
    assert_eq!(
        slot_claimants(&rows),
        vec![newcomer],
        "the newcomer is the only claimant, and the dangle is repaired"
    );
    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(newcomer),
        "the newcomer's write re-points the index at itself"
    );
}

/// An incumbent row that exists but cannot be hydrated fails the write.
///
/// Why: this is the fail-open arm the fix must NOT take. A `DRAWERS` row the
/// decoder rejects is still a row holding the slot — it is exactly what a
/// degraded open (#6201) skips, which is how the incumbent went missing from
/// the in-memory table in the first place. Reading that error as absence would
/// reproduce the very bug: the newcomer lands, the corrupt row keeps its live
/// `fact_key`, and nothing can retire it afterwards because the index has moved
/// on. Refusing the write keeps the durable state to one claimant and surfaces
/// the corruption instead of burying it.
/// What: seeds a row claiming the slot, overwrites its value with bytes no
/// `DrawerRecord` shape decodes, then writes a newcomer. The write must fail,
/// and the index must still name the corrupt incumbent — no newcomer admitted.
/// Test: this test.
#[tokio::test]
async fn an_undecodable_incumbent_row_fails_the_write_instead_of_admitting_a_second_claimant() {
    init_embedder();
    let dir = tempdir().unwrap();
    let corrupt_id = seed_slot_then_edit_row(dir.path(), |id, table| {
        table
            .insert(id.into_bytes().as_slice(), [0xFFu8; 4].as_slice())
            .expect("overwrite the drawer value with undecodable bytes");
    });
    let handle = make_handle(dir.path());

    let err = write(
        &handle,
        "the newcomer that must be refused",
        Some(SLOT),
        None,
    )
    .await
    .expect_err("#6438: an unreadable incumbent must fail the write");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("durable point-read of the fact_key slot incumbent"),
        "the failure must name the point-read that refused, got: {rendered}"
    );

    assert_eq!(
        handle.kg.drawer_id_for_fact_key(SLOT).unwrap(),
        Some(corrupt_id),
        "the slot must still name the incumbent — no newcomer may have claimed it"
    );
    assert!(
        slot_claimants(&handle.kg.load_drawers().unwrap()).is_empty(),
        "no readable row may claim {SLOT}: the only claimant is the corrupt \
         row load_drawers skips"
    );
}
