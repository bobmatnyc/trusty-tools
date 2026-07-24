//! Health-honesty regression tests for issue #1870.
//!
//! Why: these tests landed in their own module (rather than joining
//! `tests_health.rs`) to keep that file under the 500-SLOC production cap —
//! it is not classified as a test file by the line-cap gate because its
//! basename does not end in `_test.rs`/`_tests.rs`, so adding these cases
//! there would have pushed it over the limit. Nothing was moved out of
//! `tests_health.rs`; this module is new.
//! What: covers the `/health` degraded-status path when a registered index's
//! durable corpus failed to open on warm-boot, plus the healthy-path guard.
//! Test: these tests.
use super::*;
use axum::extract::State;
use axum::Json;

/// Why (issue #1870): the daemon used to report `status: "ok"` even when a
/// registered index's durable redb corpus failed to open on warm-boot — a
/// silent, total search outage (every free-text query returns `[]`) that the
/// daemon self-certified as healthy. `/health` must instead report `degraded`
/// and surface a machine-readable count so downstream consumers that gate on
/// `/health` can detect the degradation.
/// What: registers an index and drives its stages to the exact state the
/// warm-boot classifier produces on a corpus-open failure
/// (`derive_warm_boot_stages` with `corpus_open_failed: true` → all lanes
/// `Failed`), then asserts `/health` downgrades `status` to `"degraded"`, sets
/// `warm_boot_degraded = true`, and reports `indexes_corpus_failed == 1`.
/// Test: this test.
#[tokio::test]
async fn health_reports_degraded_when_corpus_open_failed() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();

    // A healthy index (all lanes non-failed) so we prove the count is scoped
    // to the broken one, not "any registered index".
    registry.register(IndexHandle::bare(
        IndexId::new("healthy"),
        Arc::new(RwLock::new(CodeIndexer::new("healthy", "/tmp/healthy"))),
        "/tmp/healthy".into(),
    ));

    // The #1870 index: registered, but its redb corpus failed to open on
    // warm-boot, so the classifier marks every lane Failed.
    let broken = registry.register(IndexHandle::bare(
        IndexId::new("apex-green"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "apex-green",
            "/tmp/apex-green",
        ))),
        "/tmp/apex-green".into(),
    ));
    {
        let mut stages = broken.stages.write().await;
        *stages = derive_warm_boot_stages(WarmBootInputs {
            chunk_count: 47_946,
            hnsw_snapshot_ready: true,
            graph_node_count: 1_000,
            lexical_only: false,
            skip_kg: false,
            skip_vector: false,
            corpus_open_failed: true,
        });
        assert!(
            stages.any_failed(),
            "precondition: broken index must have a failed lane"
        );
    }

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(Arc::clone(&state))).await;

    assert_eq!(
        resp.status, "degraded",
        "#1870: /health must NOT report 'ok' when a registered corpus failed to open"
    );

    let json: Value = serde_json::to_value(&resp).expect("serialize");
    let summary = &json["warmboot_summary"];
    assert_eq!(
        summary["indexes_corpus_failed"].as_u64(),
        Some(1),
        "#1870: exactly the one all-lanes-Failed index must be counted; json={json}"
    );
    assert_eq!(
        summary["warm_boot_degraded"].as_bool(),
        Some(true),
        "#1870: corpus-open failure must flip the machine-readable degraded flag"
    );
}

/// Why: the healthy path must stay `"ok"` — the #1870 downgrade is scoped to
/// indexes with a failed lane and must not regress the common case that
/// external probes (open-mpm, `ensure_daemon_running`) depend on.
/// What: registers a single index left in its default (non-failed) stage state
/// and asserts `/health` still reports `status: "ok"`, `indexes_corpus_failed:
/// 0`, and does not spuriously set `warm_boot_degraded`.
/// Test: this test.
#[tokio::test]
async fn health_stays_ok_when_no_corpus_failed() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("healthy"),
        Arc::new(RwLock::new(CodeIndexer::new("healthy", "/tmp/healthy"))),
        "/tmp/healthy".into(),
    ));

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;

    assert_eq!(resp.status, "ok");
    let json: Value = serde_json::to_value(&resp).expect("serialize");
    let summary = &json["warmboot_summary"];
    assert_eq!(summary["indexes_corpus_failed"].as_u64(), Some(0));
    assert_eq!(summary["warm_boot_degraded"].as_bool(), Some(false));
}

