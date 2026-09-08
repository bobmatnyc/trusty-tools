//! Issue #7125: a recall-all must not leave the estate it searched resident.
//!
//! Why: `open_palaces_blocking` opened every palace on disk into one `Vec` and
//! held all of them for the whole query, so peak residency and the LRU residue
//! afterwards both scaled with the palace count — ~94 fully hydrated palaces on
//! the estate that prompted the issue, and a 64-handle floor once the local
//! `Vec` dropped. The 64-slot cap bounded the cache, never the peak. A test
//! that only checked the RESULTS would not have caught that, so these assert on
//! the registry's open-handle count.
//!
//! What: seeds an estate wider than one `RECALL_PALACE_BATCH`, records the
//! registry's open-handle baseline, runs the real `memory_recall_all` MCP
//! dispatch, and asserts the count came back to that baseline. The second test
//! adds a palace the operator already had open and asserts the fan-out left it
//! alone. Both run with a provably cold embedder, so the degraded (BM25 + L0/L1)
//! lane serves the query and no ONNX model is needed — residency is the subject
//! either way, since both lanes open the same handles.
//! Test: this IS the test module.

use serde_json::json;
use std::path::Path;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::PalaceRegistry;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// Palaces to seed: wider than `RECALL_PALACE_BATCH` (8) so the batch loop
/// actually iterates, and wider than one batch's worth of residue.
const ESTATE: usize = 20;

/// Create `ESTATE` palaces on disk and close every handle again.
///
/// Why: the fan-out's residue is only observable against a known baseline, so
/// the seeding registry must not still be holding the handles it created.
/// What: creates each palace through a throwaway `PalaceRegistry`, then drops
/// it — every `Arc<PalaceHandle>` goes with it and the redb flocks release.
/// Returns the ids in creation order.
fn seed_estate(data_root: &Path) -> Vec<PalaceId> {
    let registry = PalaceRegistry::new();
    let mut ids = Vec::with_capacity(ESTATE);
    for i in 0..ESTATE {
        let id = PalaceId::new(format!("estate-{i:02}"));
        let palace = Palace {
            id: id.clone(),
            name: id.as_str().to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: data_root.join(id.as_str()),
        };
        registry
            .create_palace(data_root, palace)
            .unwrap_or_else(|e| panic!("create_palace({id}) failed: {e:#}"));
        ids.push(id);
    }
    drop(registry);
    ids
}

/// A recall-all hands back every palace it opened only to serve the query.
///
/// Why (#7125): against `origin/main` this reads 20, not 0 — the old
/// `open_palaces_blocking` fan-out left the whole estate in the LRU (up to the
/// 64-slot cap) after the call returned, a multi-GB floor on a real daemon
/// until the idle sweep ran. The open-handle count is the observable that can
/// distinguish the two implementations; the result set cannot, because both
/// search every palace.
/// What: seeds 20 palaces, asserts the registry starts empty, runs
/// `memory_recall_all`, and asserts the count is back to the baseline.
/// Test: this test.
#[tokio::test]
async fn recall_all_returns_open_palaces_to_baseline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ids = seed_estate(tmp.path());
    assert_eq!(ids.len(), ESTATE, "fixture seeds the full estate");

    let state = AppState::new(tmp.path().to_path_buf());
    let baseline = state.registry.len();
    assert_eq!(
        baseline, 0,
        "a fresh AppState must start with no open handles, or the assertion \
         below measures the fixture instead of the fan-out"
    );

    let result = dispatch_tool(
        &state,
        "memory_recall_all",
        json!({"q": "anything at all", "top_k": 5}),
    )
    .await
    .expect("memory_recall_all must succeed across the estate");
    assert!(
        result["results"].is_array(),
        "the fan-out must still answer: {result}"
    );

    assert_eq!(
        state.registry.len(),
        baseline,
        "recall-all must release every palace it opened only to serve the \
         query; {} of {ESTATE} were left resident",
        state.registry.len()
    );
}

/// A palace the operator already had open survives a recall-all.
///
/// Why (#7125): releasing indiscriminately would trade a residency leak for a
/// cache-eviction bug — the next request for the palace someone was actively
/// using would pay a cold reopen. The pre-call resident set is what separates
/// "this query's residue" from "the operator's working set".
/// What: opens one palace before the query and drops the caller's `Arc` (so
/// only the registry holds it, the case `release_if_unreferenced` would
/// otherwise pop), runs `memory_recall_all`, and asserts that palace is still
/// resident and is the only one.
/// Test: this test.
#[tokio::test]
async fn recall_all_keeps_palaces_that_were_already_open() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ids = seed_estate(tmp.path());
    let pinned = ids[3].clone();

    let state = AppState::new(tmp.path().to_path_buf());
    // Drop the Arc immediately: the registry is then the sole holder, which is
    // exactly the condition `release_if_unreferenced` acts on.
    drop(
        state
            .registry
            .open_palace(&state.data_root, &pinned)
            .unwrap_or_else(|e| panic!("pre-open {pinned} failed: {e:#}")),
    );
    assert_eq!(
        state.registry.len(),
        1,
        "exactly the pre-opened palace is resident before the query"
    );

    dispatch_tool(
        &state,
        "memory_recall_all",
        json!({"q": "anything at all", "top_k": 5}),
    )
    .await
    .expect("memory_recall_all must succeed across the estate");

    assert!(
        state.registry.peek(&pinned).is_some(),
        "the palace that was already open must not be evicted by a recall-all"
    );
    assert_eq!(
        state.registry.len(),
        1,
        "and nothing else may be left behind"
    );
}
