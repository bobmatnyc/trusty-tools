//! `/health` counter-semantics tests for issue #5927 — corpus-open failure vs.
//! any-lane failure.
//!
//! Why: `indexes_corpus_failed` counted every handle where
//! `IndexStages::any_failed()` held, so a failed SEMANTIC lane on an index
//! whose corpus opened fine incremented a counter named for corpus-open
//! failures. Two separate investigations read the name literally, checked
//! `GET /indexes/:id/status`'s `corpus_open_failure` (correctly `null`
//! everywhere), found nothing, and wrote a live count of `1` off as a stale
//! boot snapshot. It was neither stale nor cosmetic — one index had
//! `stages.semantic = "failed"`. These tests pin the two counters to the two
//! distinct facts so the same misreading cannot recur.
//! What: exercises the three cohorts the split creates — a lane failure with a
//! healthy corpus, a corpus-open failure, and the fail-open arm where the
//! corpus flag cannot be read at all.
//! Test: these tests.
//!
//! This module is separate from `tests_health_degraded.rs` because that file's
//! basename is not classified as a test file by the line-cap gate and it sits
//! at 455 of its 500-SLOC production cap. The `_tests.rs` basename here earns
//! the 3000-SLOC test cap instead.
use super::health::HealthResponse;
use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageState};
use axum::extract::State;
use axum::Json;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Register one index with a healthy (never-failed) corpus into a fresh
/// registry and return the registry plus that handle.
fn registry_with_one_index(id: &str) -> (IndexRegistry, Arc<IndexHandle>) {
    let registry = IndexRegistry::new();
    let root = format!("/tmp/{id}");
    let handle = registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, &root))),
        root.into(),
    ));
    (registry, handle)
}

/// Serialize a `/health` response and hand back its `warmboot_summary` object.
fn warmboot_summary_of(resp: &HealthResponse) -> Value {
    let json: Value = serde_json::to_value(resp).expect("serialize /health response");
    json["warmboot_summary"].clone()
}

/// #5927: a failed SEMANTIC lane on an index whose corpus opened fine must not
/// be counted as a corpus-open failure.
///
/// Why: this is the exact production shape that misdirected two
/// investigations. `indexes_corpus_failed` read `1`, every per-index
/// `corpus_open_failure` read `null`, and the two facts were irreconcilable
/// because the counter measured `stages.any_failed()` rather than what its
/// name claims. Against the pre-fix implementation this test fails on the
/// first assertion (the counter reads `1`) and on the second (the field does
/// not exist).
/// What: registers one index, fails only its semantic lane, leaves
/// `CodeIndexer::corpus_open_failed` at its constructed `false`, and asserts
/// the corpus counter stays `0` while the new lane counter reports `1`. Also
/// asserts the degraded signal is unchanged — narrowing the corpus counter
/// must not quietly stop a lane failure from degrading the daemon.
/// Test: this IS the test.
#[tokio::test]
async fn health_does_not_count_a_semantic_lane_failure_as_a_corpus_failure() {
    let (registry, handle) = registry_with_one_index("semantic-failed-5927");
    {
        let mut stages = handle.stages.write().await;
        stages.semantic = StageState::failed("embed backend unreachable".to_string());
    }
    {
        let indexer = handle.indexer.read().await;
        assert!(
            !indexer.corpus_open_failed,
            "precondition: this index's corpus opened fine — only its semantic lane failed"
        );
    }

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    let summary = warmboot_summary_of(&resp);

    assert_eq!(
        summary["indexes_corpus_failed"].as_u64(),
        Some(0),
        "#5927: no corpus failed to open, so the counter NAMED for that fact must read 0; \
         summary={summary}"
    );
    assert_eq!(
        summary["indexes_stage_failed"].as_u64(),
        Some(1),
        "#5927: the failed semantic lane must still be counted — under its own name; \
         summary={summary}"
    );
    assert_eq!(
        summary["warm_boot_degraded"].as_bool(),
        Some(true),
        "#5927: splitting the counters must not weaken the degraded signal a lane \
         failure already produced"
    );
    assert_eq!(
        resp.status, "degraded",
        "#5927: a dead search lane still degrades the daemon that `status != \"ok\"` \
         monitors gate on"
    );
}

