//! Regression tests for #4836 — `memory_recall` must answer the query it was
//! given, not the same drawers every time.
//!
//! Why: on a live daemon, 19 distinct queries — including a nonsense control —
//! returned the identical 5 drawers, every one at `score: 1.0`, out of a
//! 1,066-drawer palace. The store and the vector search were fine: the HTTP
//! `/recall` route discriminated correctly against that same palace at that same
//! moment. What differed was the MCP tool path, which gates on the daemon
//! readiness latch and falls back to a degraded L0/L1 lane that ignores the
//! query. That latch is written once, by the startup warm-up task; when the
//! single attempt failed it stayed `Warming` for the daemon's whole life
//! (observed: 6.8 h uptime still reporting `warming`) even though the embedder
//! had since initialised and was serving the HTTP path. So every MCP recall
//! served the query-independent fallback forever.
//!
//! What: these tests reproduce that exact condition — a live embedder behind a
//! stale `Warming` latch — and assert the recall paths discriminate anyway.
//! Lives in its own integration binary (hence its own process) so seeding the
//! mock embedder cannot race the lib-test binary's `dispatch_remember_then_recall`,
//! which deliberately uses the real process-wide singleton.
//! Test: this IS the test module.

use serde_json::json;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::{AppState, DaemonReadiness};

/// Drawers with deliberately disjoint vocabularies, so "did recall discriminate"
/// is answerable without depending on semantic nuance the mock embedder (a
/// byte-position hash) does not model.
const DRAWERS: [&str; 4] = [
    "Quokkas are photogenic marsupials native to Rottnest Island in Australia",
    "Sourdough starter needs daily feeding with equal weights of flour and water",
    "The Voyager probes each carry a gold-plated record of sounds from Earth",
    "Basalt columns form when thick lava cools and fractures into hexagons",
];

/// Build a ready `AppState` and populate it with [`DRAWERS`].
///
/// Why: the bug only shows up against a palace that HAS vector-indexed content —
/// an empty palace returns nothing on every path and would hide the defect.
/// Writes run while the state is `Ready` so `write_drawer` embeds inline rather
/// than deferring to a background task the test would have to await.
/// What: creates palace `recalltest`, writes every drawer through the real MCP
/// `memory_remember` dispatch, returns the state.
/// Test: used by every test below.
async fn seeded_state(tmp: &TempDir) -> AppState {
    seed_shared_embedder_with_mock();
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();

    let cwd = tmp.path().to_string_lossy().to_string();
    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "recalltest", "force": true, "cwd": cwd }),
    )
    .await
    .expect("palace_create");

    for text in DRAWERS {
        dispatch_tool(
            &state,
            "memory_remember",
            json!({ "palace": "recalltest", "text": text, "force": true }),
        )
        .await
        .expect("memory_remember");
    }
    state
}

/// Reproduce #4836's stale latch: a daemon whose embedder is live but whose
/// readiness flag never got flipped.
///
/// Why: this is the whole defect. `set_ready` is one-way by design, so the test
/// writes the atomic directly — the point is to model a daemon that never
/// reached `Ready`, not to exercise a supported downgrade.
/// What: stores `Warming` into `daemon_readiness` while the shared embedder cell
/// stays initialised.
/// Test: used by the three recall tests below.
fn strand_latch_in_warming(state: &AppState) {
    state
        .daemon_readiness
        .store(DaemonReadiness::Warming as u8, Ordering::Release);
    assert_eq!(
        state.readiness(),
        DaemonReadiness::Warming,
        "precondition: the latch must read Warming for this to reproduce #4836"
    );
}

/// Collect the drawer ids a recall returned, in rank order.
///
/// Asserts non-emptiness: two empty result sets would satisfy every `assert_ne!`
/// below without recall having worked at all, so "returned nothing" must fail
/// here rather than pass silently downstream.
fn drawer_ids(result: &serde_json::Value, label: &str) -> Vec<String> {
    let ids: Vec<String> = result["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["drawer_id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !ids.is_empty(),
        "#4836: recall returned NO results for {label} against a populated \
         palace — the vector lane was skipped entirely"
    );
    ids
}

