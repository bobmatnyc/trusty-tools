//! Tests for #4906 — the deferred-embed retry lane, the durable failure
//! ledger, the vector-coverage health surface, and the repair backfill.
//!
//! Why: the defect these cover is silent. A drawer stored without a vector is
//! byte-identical, from the caller's side, to one stored with a vector — the
//! write returns `Ok`, the drawer is durable, and only a search that never
//! finds it reveals the difference. Every test here therefore asserts on state
//! that survives the process (the ledger file, the vector index), not on log
//! output.
//! What: fail-before / pass-after coverage for all four behaviours the fix
//! claims — transient failure retried, permanent failure marked, backfill
//! repairing a marked drawer, backfill being a no-op on a healthy palace.
//! Test: this file IS the tests. Run with
//!   `cargo test -p trusty-common --features memory-core,embedder-test-support embed_repair`

use super::deferred_embed::{
    EmbedLoss, RetryPolicy, embed_and_store, embed_store_or_record, record_loss,
};
use super::embed_repair::VectorBackfillOptions;
use super::embedder::seed_shared_embedder_with_mock;
use super::handle::PalaceHandle;
use crate::embedder::MockEmbedder;
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::store::embed_ledger::{self, EmbedFailure};
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use uuid::Uuid;

/// Embedder that fails its first `fail_times` calls, then behaves like the
/// deterministic mock.
///
/// Why: "transient" is the whole distinction the retry loop exists to make. A
/// double that only ever fails cannot prove a retry helped; this one fails and
/// then recovers, so the assertion is that the vector arrived, not that a
/// counter moved.
/// What: an `AtomicU32` call counter; calls at or below `fail_times` return an
/// error, later calls delegate to `MockEmbedder`.
/// Test: used by `retry_succeeds_after_transient_failures`.
struct FlakyEmbedder {
    fail_times: u32,
    calls: AtomicU32,
    inner: MockEmbedder,
}

impl FlakyEmbedder {
    fn new(fail_times: u32) -> Self {
        Self {
            fail_times,
            calls: AtomicU32::new(0),
            inner: MockEmbedder::new(384),
        }
    }
    fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for FlakyEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= self.fail_times {
            anyhow::bail!("simulated transient embedder failure (call {n})");
        }
        self.inner.embed_batch(texts).await
    }
    fn dimension(&self) -> usize {
        384
    }
}

/// Embedder that always fails — the permanent-failure case.
struct DeadEmbedder;

#[async_trait]
impl Embedder for DeadEmbedder {
    async fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!("simulated permanent embedder failure")
    }
    fn dimension(&self) -> usize {
        384
    }
}

/// A palace handle backed by a real data dir, so the ledger has somewhere to go.
fn make_handle(dir: &std::path::Path) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    let mut handle = PalaceHandle::new(PalaceId::new("embed-repair"), String::new(), vs, kg);
    handle.data_dir = Some(dir.to_path_buf());
    handle
}

/// Add a drawer to the in-memory table WITHOUT a vector — the exact state the
/// 39 live drawers are in.
fn add_vectorless_drawer(handle: &PalaceHandle, content: &str) -> Uuid {
    let drawer = Drawer::new(Uuid::new_v4(), content);
    let id = drawer.id;
    handle.add_drawer(drawer);
    id
}

// ── 1. Transient failure is retried ──────────────────────────────────────────