/// #5927: a genuine corpus-open failure must be counted by BOTH counters.
///
/// Why: the split must not create a blind spot in the other direction. A
/// corpus-open failure marks every lane `Failed`
/// (`derive_warm_boot_stages`' `corpus_open_failed` guard), so it belongs in
/// the lane counter too — `indexes_stage_failed` stays the strict superset an
/// operator can gate on without knowing which sub-fact applies.
/// What: sets `CodeIndexer::corpus_open_failed` (the same flag
/// `persistence_loader::build_indexer_from_entry` sets, and the same one
/// `GET /indexes/:id/status` reports as `corpus_open_failure`) alongside the
/// all-lanes-`Failed` stage state the classifier produces, then asserts both
/// counters read `1`.
/// Test: this IS the test.
#[tokio::test]
async fn health_counts_a_corpus_open_failure_under_both_names() {
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let (registry, handle) = registry_with_one_index("corpus-failed-5927");
    {
        // The quarantine invariant (`core/indexer/quarantine.rs`) is
        // `corpus_open_failed == true` implies `corpus == None`;
        // `CodeIndexer::new` wires no corpus, so setting the flag alone is a
        // faithful fixture.
        let mut indexer = handle.indexer.write().await;
        indexer.corpus_open_failed = true;
    }
    {
        let mut stages = handle.stages.write().await;
        *stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 47_946,
            hnsw_snapshot_ready: true,
            graph_node_count: 1_000,
            lexical_only: false,
            skip_kg: false,
            skip_vector: false,
            corpus_open_failure: Some(crate::core::corpus::CorpusOpenFailure::FormatIncompatible),
        });
    }

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    let summary = warmboot_summary_of(&resp);

    assert_eq!(
        summary["indexes_corpus_failed"].as_u64(),
        Some(1),
        "#5927: a real corpus-open failure is what this counter is for; summary={summary}"
    );
    assert_eq!(
        summary["indexes_stage_failed"].as_u64(),
        Some(1),
        "#5927: a corpus-open failure fails every lane, so the lane counter must \
         remain a superset of the corpus counter; summary={summary}"
    );
    assert_eq!(resp.status, "degraded");
}

/// #5927 fail-open arm: an unreadable `corpus_open_failed` flag must not clear
/// the degraded signal.
///
/// Why: the corpus counter now reads `handle.indexer`, a DIFFERENT lock from
/// `handle.stages`, and a contended `try_read` there is folded into "not
/// failed" for that poll — the same fail-open the rest of this scan uses. That
/// is only safe because the lane counter reads the other lock and a
/// corpus-open failure fails every lane, so `indexes_stage_failed` still fires
/// and still degrades the daemon. This test is what proves the undercount
/// cannot advance the daemon to `status: "ok"`.
/// What: holds the indexer's write lock across the poll so the corpus read
/// cannot happen, and asserts the corpus counter undercounts to `0` while the
/// lane counter and the top-level status both still report the failure.
/// Test: this IS the test.
#[tokio::test]
async fn health_stays_degraded_when_the_corpus_flag_read_is_contended() {
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let (registry, handle) = registry_with_one_index("contended-5927");
    {
        let mut indexer = handle.indexer.write().await;
        indexer.corpus_open_failed = true;
    }
    {
        let mut stages = handle.stages.write().await;
        *stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 10,
            hnsw_snapshot_ready: false,
            graph_node_count: 0,
            lexical_only: false,
            skip_kg: false,
            skip_vector: false,
            corpus_open_failure: Some(crate::core::corpus::CorpusOpenFailure::FormatIncompatible),
        });
    }

    let state = Arc::new(SearchAppState::new(registry));

    // Held across the whole poll: every `try_read` on this handle's indexer
    // fails, exactly as a concurrent ingest write would make it fail.
    let _writer = handle.indexer.write().await;

    let Json(resp) = health_handler(State(state)).await;
    let summary = warmboot_summary_of(&resp);

    assert_eq!(
        summary["indexes_corpus_failed"].as_u64(),
        Some(0),
        "#5927: an unreadable indexer lock undercounts the corpus counter for this \
         poll — documenting the fail-open, not endorsing it as the whole answer"
    );
    assert_eq!(
        summary["indexes_stage_failed"].as_u64(),
        Some(1),
        "#5927: the lane counter reads the OTHER lock, so the failure is still \
         reported even when the corpus flag cannot be read; summary={summary}"
    );
    assert_eq!(
        resp.status, "degraded",
        "#5927: the fail-open on the corpus read must never advance the daemon to \
         'ok' while a lane is dead"
    );
}
