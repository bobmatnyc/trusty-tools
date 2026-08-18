//! End-to-end proof that the BM25 backfill is lossless.
//!
//! Why: the whole reason backfill does not reuse `bm25_index_enqueue` is that
//! the live write path holds a 256-slot channel written with `try_send` and
//! drops on full. The largest palace on this host holds 1311 drawers — five
//! times that queue — so a backfill routed through it would land roughly a
//! fifth of the corpus and report nothing wrong. That claim is only worth
//! anything if it is measured, so this test pushes more documents than the
//! queue could hold and asserts the index's own `stats` reports every one.
//!
//! It also exercises the bounded residency model: the lane's cap must evict the
//! least-recently-used palace rather than accumulating one index per palace,
//! which is the failure mode #2845/#2846 record.
//!
//! What: #5329 reworked this file from the subprocess model and removed its
//! `#[ignore]`. The old version needed a built `trusty-bm25-daemon` binary, so
//! the lossless-backfill regression — the one that catches a feeder silently
//! landing a fifth of a corpus — ran only under `--include-ignored`. It now
//! runs in the default `cargo test -p trusty-memory`. Two of its four tests
//! changed subject with the architecture: `daemon_population_stays_within_the_
//! cap` and `a_reaped_palace_respawns_on_next_use` became
//! `palace_residency_stays_within_the_cap` and
//! `an_evicted_palace_reloads_on_next_use`, asserting the same bound over
//! resident indexes instead of over child processes.
//!
//! Test: this *is* the test file.

use trusty_memory::bm25_backfill::{backfill_palace, BackfillStatus, PalaceDocs};
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::BM25_INDEX_QUEUE_CAPACITY;

/// Why: this is the regression the separate feeder exists for. Against a
/// backfill built on `bm25_index_enqueue`, this assertion reads ~256 (the queue
/// capacity), not 1024 — the rest are dropped with a `warn!` and the palace
/// serves a quarter of its corpus while looking healthy.
/// What: backfills 4x the live write path's queue capacity, asserts the index's
/// own `stats` reports every document, that the report says `Completed`, and
/// that `fully_indexed()` agrees. Then re-runs to prove idempotence and the
/// already-indexed short circuit, and finally drifts the corpus to prove
/// coverage is decided by identity rather than by count.
/// Test: this test itself.
#[tokio::test(flavor = "current_thread")]
async fn backfill_indexes_every_drawer_without_drops() {
    // 4x the live write path's queue. Any drop-on-full feeder fails here.
    let doc_count = BM25_INDEX_QUEUE_CAPACITY * 4;
    let docs: Vec<(String, String)> = (0..doc_count)
        .map(|i| {
            (
                format!("drawer-{i}"),
                format!("token{i} shared corpus text"),
            )
        })
        .collect();

    let palace = "backfill";
    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::with_limits(tmp.path().to_path_buf(), 3, None);

    let docs_for_drift = docs.clone();
    let started = std::time::Instant::now();
    let report = backfill_palace(&lane, palace, PalaceDocs::from_pairs(docs.clone()), false).await;
    let elapsed = started.elapsed();

    assert_eq!(
        report.status,
        BackfillStatus::Completed,
        "backfill must complete: {report:?}"
    );
    assert_eq!(report.failed, 0, "no document may fail: {report:?}");
    assert_eq!(
        report.indexed, doc_count,
        "every document must land — a drop-on-full feeder lands ~{BM25_INDEX_QUEUE_CAPACITY}"
    );
    assert_eq!(
        report.final_doc_count,
        Some(doc_count),
        "the index's own count must match what we sent"
    );
    assert!(report.fully_indexed());
    eprintln!("backfilled {doc_count} docs in {elapsed:?}");

    // The corpus is genuinely searchable, not merely counted.
    let hits = lane
        .search(palace, "token7", 5)
        .await
        .expect("search must succeed");
    assert!(
        hits.iter().any(|h| h.doc_id == "drawer-7"),
        "a backfilled doc must be reachable by a lexical query; got {hits:?}"
    );

    // Idempotent: a second run sees full coverage and does no work.
    let again = backfill_palace(&lane, palace, PalaceDocs::from_pairs(docs), false).await;
    assert_eq!(again.status, BackfillStatus::AlreadyIndexed);
    assert_eq!(again.indexed, 0, "a covered palace must not be re-fed");
    assert_eq!(
        again.missing_after,
        Some(0),
        "the skip must be a VERIFIED set"
    );
    assert!(again.fully_indexed());

    // The identity guarantee: a corpus inflated with documents the palace does
    // not have must NOT read as coverage. Against the old count-based
    // predicate, deleting one live doc and adding two stale ones leaves
    // `doc_count` above the drawer count and `AlreadyIndexed` claims full
    // coverage over a palace missing a drawer.
    lane.delete(palace, "drawer-7")
        .await
        .expect("delete a live doc");
    for i in 0..2 {
        lane.index(
            palace,
            &format!("stale-{i}"),
            "a drawer the palace no longer has",
        )
        .await
        .expect("seed a stale doc");
    }
    let drifted =
        backfill_palace(&lane, palace, PalaceDocs::from_pairs(docs_for_drift), false).await;
    assert!(
        drifted.final_doc_count.unwrap() > drifted.drawers_total,
        "precondition: the corpus is larger than the palace, so every count \
         comparison reports coverage"
    );
    assert_eq!(
        drifted.indexed, doc_count,
        "the missing drawer must have triggered a real run, not the skip"
    );
    assert!(drifted.fully_indexed(), "and the run must have repaired it");

    lane.shutdown().await;
}

