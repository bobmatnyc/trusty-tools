//! #9275 PR C: the expected-chunk-count health check and its repair.
//!
//! Why: a drawer with a vector row but fewer chunk rows than its text needs is
//! unfindable by its tail, and key presence cannot see it.
//! What: disk palaces with rows written directly; health and backfill driven
//! with an explicit chunking embedder (window 12: ten words per chunk).
//! Test: this file IS the tests.

use super::chunk_health::ChunkCheck;
use super::deferred_embed::{RetryPolicy, embed_and_store};
use super::embed_repair::VectorBackfillOptions;
use super::embed_store_tests::{Counting, DIM, add, chunks_at, handle, shape, words};
use crate::engine::embed::Embedder;
use crate::engine::store::embed_ledger;
use std::sync::Arc;
use uuid::Uuid;

const WINDOW: usize = 12;

/// A drawer of `n` words whose only row is the bare key (what 0.33.x wrote).
async fn legacy_drawer(h: &super::handle::PalaceHandle, n: usize) -> Uuid {
    let id = add(h, &words(n));
    h.vector_store
        .upsert_chunks(id, vec![vec![1.0; DIM]])
        .await
        .unwrap();
    id
}

/// Write the raw rows `indices` (0 = bare key) for drawer `id`.
fn raw_rows(h: &super::handle::PalaceHandle, id: Uuid, indices: &[u32]) {
    for n in indices {
        let key = if *n == 0 {
            id.to_string()
        } else {
            format!("{id}#{n}")
        };
        h.vector_store
            .hnsw_for_test()
            .upsert(&key, &[1.0; DIM])
            .unwrap();
    }
}

fn chunking() -> Arc<dyn Embedder + Send + Sync> {
    Arc::new(Counting::chunking(WINDOW))
}

#[tokio::test]
async fn health_flags_a_drawer_with_fewer_chunk_rows_than_expected() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let long = legacy_drawer(&h, 100).await;
    let e = chunking();
    let health = h.embed_health_with(Some(e.as_ref()));
    assert_eq!(health.chunk_check, ChunkCheck::Ran);
    assert_eq!(health.short_chunk_ids, vec![long]);
    assert!(health.missing_vector_ids.is_empty());
    assert!(!health.is_healthy());
}

#[tokio::test]
async fn health_flags_a_gapped_group() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let text = words(37);
    assert_eq!(
        chunks_at(WINDOW, &text),
        4,
        "fixture: 37 words make 4 chunks"
    );
    let id = add(&h, &text);
    // Four rows, as many as expected, but chunk 3 is missing.
    raw_rows(&h, id, &[0, 1, 2, 4]);
    let e = chunking();
    let health = h.embed_health_with(Some(e.as_ref()));
    assert_eq!(health.short_chunk_ids, vec![id], "a gap is not complete");
}

#[tokio::test]
async fn health_passes_a_complete_group_and_a_short_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let e = chunking();
    let policy = RetryPolicy::instant(1);
    for text in [words(100), words(5)] {
        let id = add(&h, &text);
        embed_and_store(&e, &h.vector_store, id, &text, &policy)
            .await
            .unwrap();
    }
    let health = h.embed_health_with(Some(e.as_ref()));
    assert_eq!(health.chunk_check, ChunkCheck::Ran);
    assert!(
        health.short_chunk_ids.is_empty(),
        "{:?}",
        health.short_chunk_ids
    );
    assert!(health.is_healthy(), "{health:?}");
}

#[tokio::test]
async fn health_reports_chunk_check_unavailable_when_no_embedder_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    // No drawer with a vector: nothing to compare, so the check ran.
    assert_eq!(h.embed_health_with(None).chunk_check, ChunkCheck::Ran);

    legacy_drawer(&h, 100).await;
    let health = h.embed_health_with(None);
    assert!(
        matches!(health.chunk_check, ChunkCheck::Unavailable(_)),
        "{:?}",
        health.chunk_check
    );
    assert!(health.short_chunk_ids.is_empty());
    assert!(!health.is_healthy(), "an unrun check is never healthy");
}

#[tokio::test]
async fn backfill_dry_run_counts_short_drawers_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let long = legacy_drawer(&h, 100).await;
    let opts = VectorBackfillOptions {
        dry_run: true,
        ..VectorBackfillOptions::default()
    };
    let report = h.backfill_with(opts, &chunking()).await.unwrap();
    assert_eq!((report.short, report.attempted), (1, 0));
    assert_eq!(report.still_short_ids, vec![long]);
    assert_eq!(report.chunk_check, ChunkCheck::Ran);
    assert_eq!(shape(&h, long), Some((1, 0)), "a dry run writes nothing");
}

#[tokio::test]
async fn backfill_limit_applies_to_missing_then_short() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let missing = [add(&h, &words(5)), add(&h, &words(6))];
    let short = [legacy_drawer(&h, 100).await, legacy_drawer(&h, 101).await];
    let opts = VectorBackfillOptions {
        dry_run: false,
        limit: Some(3),
        retry: RetryPolicy::instant(1),
    };
    let report = h.backfill_with(opts, &chunking()).await.unwrap();
    assert_eq!((report.missing, report.short), (2, 2));
    assert_eq!((report.attempted, report.repaired), (3, 3));
    assert!(report.still_missing_ids.is_empty(), "missing go first");
    assert_eq!(report.still_short_ids.len(), 1, "the limit stops one short");
    for id in missing {
        assert_eq!(shape(&h, id), Some((1, 0)));
    }
    let k = chunks_at(WINDOW, &words(100));
    let repaired = short
        .iter()
        .filter(|id| shape(&h, **id) != Some((1, 0)))
        .count();
    assert_eq!(repaired, 1);
    assert_eq!(shape(&h, short[0]), Some((k, (k - 1) as u32)));
}

#[tokio::test]
async fn backfill_failure_on_a_short_drawer_keeps_it_listed_and_records_the_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let long = legacy_drawer(&h, 100).await;
    let mut failing = Counting::chunking(WINDOW);
    failing.fail = true;
    let failing: Arc<dyn Embedder + Send + Sync> = Arc::new(failing);
    let opts = VectorBackfillOptions {
        dry_run: false,
        limit: None,
        retry: RetryPolicy::instant(1),
    };
    let report = h.backfill_with(opts, &failing).await.unwrap();
    assert_eq!((report.attempted, report.still_failing), (1, 1));
    assert_eq!(report.still_short_ids, vec![long]);
    assert_eq!(shape(&h, long), Some((1, 0)), "the old row stays");
    let ledger = embed_ledger::load(dir.path());
    assert!(ledger.iter().any(|f| f.drawer_id == long), "{ledger:?}");
}