/// Why: before #4906 the deferred lane took the first `Err` from `embed_batch`
/// as final — one transient blip and the drawer was permanently unfindable.
/// This is the fail-before case: with a single-attempt policy (the old
/// behaviour) the vector never lands; with the shipped default it does.
/// What: an embedder that fails twice then succeeds, run under a 3-attempt
/// policy; asserts the vector reached the store and exactly 3 calls were made.
/// Test: itself.
#[tokio::test]
async fn retry_succeeds_after_transient_failures() {
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    let flaky = Arc::new(FlakyEmbedder::new(2));
    let embedder: Arc<dyn Embedder + Send + Sync> = flaky.clone();
    let id = Uuid::new_v4();

    // Fail-before: one attempt reproduces the old fire-and-forget behaviour.
    let single = embed_and_store(
        &embedder,
        &handle.vector_store,
        id,
        "content",
        &RetryPolicy::instant(1),
    )
    .await;
    assert!(
        single.is_err(),
        "a single attempt must surface the transient failure (this is the pre-fix behaviour)"
    );
    assert!(
        !handle.vector_store.all_ids().contains(&id),
        "no vector may exist after the failed single attempt"
    );

    // Pass-after: retrying rides out the same transient failures.
    let attempts = embed_and_store(
        &embedder,
        &handle.vector_store,
        id,
        "content",
        &RetryPolicy::instant(3),
    )
    .await
    .expect("retry must ride out two transient failures");
    assert_eq!(attempts, 2, "second call succeeds on its 2nd attempt");
    assert_eq!(
        flaky.call_count(),
        3,
        "1 failed single + 2 retried attempts"
    );
    assert!(
        handle.vector_store.all_ids().contains(&id),
        "the vector must be in the index after the retry succeeded"
    );
}

// ── 2. Permanent failure is marked, not dropped ──────────────────────────────

/// Why: this is the core of the defect. The old code's every failure path was
/// `warn!` + `return`, leaving nothing durable that said the drawer has no
/// vector. A marker that only exists in a log line is not a marker.
/// What: drives the deferred lane's outcome handler with a permanently broken
/// embedder and asserts the palace's ledger file — re-read from disk, not from
/// memory — carries a row for that drawer with the attempt count.
/// Test: itself.
#[tokio::test]
async fn permanent_failure_writes_a_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = add_vectorless_drawer(&handle, "a fact that will never embed");
    let embedder: Arc<dyn Embedder + Send + Sync> = Arc::new(DeadEmbedder);

    assert!(
        embed_ledger::load(dir.path()).is_empty(),
        "ledger starts empty"
    );

    embed_store_or_record(
        &handle.deferred_embed_ctx(),
        &embedder,
        id,
        "a fact that will never embed",
        &RetryPolicy::instant(2),
    )
    .await;

    let rows = embed_ledger::load(dir.path());
    assert_eq!(
        rows.len(),
        1,
        "the failure must be recorded durably: {rows:?}"
    );
    assert_eq!(rows[0].drawer_id, id);
    assert_eq!(rows[0].attempts, 2, "the attempt count must be recorded");
    assert!(
        rows[0].reason.contains("embed failed"),
        "the reason must name the failing stage: {}",
        rows[0].reason
    );
    assert!(
        !handle.vector_store.all_ids().contains(&id),
        "no vector was produced"
    );

    // The health surface reports it without re-deriving anything.
    let health = handle.embed_health();
    assert_eq!(health.missing_vector_ids, vec![id]);
    assert_eq!(health.recorded_failures.len(), 1);
    assert!(!health.is_healthy());
}

/// Why: an embedder that cannot initialise at all is an expected state — a host
/// with `--no-embedder`-style operation, a cold start, or no model downloaded.
/// Marking every drawer written on such a host as broken would be a false alarm
/// at estate scale, so the two failure classes must not share a code path.
/// What: records an `EmbedderUnavailable` loss and asserts the ledger stays
/// empty, while a drawer-specific loss for the same drawer does write a row.
/// Test: itself.
#[tokio::test]
async fn missing_embedder_does_not_mark_the_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = add_vectorless_drawer(&handle, "written on a host with no model");
    let ctx = handle.deferred_embed_ctx();

    record_loss(
        &ctx,
        id,
        &EmbedLoss::EmbedderUnavailable("model not downloaded".to_string()),
    );
    assert!(
        embed_ledger::load(dir.path()).is_empty(),
        "a host-level embedder outage must not mark individual drawers broken"
    );

    // The same drawer, failing with the embedder present, IS marked.
    record_loss(
        &ctx,
        id,
        &EmbedLoss::Embed {
            reason: "tensor shape mismatch".to_string(),
            attempts: 3,
        },
    );
    assert_eq!(
        embed_ledger::load(dir.path()).len(),
        1,
        "a drawer-specific failure must be recorded"
    );
}

