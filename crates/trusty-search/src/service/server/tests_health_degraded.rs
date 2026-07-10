//! Health-honesty regression tests for issue #1870.
//!
//! Why: split out of `tests_health.rs` to keep that file under the 500-SLOC
//! production cap (it is not classified as a test file by the line-cap gate
//! because its basename does not end in `_test.rs`/`_tests.rs`).
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
