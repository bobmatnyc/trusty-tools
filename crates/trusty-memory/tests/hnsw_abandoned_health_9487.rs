//! #9487 F5: `/health` keeps reporting an abandoned HNSW operation after its
//! palace is evicted from the handle cache.
//!
//! Why: a detached HNSW operation holds the vector store, not the palace
//! handle, so idle eviction could drop a tripped palace and take the only
//! health signal with it while the stuck thread and lock remained.
//! What: its own test binary, so the process-wide abandoned-op gauge it raises
//! cannot turn another test's `ok` status into `wedged`.
//! Test: this file is the test.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::hnsw_store::op_budget::{op_budget_error, OpBudgetError};
use trusty_common::memory_core::store::kg::KnowledgeGraph;
use trusty_common::memory_core::store::vector::UsearchStore;
use trusty_memory::transport::methods::health::{health, HealthQuery};
use trusty_memory::AppState;

const BUDGET: Duration = Duration::from_millis(200);
const WATCHDOG: Duration = Duration::from_secs(20);

async fn health_body(state: &AppState) -> Value {
    health(state, HealthQuery::default())
        .await
        .expect("the cheap health path never fails")
}

fn test_state(root: std::path::PathBuf) -> AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    // #88: bypass the project-slug gate — this palace has no project root.
    // SAFETY: the only test in this process, and it wants the idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // #911: flip past the warming preflight so handlers run.
    state.set_ready();
    state
}

/// Why (#9487 F5): see the module doc. Before F5, `/health` read tripped
/// breakers only from cached handles, so it went back to `ok` the moment
/// the palace was evicted.
/// What: abandons a real `run_bounded` operation on a registered palace,
/// removes that palace from the cache and drops every handle to it, then
/// asserts `/health` is not `ok` and reports the in-flight gauge. Releasing
/// the operation must bring `/health` back to `ok`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_stays_not_ok_after_a_palace_with_an_abandoned_op_is_evicted() {
    let data = tempfile::tempdir().expect("tempdir");
    let root = data.path().join("root");
    std::fs::create_dir_all(&root).expect("create data root");
    let state = test_state(root);
    assert_eq!(health_body(&state).await["status"], "ok", "baseline");

    let id = PalaceId::new("hnsw-abandoned");
    let vs = UsearchStore::new(data.path().join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&data.path().join("kg.db")).expect("kg");
    let handle = Arc::new(PalaceHandle::new(id.clone(), String::new(), vs, kg));
    state.registry.register_arc(Arc::clone(&handle));

    let vector_store = Arc::clone(&handle.vector_store);
    vector_store.op_breaker().set_budget_for_test(BUDGET);
    let (release, parked) = std::sync::mpsc::channel::<()>();
    let attempt = vector_store.hnsw_for_test().run_bounded("upsert", move || {
        let _ = parked.recv();
        Ok(())
    });
    let err = tokio::time::timeout(WATCHDOG, attempt)
        .await
        .expect("a parked operation must give up at its budget")
        .expect_err("a parked operation must fail");
    assert!(
        matches!(
            op_budget_error(&err),
            Some(OpBudgetError::BudgetExceeded { .. })
        ),
        "expected BudgetExceeded, got {err:#}"
    );

    state.registry.remove(&id);
    drop(vector_store);
    drop(handle);
    assert!(state.registry.peek(&id).is_none(), "the palace is evicted");

    let v = health_body(&state).await;
    assert_ne!(
        v["status"], "ok",
        "an abandoned op still runs; /health must not be ok after eviction: {v:?}"
    );
    assert_eq!(v["hnsw_abandoned_ops_in_flight"], 1, "got {v:?}");

    drop(release);
    let deadline = Instant::now() + WATCHDOG;
    let mut v = health_body(&state).await;
    while v["status"] != "ok" && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        v = health_body(&state).await;
    }
    assert_eq!(
        v["status"], "ok",
        "once the abandoned op returns, the gauge falls and /health recovers: {v:?}"
    );
    assert_eq!(v["hnsw_abandoned_ops_in_flight"], 0, "got {v:?}");
}