/// Why: a drawer forgotten while its embed was in flight has nothing to
/// annotate; a ledger row for an id nothing can look up is noise an operator
/// would have to triage.
/// What: records a drawer-specific loss for an id that is not in the drawer
/// table and asserts nothing is written.
/// Test: itself.
#[tokio::test]
async fn loss_for_a_forgotten_drawer_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    record_loss(
        &handle.deferred_embed_ctx(),
        Uuid::new_v4(),
        &EmbedLoss::Embed {
            reason: "gone".to_string(),
            attempts: 1,
        },
    );
    assert!(embed_ledger::load(dir.path()).is_empty());
}

// ── 3. The backfill repairs what is already broken ───────────────────────────

/// Why: the deliverable that actually unblocks #4834. Fixing the write path
/// forward leaves the 39 already-vectorless drawers unfindable; only a repair
/// pass makes them retrievable, and it must clear the marker it repaired so the
/// ledger cannot outlive the condition.
/// What: seeds a vectorless drawer plus its ledger row, runs a live backfill,
/// and asserts the vector landed, the row is gone, and a semantic recall now
/// finds the drawer it previously could not.
/// Test: itself.
#[tokio::test]
async fn backfill_reembeds_a_marked_drawer() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = add_vectorless_drawer(&handle, "Rust ownership and borrowing rules");
    embed_ledger::record(
        dir.path(),
        EmbedFailure {
            drawer_id: id,
            failed_at: chrono::Utc::now(),
            attempts: 3,
            reason: "embed failed: simulated".to_string(),
        },
    )
    .unwrap();

    // Fail-before: the drawer is invisible to vector recall.
    let embedder = super::embedder::shared_embedder().await.unwrap();
    let before = super::layers::retrieve_l2(
        &handle,
        embedder.as_ref(),
        "Rust ownership and borrowing rules",
        None,
        5,
    )
    .await
    .unwrap();
    assert!(
        before.is_empty(),
        "a drawer with no vector must not be retrievable — that is the defect"
    );

    let report = handle
        .backfill_missing_vectors(VectorBackfillOptions {
            dry_run: false,
            limit: None,
            retry: RetryPolicy::instant(2),
        })
        .await
        .expect("backfill must run");

    assert_eq!(report.missing, 1);
    assert_eq!(report.attempted, 1);
    assert_eq!(report.repaired, 1);
    assert_eq!(report.still_failing, 0);
    assert!(report.still_missing_ids.is_empty());
    assert!(
        embed_ledger::load(dir.path()).is_empty(),
        "a repaired drawer must lose its ledger row"
    );

    // Pass-after: the same query now finds it.
    let after = super::layers::retrieve_l2(
        &handle,
        embedder.as_ref(),
        "Rust ownership and borrowing rules",
        None,
        5,
    )
    .await
    .unwrap();
    assert!(
        after.iter().any(|r| r.drawer.id == id),
        "the repaired drawer must now be retrievable"
    );

    // Idempotent: a second run finds nothing to do.
    let again = handle
        .backfill_missing_vectors(VectorBackfillOptions {
            dry_run: false,
            limit: None,
            retry: RetryPolicy::instant(2),
        })
        .await
        .unwrap();
    assert_eq!(again.missing, 0);
    assert_eq!(again.attempted, 0);
    assert_eq!(again.repaired, 0);
}

/// Why: a repair tool that does work on a healthy palace is a repair tool
/// nobody dares run on a schedule. It must also be safe on a host with no
/// embedder — which means it must not even try to resolve one when there is
/// nothing to repair.
/// What: writes a drawer through the normal synchronous path (so it has a
/// vector), then asserts the backfill reports zero missing and attempts nothing.
/// Test: itself.
#[tokio::test]
async fn backfill_is_a_noop_on_a_healthy_palace() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    handle
        .remember(
            "a fact that embeds normally".to_string(),
            crate::memory_core::palace::RoomType::Custom("t".into()),
            vec![],
            0.5,
        )
        .await
        .expect("remember");

    let health = handle.embed_health();
    assert!(health.is_healthy(), "palace should be healthy: {health:?}");

    let report = handle
        .backfill_missing_vectors(VectorBackfillOptions {
            dry_run: false,
            limit: None,
            retry: RetryPolicy::instant(1),
        })
        .await
        .expect("backfill on a healthy palace must succeed");
    assert_eq!(report.missing, 0);
    assert_eq!(report.attempted, 0);
    assert_eq!(report.repaired, 0);
}

