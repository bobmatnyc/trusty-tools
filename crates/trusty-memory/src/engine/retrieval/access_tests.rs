//! #9881: the recall access buffer — bump, debounce, flush, drop, failure.
//!
//! Why: the bump must reach redb without a commit per hit, must never write
//! through a read-only handle, and must never fail the recall that caused it.
//! What: disk-backed handles over a tempdir; the persisted count is read with
//! `KnowledgeGraph::load_drawers`, straight from redb.
//! Test: this IS the test module.

use super::super::handle::PalaceHandle;
use crate::engine::palace::{Drawer, PalaceId};
use crate::engine::store::{kg::KnowledgeGraph, vector::UsearchStore};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

const DIM: usize = 8;

/// A disk-backed handle over `dir` holding one persisted drawer.
async fn handle_with_drawer(dir: &Path) -> (PalaceHandle, Uuid) {
    let vs = UsearchStore::new(dir.join("idx.usearch"), DIM).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).expect("kg");
    let h = PalaceHandle::new(PalaceId::new("access"), String::new(), vs, kg);
    let d = Drawer::new(Uuid::new_v4(), "a drawer recall will serve");
    let id = d.id;
    h.kg.upsert_drawer(&d).await.expect("persist drawer");
    h.add_drawer(d);
    (h, id)
}

/// `(access_count, last_accessed_at.is_some())` of `id` in memory.
fn in_memory(h: &PalaceHandle, id: Uuid) -> (u32, bool) {
    let drawers = h.drawers.read();
    let d = drawers
        .iter()
        .find(|d| d.id == id)
        .expect("drawer in memory");
    (d.access_count, d.last_accessed_at.is_some())
}

/// `(access_count, last_accessed_at.is_some())` of `id` as redb holds it.
fn on_disk(kg: &KnowledgeGraph, id: Uuid) -> (u32, bool) {
    let rows = kg.load_drawers().expect("load drawers");
    let d = rows.iter().find(|d| d.id == id).expect("drawer on disk");
    (d.access_count, d.last_accessed_at.is_some())
}

/// Why: the in-memory bump is what rank and prune read; the flush makes it
/// survive a restart. An unknown id (a drawer forgotten meanwhile) is skipped.
/// What: records the drawer and an unknown id; memory shows 1 at once, disk
/// shows 0 until the flush writes one row, then 1.
/// Test: this function.
#[tokio::test]
async fn record_accesses_bumps_in_memory_and_persists_on_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    h.record_accesses(&[id, Uuid::new_v4()]);
    assert_eq!(in_memory(&h, id), (1, true));
    assert_eq!(h.accesses.pending_len(), 1, "an unknown id is not queued");
    assert_eq!(on_disk(&h.kg, id), (0, false), "no write on the read path");
    assert_eq!(h.flush_pending_accesses().await.expect("flush"), 1);
    assert_eq!(on_disk(&h.kg, id), (1, true));
    assert_eq!(h.flush_pending_accesses().await.expect("flush"), 0);
}

/// Why (Q5): with no explicit flush, the debounce alone must persist.
/// What: a 20 ms debounce, two records inside it, then a bounded wait for
/// the background write. Both accesses land; nothing stays pending.
/// Test: this function.
#[tokio::test]
async fn the_debounced_flush_persists_without_an_explicit_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    h.accesses.set_debounce(Duration::from_millis(20));
    h.record_accesses(&[id]);
    h.record_accesses(&[id]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while on_disk(&h.kg, id).0 < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(on_disk(&h.kg, id), (2, true));
    assert_eq!(h.accesses.pending_len(), 0);
}

/// Why (Q5): a handle released before its debounce fires must not lose the
/// accesses it buffered.
/// What: records, drops the handle without a flush, reopens the KG.
/// Test: this function.
#[tokio::test]
async fn dropping_the_handle_persists_pending_accesses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    h.record_accesses(&[id]);
    drop(h);
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("reopen kg");
    assert_eq!(on_disk(&kg, id), (1, true));
}