/// Issue #3748 slice A: `warm_boot_degraded` must stop being sticky once the
/// deferred-embed catch-up queue drains cleanly — a boot that had no TCC
/// skip, no scan timeout, and no count drop, and whose one in-flight index
/// finished Ready, must recompute to `false` rather than staying frozen at
/// whatever `record_warm_boot_result` set (here: pre-seeded `true` to prove
/// the recompute actually changes it, not just confirms a default).
///
/// Why: before this fix, nothing ever re-derived the persisted
/// `warmboot_summary.warm_boot_degraded` after boot completion — a
/// transient/stale `true` (or one set for a since-resolved reason) would
/// have wedged `warm_boot_degraded: true` in every `/health` response for
/// the rest of the daemon's life, and trusty-review's context gate reads
/// exactly that field to decide whether to skip.
/// What: seeds `warmboot_summary` with `warm_boot_degraded = true` and
/// `indexes_skipped_tcc = 0` / `indexes_skipped_timeout = 0` (i.e. nothing
/// that should be permanently sticky), registers one healthy (Ready) index,
/// calls `recompute_warm_boot_degraded` directly, and asserts the persisted
/// flag flips to `false`.
/// Test: this IS the test.
#[tokio::test]
async fn warm_boot_degraded_recomputes_to_false_once_catch_up_drains_cleanly() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new("healthy-after-drain"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "healthy-after-drain",
            "/tmp/healthy-after-drain",
        ))),
        "/tmp/healthy-after-drain".into(),
    ));
    {
        let mut stages = handle.stages.write().await;
        stages.semantic.status = crate::core::registry::StageStatus::Ready;
        stages.graph.status = crate::core::registry::StageStatus::Ready;
    }

    let state = Arc::new(SearchAppState::new(registry));
    {
        let mut summary = state.warmboot_summary.lock().expect("lock");
        summary.warm_boot_degraded = true;
        summary.indexes_skipped_tcc = 0;
        summary.indexes_skipped_timeout = 0;
    }

    super::health::recompute_warm_boot_degraded(&state);

    let summary = state.warmboot_summary.lock().expect("lock").clone();
    assert!(
        !summary.warm_boot_degraded,
        "recompute must clear a stale warm_boot_degraded once no genuine \
         cause (tcc/timeout/count/failed-stage) remains"
    );
}

/// Issue #3748 slice A: a deferred-embed pass that genuinely FAILED (issue
/// #928's `semantic = Failed`) must keep `warm_boot_degraded = true` through
/// the recompute — the fix must never weaken a real degraded signal.
///
/// Why: this is the explicit non-goal call-out from the issue: "an index
/// that failed its embed pass must still count as degraded".
/// What: registers an index with `stages.semantic = Failed`, seeds
/// `warmboot_summary` with `warm_boot_degraded = false` and no tcc/timeout
/// skips (proving the Failed stage ALONE drives the result, not a
/// pre-existing sticky value), calls `recompute_warm_boot_degraded`, and
/// asserts the flag is `true`.
/// Test: this IS the test.
#[tokio::test]
async fn warm_boot_degraded_recompute_keeps_a_genuinely_failed_embed_degraded() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageState};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new("failed-embed-3748"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "failed-embed-3748",
            "/tmp/failed-embed-3748",
        ))),
        "/tmp/failed-embed-3748".into(),
    ));
    {
        let mut stages = handle.stages.write().await;
        stages.semantic = StageState::failed("injected embed failure for test".to_string());
    }

    let state = Arc::new(SearchAppState::new(registry));
    {
        let mut summary = state.warmboot_summary.lock().expect("lock");
        summary.warm_boot_degraded = false;
        summary.indexes_skipped_tcc = 0;
        summary.indexes_skipped_timeout = 0;
    }

    super::health::recompute_warm_boot_degraded(&state);

    let summary = state.warmboot_summary.lock().expect("lock").clone();
    assert!(
        summary.warm_boot_degraded,
        "a genuinely failed embed pass must still count as degraded after recompute (#3748)"
    );
}

/// Issue #3748 slice A: an index whose semantic (or graph) stage is merely
/// `InProgress` — i.e. still catching up, not failed and not skipped — must
/// NOT be treated as degraded by the recompute. This is the "tail-blocked"
/// case: once every OTHER index is Ready and only the final (largest) job is
/// still embedding, `warm_boot_degraded` must already read `false` (assuming
/// no genuine tcc/timeout/count/failure cause), because
/// `indexes_component_catch_up_in_progress` was never one of the flag's
/// inputs to begin with.
///
/// Why: documents/pins the "flip earlier" design decision described in
/// `recompute_warm_boot_degraded`'s doc comment — no separate "tail-blocked"
/// carve-out was needed because InProgress was never a degraded input.
/// What: registers an index with `stages.semantic = InProgress`, seeds a
/// clean `warmboot_summary` (no tcc/timeout skips, `warm_boot_degraded =
/// false`), recomputes, and asserts the flag stays `false`.
/// Test: this IS the test.
#[tokio::test]
async fn recompute_does_not_flag_an_in_progress_tail_job_as_degraded() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new("tail-blocked-3748"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "tail-blocked-3748",
            "/tmp/tail-blocked-3748",
        ))),
        "/tmp/tail-blocked-3748".into(),
    ));
    {
        let mut stages = handle.stages.write().await;
        stages.semantic.status = crate::core::registry::StageStatus::InProgress;
    }

    let state = Arc::new(SearchAppState::new(registry));
    {
        let mut summary = state.warmboot_summary.lock().expect("lock");
        summary.warm_boot_degraded = false;
        summary.indexes_skipped_tcc = 0;
        summary.indexes_skipped_timeout = 0;
    }

    super::health::recompute_warm_boot_degraded(&state);

    let summary = state.warmboot_summary.lock().expect("lock").clone();
    assert!(
        !summary.warm_boot_degraded,
        "an InProgress (still embedding) tail job must not read as degraded (#3748)"
    );
}