/// Why: the operator's first action on a live palace is a measurement, not a
/// mutation — #4834 deletes source files on the strength of the number, so
/// seeing it before changing anything is the point.
/// What: asserts a dry run reports the gap and repairs nothing.
/// Test: itself.
#[tokio::test]
async fn backfill_dry_run_repairs_nothing() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    let id = add_vectorless_drawer(&handle, "unembedded");

    let report = handle
        .backfill_missing_vectors(VectorBackfillOptions::default())
        .await
        .unwrap();
    assert!(report.dry_run);
    assert_eq!(report.missing, 1);
    assert_eq!(report.attempted, 0);
    assert_eq!(report.repaired, 0);
    assert_eq!(report.still_missing_ids, vec![id]);
    assert!(!handle.vector_store.all_ids().contains(&id));
}

// ── 4. The health surface and the ledger ─────────────────────────────────────

/// Why: "which drawers have no vector" had no answer short of self-retrieving
/// every drawer and guessing from the ranking. This is the set difference that
/// replaces the guess.
/// What: one embedded drawer and one vectorless drawer; asserts only the
/// vectorless one is reported and the counts are the ones the doctor check
/// compares.
/// Test: itself.
#[tokio::test]
async fn health_reports_the_drawer_with_no_vector() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let handle = make_handle(dir.path());
    handle
        .remember(
            "an embedded fact about caching".to_string(),
            crate::memory_core::palace::RoomType::Custom("t".into()),
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let missing_id = add_vectorless_drawer(&handle, "a fact with no vector");

    let health = handle.embed_health();
    assert_eq!(health.drawer_count, 2);
    assert_eq!(health.vector_count, 1);
    assert_eq!(health.missing_vector_ids, vec![missing_id]);
    assert!(health.embedder_ready, "the mock embedder is seeded");
}

/// Why: the ledger is keyed by drawer id precisely so a repeated failure
/// refreshes the row instead of appending one — an append-only ledger for a
/// retrying background lane grows without bound.
/// What: records twice for the same drawer and once for another; asserts two
/// rows with the second attempt count winning.
/// Test: itself.
#[test]
fn embed_ledger_roundtrips_and_upserts_by_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let row = |id: Uuid, attempts: u32| EmbedFailure {
        drawer_id: id,
        failed_at: chrono::Utc::now(),
        attempts,
        reason: "x".to_string(),
    };
    embed_ledger::record(dir.path(), row(a, 1)).unwrap();
    embed_ledger::record(dir.path(), row(b, 1)).unwrap();
    embed_ledger::record(dir.path(), row(a, 7)).unwrap();

    let rows = embed_ledger::load(dir.path());
    assert_eq!(
        rows.len(),
        2,
        "the second write for `a` must replace the first"
    );
    let a_row = rows.iter().find(|r| r.drawer_id == a).unwrap();
    assert_eq!(a_row.attempts, 7);
}

/// Why: clearing must be surgical — a backfill that repaired one drawer must
/// not wipe the record of the ones it could not.
/// What: records two rows, clears one, asserts the other survives.
/// Test: itself.
#[test]
fn embed_ledger_clear_removes_only_named_rows() {
    let dir = tempfile::tempdir().unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    for id in [a, b] {
        embed_ledger::record(
            dir.path(),
            EmbedFailure {
                drawer_id: id,
                failed_at: chrono::Utc::now(),
                attempts: 1,
                reason: "x".to_string(),
            },
        )
        .unwrap();
    }
    embed_ledger::clear(dir.path(), &std::iter::once(a).collect()).unwrap();
    let rows = embed_ledger::load(dir.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].drawer_id, b);
}

/// Why: every palace that has never failed an embed has no ledger file, which
/// is the overwhelmingly common case and must not be an error.
/// What: asserts a missing file reads as an empty ledger.
/// Test: itself.
#[test]
fn embed_ledger_load_is_empty_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    assert!(embed_ledger::load(dir.path()).is_empty());
    assert!(embed_ledger::load(&dir.path().join("nope")).is_empty());
}
