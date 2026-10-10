//! #9544: concurrent writes through a palace alias and its target collapse to
//! one drawer.
//!
//! Why: the dedup gate in `memory_remember` is atomic only under the
//! per-palace write mutex. Keyed on the raw id, a write through an alias and a
//! write through its target took two mutexes for one store, so both could see
//! the empty pre-write snapshot and both could land.
//!
//! What: its own binary because it seeds the process-wide mock embedder; the
//! `alias_lane` fixture supplies the live alias.
//!
//! Test: this *is* the test file.

mod alias_lane;

use std::sync::Arc;

use serde_json::json;
use trusty_memory::tools::dispatch_tool;

/// Attempts per run. A race test can pass against the racy code when one
/// attempt happens to serialise, so it retries on fresh palaces.
const ATTEMPTS: usize = 8;

/// Long enough to clear the 8-token MCP filter.
const TEXT: &str =
    "Concurrent identical writes through an alias and its target palace must collapse to one drawer";

/// Why (#9544): see the module doc.
/// What: per attempt, builds a fresh aliased palace, releases two
/// `memory_remember` calls of the same text at once — one through the alias,
/// one through the target — and asserts the target holds exactly one drawer.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_remember_through_alias_and_target_stores_one_drawer() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    for attempt in 0..ATTEMPTS {
        let fx = alias_lane::Aliased::new(&format!("l{attempt}"));
        fx.state.set_ready();
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        let spawn = |palace: String| {
            let state = fx.state.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                dispatch_tool(
                    &state,
                    "memory_remember",
                    json!({"palace": palace, "text": TEXT}),
                )
                .await
            })
        };
        let via_alias = spawn(fx.alias.clone());
        let via_target = spawn(fx.canonical.clone());
        let r1 = via_alias.await.expect("join").expect("via alias");
        let r2 = via_target.await.expect("join").expect("via target");

        let listed = dispatch_tool(
            &fx.state,
            "memory_list",
            json!({"palace": fx.canonical, "limit": 10}),
        )
        .await
        .expect("memory_list");
        let drawers = listed["drawers"].as_array().expect("drawers array").len();
        assert_eq!(
            drawers, 1,
            "#9544 attempt {attempt}: alias and target writes must share one write lock; \
             responses {r1} / {r2}"
        );
        fx.shutdown().await;
    }
}