/// Why: a drop while a writer holds the palace must not overwrite that
/// writer's commit with an older in-memory row.
/// What: holds `write_mutex` across the drop; nothing is written.
/// Test: this function.
#[tokio::test]
async fn a_drop_while_a_writer_holds_the_palace_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    h.record_accesses(&[id]);
    let lock = h.write_mutex_for_test();
    let guard = lock.lock().await;
    drop(h);
    drop(guard);
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("reopen kg");
    assert_eq!(on_disk(&kg, id), (0, false));
}

/// Why: a failed write must not lose the accesses or fail the caller.
/// What: the seam fails the next write. `record_accesses` returns nothing, so
/// it cannot fail; the flush reports the error, the id stays pending, and the
/// next flush writes it.
/// Test: this function.
#[tokio::test]
async fn a_flush_failure_requeues_and_never_fails_the_caller() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    h.accesses.fail_next_write();
    h.record_accesses(&[id]);
    assert_eq!(in_memory(&h, id), (1, true), "the bump itself never fails");
    let err = h.flush_pending_accesses().await.expect_err("seam fails");
    assert!(err.to_string().contains("access write failed"), "{err:#}");
    assert_eq!(h.accesses.pending_len(), 1, "requeued");
    assert_eq!(on_disk(&h.kg, id), (0, false));
    assert_eq!(h.flush_pending_accesses().await.expect("retry"), 1);
    assert_eq!(on_disk(&h.kg, id), (1, true));
}

/// Why (Q6): a snapshot handle must never write, nor drift its counts.
/// What: holds the vector redb so the handle opens read-only, records, flushes
/// and drops; memory, the pending set and disk all stay at 0.
/// Test: this function.
#[tokio::test]
async fn a_read_only_handle_records_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    drop(h);
    let _live = redb::Database::create(dir.path().join("idx.usearch.redb")).expect("hold lock");
    let vs = UsearchStore::new(dir.path().join("idx.usearch"), DIM).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("kg");
    let h = PalaceHandle::new(PalaceId::new("access"), String::new(), vs, kg);
    assert!(h.is_read_only(), "precondition: a snapshot handle");
    h.add_drawer(h.kg.load_drawers().expect("load").remove(0));
    h.record_accesses(&[id]);
    assert_eq!(in_memory(&h, id), (0, false));
    assert_eq!(h.accesses.pending_len(), 0);
    assert_eq!(h.flush_pending_accesses().await.expect("flush"), 0);
    drop(h);
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("reopen kg");
    assert_eq!(on_disk(&kg, id), (0, false));
}

/// A dream config with no embedding-heavy or LLM pass.
fn quiet_dream() -> crate::engine::dream::DreamConfig {
    crate::engine::dream::DreamConfig {
        semantic: crate::engine::semantic_consolidation::SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        recall_benchmark_enabled: false,
        compact: false,
        ..Default::default()
    }
}

/// Why (Q5): prune and fading read the counts a dream cycle sees; the cycle
/// writes the pending accesses before any pass runs.
/// What: records, runs one quiet cycle, and finds the access on disk.
/// Test: this function.
#[tokio::test]
async fn a_dream_cycle_writes_pending_accesses_first() {
    crate::engine::retrieval::seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    let h = std::sync::Arc::new(h);
    h.record_accesses(&[id]);
    let dreamer = crate::engine::dream::Dreamer::new(quiet_dream());
    dreamer.dream_cycle(&h).await.expect("dream cycle");
    assert_eq!(on_disk(&h.kg, id), (1, true));
}

/// Why: the pre-dream flush is advisory; its failure must not fail the cycle.
/// What: the seam fails the write; the cycle still returns `Ok` and the
/// access stays pending for the next flush.
/// Test: this function.
#[tokio::test]
async fn a_failed_pre_dream_flush_does_not_fail_the_cycle() {
    crate::engine::retrieval::seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let (h, id) = handle_with_drawer(dir.path()).await;
    let h = std::sync::Arc::new(h);
    h.accesses.fail_next_write();
    h.record_accesses(&[id]);
    let dreamer = crate::engine::dream::Dreamer::new(quiet_dream());
    dreamer.dream_cycle(&h).await.expect("the cycle survives");
    assert_eq!(h.accesses.pending_len(), 1);
    assert_eq!(on_disk(&h.kg, id), (0, false));
}
