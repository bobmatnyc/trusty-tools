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

/// #6688: `/health` must name the indexes whose lanes failed, not only count
/// them.
///
/// Why: `indexes_stage_failed: 1` on a 41-index daemon told a consumer nothing
/// about WHICH index was broken, so per-index remediation had to poll
/// `GET /indexes/:id/status` for every registration to find it. Against the
/// pre-#6688 implementation this test fails on the first assertion — the key
/// does not exist.
/// What: registers three indexes, fails a lane on two of them (a semantic-lane
/// failure and a corpus-open failure, the two shapes #5927 separated), and
/// asserts the id array names exactly those two, sorted, with a length equal to
/// the counter it is derived from.
/// Test: this IS the test.
#[tokio::test]
async fn health_names_the_indexes_with_a_failed_lane() {
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

    let (registry, semantic_failed) = registry_with_one_index("zeta-semantic-6688");
    {
        let mut stages = semantic_failed.stages.write().await;
        stages.semantic = StageState::failed("embed backend unreachable".to_string());
    }

    let corpus_failed = registry.register(IndexHandle::bare(
        IndexId::new("alpha-corpus-6688"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "alpha-corpus-6688",
            "/tmp/alpha-corpus-6688",
        ))),
        "/tmp/alpha-corpus-6688".into(),
    ));
    {
        let mut indexer = corpus_failed.indexer.write().await;
        indexer.corpus_open_failed = true;
    }
    {
        let mut stages = corpus_failed.stages.write().await;
        *stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 1_000,
            hnsw_snapshot_ready: true,
            graph_node_count: 10,
            lexical_only: false,
            skip_kg: false,
            skip_vector: false,
            corpus_open_failure: Some(crate::core::corpus::CorpusOpenFailure::FormatIncompatible),
        });
    }

    // A third, entirely healthy index that must NOT appear in the list.
    registry.register(IndexHandle::bare(
        IndexId::new("mid-healthy-6688"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "mid-healthy-6688",
            "/tmp/mid-healthy-6688",
        ))),
        "/tmp/mid-healthy-6688".into(),
    ));

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    let json: Value = serde_json::to_value(&resp).expect("serialize /health response");

    let ids = json["indexes_stage_failed_ids"]
        .as_array()
        .unwrap_or_else(|| panic!("#6688: /health must carry the failing index ids; json={json}"));
    let ids: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        ids,
        vec!["alpha-corpus-6688", "zeta-semantic-6688"],
        "#6688: exactly the two indexes with a failed lane, sorted — the healthy one must \
         not appear; json={json}"
    );
    assert_eq!(
        ids.len() as u64,
        json["warmboot_summary"]["indexes_stage_failed"]
            .as_u64()
            .unwrap_or_default(),
        "#6688: the ids and the counter come from one predicate in one scan, so their \
         cardinalities must agree; json={json}"
    );
}

/// #6688: the id array must be ABSENT from the serialized JSON when no index is
/// failing — not `null`, not `[]`.
///
/// Why: that absence is the entire back-compat contract
/// `skip_serializing_if = "Option::is_none"` buys. A consumer built against an
/// older daemon must see a payload it already parses, and the healthy-path
/// payload is what it sees almost always. Asserting on the STRUCT would prove
/// nothing here — only the serialized output shows whether the key is emitted.
/// What: polls `/health` against a registry whose one index has no failed lane
/// and asserts the key is missing from the JSON object.
/// Test: this IS the test.
#[tokio::test]
async fn health_omits_the_failing_index_ids_when_nothing_is_failing() {
    let (registry, _handle) = registry_with_one_index("healthy-6688");

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    let json: Value = serde_json::to_value(&resp).expect("serialize /health response");

    assert_eq!(
        json["warmboot_summary"]["indexes_stage_failed"].as_u64(),
        Some(0),
        "precondition: nothing is failing on this daemon; json={json}"
    );
    assert!(
        json.get("indexes_stage_failed_ids").is_none(),
        "#6688: the key must be ABSENT (not null, not []) when no index is failing — that \
         is what keeps an older consumer's parse working; json={json}"
    );
}

