//! Issue #1970's degraded-recall guarantee: a recall issued while the embedder
//! is genuinely cold returns normally instead of blocking or erroring.
//!
//! Why these moved out of the lib-test binary (#4836): the recall handlers now
//! consult `shared_embedder_initialized()` before degrading, and that cell is
//! process-wide. In the lib-test binary `dispatch_remember_then_recall`
//! initialises it with the real singleton, so whether these tests exercised the
//! degraded lane depended on test execution order — they still passed, but only
//! because they asserted "does not error", which both lanes satisfy. A test that
//! silently stops testing its subject is worse than no test.
//!
//! What: their own integration binary, which never seeds or initialises an
//! embedder — `palace_create` and recall are the only operations, and neither
//! touches the embedder cell. Each test asserts the cold-embedder precondition
//! explicitly, so if that ever stops holding the test fails loudly rather than
//! quietly passing on the ready path.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_common::memory_core::retrieval::shared_embedder_initialized;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::{AppState, DaemonReadiness};

/// A `Warming` state with a provably cold embedder.
///
/// Why: this is the precondition every test here depends on. Asserting it
/// (rather than assuming it) is what stops these tests from decaying back into
/// order-dependent no-ops — the exact failure mode that sent them to their own
/// binary.
/// What: fresh `AppState`, never `set_ready()`; asserts both the latch reads
/// `Warming` and the shared embedder cell is uninitialised.
/// Test: used by every test below.
fn cold_warming_state(tmp: &TempDir) -> AppState {
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(
        !shared_embedder_initialized(),
        "precondition: the shared embedder must be cold, or these tests silently \
         exercise the ready path instead of the degraded one (#4836)"
    );
    assert_eq!(
        state.readiness(),
        DaemonReadiness::Warming,
        "precondition: a fresh AppState must start Warming"
    );
    state
}

/// Every result came from a non-vector lane.
///
/// Why: "did not error" is satisfied by both lanes, so it cannot distinguish
/// degraded recall from full recall. Layer is the observable that can: L2 and L3
/// exist only when the vector search ran.
/// What: asserts no result carries `layer` 2 or 3.
fn assert_no_vector_lane(result: &serde_json::Value, tool: &str) {
    let results = result["results"].as_array().expect("results array");
    for r in results {
        let layer = r["layer"].as_u64().unwrap_or_default();
        assert!(
            layer != 2 && layer != 3,
            "{tool}: returned a layer-{layer} hit while the embedder was cold — \
             the vector lane must not run on the degraded path"
        );
    }
}

/// `memory_recall` degrades rather than blocking or erroring (issue #1970).
#[tokio::test]
async fn recall_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "warmtest-recall", "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "warmtest-recall", "query": "test query"}),
    )
    .await
    .expect("memory_recall must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall");
}

/// `memory_recall_deep` mirrors `memory_recall`'s degraded posture (issue #1970).
#[tokio::test]
async fn recall_deep_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "warmtest-recall-deep", "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");

    let result = dispatch_tool(
        &state,
        "memory_recall_deep",
        json!({"palace": "warmtest-recall-deep", "query": "test query"}),
    )
    .await
    .expect("memory_recall_deep must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall_deep");
}

/// `memory_recall_all` fans the degraded lane across every palace.
///
/// Regression guard for the gap originally fixed in #914 Part A, re-targeted at
/// graceful degradation instead of a hard error.
#[tokio::test]
async fn recall_all_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = cold_warming_state(&tmp);

    let result = dispatch_tool(
        &state,
        "memory_recall_all",
        json!({"q": "test query issued while warming up"}),
    )
    .await
    .expect("memory_recall_all must not error while Warming (issue #1970)");

    assert!(result["results"].is_array());
    assert_no_vector_lane(&result, "memory_recall_all");
}

/// Why (#8246 review): the embedder-warming path must demote stale snapshots
/// like the ready path does, or a recall issued during warm-up still puts an
/// old status line above the current ruling.
/// What: a cold, `Warming` state with the in-process BM25 lane, so the
/// lexical lane is the only query-scored lane. The snapshot repeats the query
/// terms and so out-scores the ruling on BM25; backdated 30 days it must rank
/// below it on both single-palace recall tools.
#[tokio::test]
async fn the_warming_path_demotes_stale_snapshots() {
    use trusty_common::memory_core::palace::{Drawer, PalaceId};
    use trusty_memory::bm25_lane::Bm25Lane;

    let tmp = tempfile::tempdir().expect("tempdir");
    let lane = Bm25Lane::new(tmp.path().join("bm25"));
    let state = cold_warming_state(&tmp).with_bm25_lane(lane.clone());
    let palace = "warmtest-rank";
    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": palace, "force": true, "cwd": cwd}),
    )
    .await
    .expect("palace_create");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");

    let add = |text: &str, tag: &str, age_days: i64| {
        let mut d = Drawer::new(uuid::Uuid::new_v4(), text);
        d.tags = vec![tag.to_string()];
        d.created_at = chrono::Utc::now() - chrono::Duration::days(age_days);
        let id = d.id;
        handle.add_drawer(d);
        (id, text.to_string())
    };
    let snapshot = add("rust builder cap snapshot rust builder cap", "status", 30);
    let ruling = add("rust builder cap ruling", "bob-ruling", 1);
    for (id, text) in [&snapshot, &ruling] {
        lane.index(palace, &id.to_string(), text)
            .await
            .expect("index");
    }

    for tool in ["memory_recall", "memory_recall_deep"] {
        let result = dispatch_tool(
            &state,
            tool,
            json!({"palace": palace, "query": "rust builder cap", "top_k": 5}),
        )
        .await
        .expect("recall while warming");
        assert_no_vector_lane(&result, tool);
        let ids: Vec<&str> = result["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|r| r["drawer_id"].as_str())
            .collect();
        let rank = |id: uuid::Uuid| ids.iter().position(|d| *d == id.to_string());
        let (s, r) = (rank(snapshot.0), rank(ruling.0));
        assert!(
            s.is_some() && r.is_some(),
            "{tool}: both recalled: {result:#}"
        );
        assert!(
            r < s,
            "{tool}: the ruling must outrank the stale snapshot: {result:#}"
        );
    }
}