/// Concatenated content of a recall response, for substring assertions.
fn contents(result: &serde_json::Value) -> String {
    result["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" || ")
}

/// Two clearly different queries must not return the same drawer set.
///
/// Why (#4836): the headline symptom. Nineteen queries, including
/// `"banana pancake recipe unrelated nonsense gibberish xyzzy"`, returned one
/// identical 5-drawer list. Before the fix this test fails with two identical id
/// vectors, because the stale latch routes both queries into the L0/L1 fallback
/// that never reads the query at all.
/// What: recalls twice against the seeded palace behind a stranded latch and
/// asserts the returned id lists differ.
/// Test: this IS the test.
#[tokio::test]
async fn different_queries_return_different_drawers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = seeded_state(&tmp).await;
    strand_latch_in_warming(&state);

    let quokkas = dispatch_tool(
        &state,
        "memory_recall",
        json!({ "palace": "recalltest", "query": DRAWERS[0], "top_k": 3 }),
    )
    .await
    .expect("memory_recall");

    let basalt = dispatch_tool(
        &state,
        "memory_recall",
        json!({ "palace": "recalltest", "query": DRAWERS[3], "top_k": 3 }),
    )
    .await
    .expect("memory_recall");

    assert_ne!(
        drawer_ids(&quokkas, "the marsupial query"),
        drawer_ids(&basalt, "the basalt query"),
        "#4836: two unrelated queries returned an identical drawer set — recall \
         is ignoring the query.\n  marsupial query -> {}\n  basalt query    -> {}",
        contents(&quokkas),
        contents(&basalt)
    );
}

/// A query carrying a drawer's distinctive text must return THAT drawer.
///
/// Why (#4836): "different results" alone is too weak a bar — the operator
/// question the tool exists to answer is "is this specific fact retrievable?",
/// and issue #4834 gates deleting 149 memory files on that answer. This pins the
/// tool to the affirmative form.
/// What: queries with the Voyager drawer's own text and asserts it comes back.
/// Test: this IS the test.
#[tokio::test]
async fn a_query_matching_a_drawer_returns_that_drawer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = seeded_state(&tmp).await;
    strand_latch_in_warming(&state);

    let result = dispatch_tool(
        &state,
        "memory_recall",
        json!({ "palace": "recalltest", "query": DRAWERS[2], "top_k": 3 }),
    )
    .await
    .expect("memory_recall");

    let found = contents(&result);
    assert!(
        found.contains("Voyager"),
        "#4836: recalling a drawer's own text did not return that drawer.\n  \
         query: {}\n  got:   {found}",
        DRAWERS[2]
    );
}

/// `memory_recall_deep` shares the defect and must share the fix.
///
/// Why (#4836): the issue reproduced on both tools, and deep recall runs the
/// same readiness gate before its L3 leg. A fix applied to only one of them
/// leaves the operator with a verification surface that is right half the time,
/// which is worse than one that is uniformly wrong.
/// What: the discrimination assertion from
/// `different_queries_return_different_drawers`, against `memory_recall_deep`.
/// Test: this IS the test.
#[tokio::test]
async fn deep_recall_also_discriminates_by_query() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = seeded_state(&tmp).await;
    strand_latch_in_warming(&state);

    let sourdough = dispatch_tool(
        &state,
        "memory_recall_deep",
        json!({ "palace": "recalltest", "query": DRAWERS[1], "top_k": 3 }),
    )
    .await
    .expect("memory_recall_deep");

    let voyager = dispatch_tool(
        &state,
        "memory_recall_deep",
        json!({ "palace": "recalltest", "query": DRAWERS[2], "top_k": 3 }),
    )
    .await
    .expect("memory_recall_deep");

    assert_ne!(
        drawer_ids(&sourdough, "the sourdough query"),
        drawer_ids(&voyager, "the voyager query"),
        "#4836: memory_recall_deep returned an identical drawer set for two \
         unrelated queries.\n  sourdough -> {}\n  voyager   -> {}",
        contents(&sourdough),
        contents(&voyager)
    );
}

/// Resolving the embedder clears a stranded `Warming` latch.
///
/// Why (#4836): this is the root cause at its own level, independent of recall.
/// The latch had exactly one writer — the startup warm-up task — so a single
/// failed attempt was unrecoverable, and nothing downstream could tell the
/// difference between "the embedder is cold" and "we never managed to say it
/// warmed up". A successfully resolved embedder is proof a vector search can
/// run, so it is now also a readiness signal.
/// What: strands the latch in `Warming`, resolves the embedder, asserts the
/// daemon reports `Ready` without any explicit `set_ready` call.
/// Test: this IS the test.
#[tokio::test]
async fn resolving_the_embedder_marks_a_warming_daemon_ready() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());

    assert_eq!(
        state.readiness(),
        DaemonReadiness::Warming,
        "a fresh AppState starts Warming"
    );

    state.embedder().await.expect("resolve embedder");

    assert_eq!(
        state.readiness(),
        DaemonReadiness::Ready,
        "#4836: a resolved embedder must clear the Warming latch, otherwise a \
         daemon whose startup warm-up failed once degrades every recall forever"
    );
}