/// #6688: adding the id array must not move, rename, retype, or drop any
/// counter the response already carried.
///
/// Why: the point of an additive optional field is that consumers can lag. Nine
/// consumers read this payload today and none of them must change in this PR;
/// `warm_boot_summary_wire_shape_is_pinned`
/// (`crates/trusty-review/src/integrations/health.rs`) pins the nested
/// `warmboot_summary` object from the other side of the workspace. This test
/// pins the TOP-LEVEL keys from inside trusty-search, which that test does not
/// reach.
/// What: polls `/health` on a healthy daemon and asserts every pre-#6688
/// top-level key is still present with its original JSON type, that the
/// previously-absent optional keys are still absent, and that the response
/// carries no key this pin does not name.
/// Test: this IS the test.
#[tokio::test]
async fn health_counter_wire_shape_is_unchanged_by_the_id_field() {
    let (registry, _handle) = registry_with_one_index("wire-shape-6688");

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    let json: Value = serde_json::to_value(&resp).expect("serialize /health response");
    let obj = json
        .as_object()
        .unwrap_or_else(|| panic!("#6688: /health must serialize to an object; json={json}"));

    // Every key a pre-#6688 daemon emitted on this path (healthy index, no
    // embedder installed, no boot reconcile), with the JSON type each one had.
    let expected: &[(&str, &str)] = &[
        ("status", "string"),
        ("version", "string"),
        ("indexes", "number"),
        ("uptime_secs", "number"),
        ("embedder", "string"),
        ("embedder_recent_timeout_count", "number"),
        ("rss_mb", "number"),
        ("rss_limit_mb", "number"),
        ("disk_bytes", "number"),
        ("cpu_pct", "number"),
        ("background_reindex_queue_depth", "number"),
        ("deferred_embed_queue_depth", "number"),
        ("warmboot_summary", "object"),
        ("indexes_kg_disabled", "number"),
        ("indexes_vector_disabled", "number"),
        ("indexes_component_catch_up_in_progress", "number"),
        ("indexes_embed_pool_missing", "number"),
        ("indexes_stuck_empty", "number"),
        ("indexes_stuck_mid_walk", "number"),
        ("indexes_populated", "number"),
        ("indexes_empty", "number"),
        ("total_chunks", "number"),
        ("indexes_watcher_network_degraded", "number"),
        ("embedder_bootstrap", "string"),
    ];
    for (key, kind) in expected {
        let value = obj
            .get(*key)
            .unwrap_or_else(|| panic!("#6688: pre-existing key `{key}` was dropped; json={json}"));
        let actual = match value {
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Object(_) => "object",
            Value::Array(_) => "array",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        };
        assert_eq!(
            actual, *kind,
            "#6688: pre-existing key `{key}` changed JSON type; json={json}"
        );
    }
    for key in [
        "embedder_error",
        "embedder_last_ok_secs_ago",
        "embedder_info",
        "embedderd_rss_mb",
        "update_available",
        "boot_reconcile",
        // The new field, absent on this healthy path for the same reason.
        "indexes_stage_failed_ids",
    ] {
        assert!(
            obj.get(key).is_none(),
            "#6688: `{key}` must stay absent on a healthy daemon; json={json}"
        );
    }
    assert_eq!(
        obj.len(),
        expected.len(),
        "#6688: /health emitted a key this pin does not name — an unreviewed wire change; \
         json={json}"
    );

    // The nested counters trusty-review pins from the other side.
    let summary = warmboot_summary_of(&resp);
    for key in [
        "indexes_stage_failed",
        "indexes_corpus_failed",
        "indexes_health_scan_skipped",
    ] {
        assert!(
            summary[key].is_number(),
            "#6688: `warmboot_summary.{key}` must remain a number; summary={summary}"
        );
    }
    assert_eq!(
        summary["warm_boot_degraded"].as_bool(),
        Some(false),
        "#6688: adding a field must not change the degraded verdict; summary={summary}"
    );
}