/// Why: an index that holds only part of a palace must be detectable and
/// repairable — that is the "index partial" arm of the fail-open requirement.
/// What: indexes half the corpus directly, then backfills the whole thing and
/// asserts the run detected the shortfall (did NOT short-circuit) and that the
/// read-back count covers the full corpus afterwards.
/// Test: this test itself.
#[tokio::test(flavor = "current_thread")]
async fn partial_index_is_detected_and_repaired() {
    let total = 40usize;
    let docs: Vec<(String, String)> = (0..total)
        .map(|i| (format!("d{i}"), format!("word{i} common")))
        .collect();

    let palace = "partial";
    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::with_limits(tmp.path().to_path_buf(), 3, None);

    // Seed half the corpus, standing in for a palace whose live write path
    // dropped the rest under queue pressure.
    for (doc_id, text) in docs.iter().take(total / 2) {
        lane.index(palace, doc_id, text).await.expect("seed index");
    }
    let mid = lane.stats(palace).await.expect("stats");
    assert_eq!(mid.doc_count, total / 2, "seed must be half the corpus");

    let report = backfill_palace(&lane, palace, PalaceDocs::from_pairs(docs), false).await;
    assert_eq!(
        report.status,
        BackfillStatus::Completed,
        "a shortfall must trigger a real run, not the already-indexed skip: {report:?}"
    );
    assert_eq!(report.indexed, total);
    assert_eq!(report.missing_after, Some(0));
    assert_eq!(report.final_doc_count, Some(total));
    assert!(report.fully_indexed());

    lane.shutdown().await;
}

/// Why (#2845/#2846): before the cap, touching a palace only ever grew the map
/// — one `memory_recall_all` across ~99 palaces left 99 resident corpora for
/// the daemon's lifetime. Against uncapped code this assertion reads 5, not 2.
/// The subject changed from child processes to resident indexes with #5329; the
/// bound it asserts did not.
/// What: drives five palaces through a lane capped at 2, asserting the resident
/// population never exceeded the cap and that the eviction counter moved.
/// Test: this test itself.
#[tokio::test(flavor = "current_thread")]
async fn palace_residency_stays_within_the_cap() {
    let cap = 2usize;
    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::with_limits(tmp.path().to_path_buf(), cap, None);

    for i in 0..5 {
        let palace = format!("p{i}");
        lane.index(&palace, "d1", "alpha beta gamma")
            .await
            .unwrap_or_else(|e| panic!("index {palace}: {e:#}"));
        assert!(
            lane.resident_count().await <= cap,
            "residency must never exceed the cap of {cap}"
        );
    }

    assert_eq!(
        lane.resident_count().await,
        cap,
        "steady state must sit at the cap, not at the number of palaces touched"
    );
    assert_eq!(
        lane.evicted_count(),
        3,
        "five palaces through a cap of two must evict three indexes"
    );

    lane.shutdown().await;
}

/// Why: an evicted palace must be transparently reloadable — otherwise the cap
/// trades unbounded memory for a permanently degraded palace, which is a worse
/// bug than the one it fixes. This is the in-process form of
/// `a_reaped_palace_respawns_on_next_use`, and it is strictly stronger: the
/// daemon-era version could only observe that a respawned child reloaded its
/// snapshot, while this also proves the evicting side FLUSHED it first.
/// What: capped at 1, indexes palace A, indexes palace B (evicting A), then
/// reads A again and asserts its document is intact.
/// Test: this test itself.
#[tokio::test(flavor = "current_thread")]
async fn an_evicted_palace_reloads_on_next_use() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::with_limits(tmp.path().to_path_buf(), 1, None);

    lane.index("a", "d1", "alpha beta").await.expect("index a");
    lane.index("b", "d2", "gamma delta").await.expect("index b");
    assert_eq!(lane.resident_count().await, 1, "cap of 1 must hold");
    assert_eq!(lane.evicted_count(), 1);

    let stats = lane.stats("a").await.expect("stats from reloaded a");
    assert_eq!(
        stats.doc_count, 1,
        "a reloaded palace must read back its snapshot, not start empty"
    );
    let hits = lane.search("a", "alpha", 5).await.expect("search a");
    assert_eq!(
        hits.len(),
        1,
        "the evicted write must be searchable: {hits:?}"
    );
    assert_eq!(hits[0].doc_id, "d1");

    lane.shutdown().await;
}
