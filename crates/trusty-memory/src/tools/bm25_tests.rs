//! Unit coverage for the BM25 lexical lane's fusion and degradation arms.
//!
//! Why: #5036 gave `fuse_bm25_lane` the ability to promote drawers the vector
//! lane never returned, which turned two previously-inert properties into
//! load-bearing ones — the empty-hits early return (the whole basis for "with
//! the lane off, recall is byte-identical") and the `admits` scope filter (the
//! only thing keeping a room-scoped recall from another room's drawer). Neither
//! the promotion nor either degradation arm had a test.
//! What: pure fusion tests against a real `PalaceHandle` built over temp
//! stores, plus one async test for the supervisor and search error arms.
//! Test: this IS the test module.

use super::*;
use trusty_common::bm25_client::BM25Hit;
use trusty_common::memory_core::palace::{Drawer, PalaceId};
use trusty_common::memory_core::retrieval::{PalaceHandle, RecallResult};
use trusty_common::memory_core::store::kg::KnowledgeGraph;
use trusty_common::memory_core::store::vector::UsearchStore;

/// A handle holding `contents` as drawers, plus their ids in order.
///
/// Why: `fuse_bm25_lane` reads `handle.drawers`, so every fusion test needs a
/// real handle; building one is store setup that would otherwise be restated
/// four times.
/// What: fresh usearch + kg under a tempdir, one drawer per entry. The
/// `TempDir` comes back because dropping it deletes the stores.
/// Test: used by every `fuse_bm25_lane_*` test below.
fn handle_with_drawers(contents: &[&str]) -> (tempfile::TempDir, PalaceHandle, Vec<Uuid>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let vs = UsearchStore::new(dir.path().join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("kg");
    let handle = PalaceHandle::new(PalaceId::new("fusetest"), String::new(), vs, kg);
    let mut ids = Vec::new();
    for text in contents {
        let drawer = Drawer::new(Uuid::new_v4(), *text);
        ids.push(drawer.id);
        handle.add_drawer(drawer);
    }
    (dir, handle, ids)
}

/// One vector-lane result for `id` at `score`, on layer 2.
fn vector_result(handle: &PalaceHandle, id: Uuid, score: f32) -> RecallResult {
    let drawer = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .expect("drawer exists");
    RecallResult {
        drawer,
        score,
        layer: 2,
    }
}

/// The synthetic L0 identity row, exactly as `retrieve_l0_l1` emits it.
///
/// Why: every fixture here omitted it, and that omission is what let the
/// scaling bug through — `retrieve_l0_l1` prepends this row at a hardcoded
/// `1.0` on any palace with an identity, so a `ceiling` taken over all layers
/// is 1.0 in production no matter what the vector lane returned.
/// What: a layer-0 result at score 1.0 over an arbitrary drawer.
/// Test: `fuse_bm25_lane_ignores_the_synthetic_l0_row_when_scaling`.
fn identity_row(handle: &PalaceHandle, id: Uuid) -> RecallResult {
    RecallResult {
        layer: 0,
        score: 1.0,
        ..vector_result(handle, id, 1.0)
    }
}

/// Why: this is the production shape every other test in this file misses. With
/// `ceiling` taken over all layers it is pinned at 1.0 by the identity row, so
/// `lexical = 1.0 * (hit.score / best)` — the normalise-to-1.0 formula the
/// scaling exists to avoid — and a lexical-only hit lands above every genuine
/// cosine. The fix is inert without this fixture row.
/// What: results carry an L0 row at 1.0 and a genuine L2 hit at 0.6; asserts the
/// promoted hit is scaled to the L2 ceiling, not to 1.0, and that it does not
/// outrank the genuine vector hit.
/// Test: this test.
#[test]
fn fuse_bm25_lane_ignores_the_synthetic_l0_row_when_scaling() {
    let (_dir, handle, ids) = handle_with_drawers(&["identity", "genuine vector hit", "bm25 only"]);
    let mut results = vec![
        identity_row(&handle, ids[0]),
        vector_result(&handle, ids[1], 0.6),
    ];
    let hits = vec![BM25Hit {
        doc_id: ids[2].to_string(),
        score: 5.0,
    }];

    fuse_bm25_lane(&mut results, &handle, &hits, 10, |_| true);

    let promoted = results
        .iter()
        .find(|r| r.drawer.id == ids[2])
        .expect("the BM25-only drawer is promoted");
    assert!(
        promoted.score < 1.0,
        "the identity row's synthetic 1.0 must not become the scaling ceiling; \
         got {}",
        promoted.score
    );
    assert!(
        (promoted.score - 0.6).abs() <= 1e-5,
        "the ceiling is the best L2/L3 cosine (0.6), got {}",
        promoted.score
    );

    let genuine = results
        .iter()
        .position(|r| r.drawer.id == ids[1])
        .expect("the genuine vector hit survives");
    let promoted_pos = results
        .iter()
        .position(|r| r.drawer.id == ids[2])
        .expect("the promoted hit is present");
    assert!(
        genuine < promoted_pos,
        "a lexical-only hit must not outrank a genuine vector hit it ties"
    );
}

/// Why: "with the lane off, recall is byte-identical" is what makes #5036 safe
/// to merge ahead of `TRUSTY_BM25_DAEMON=1`, and this early return is its
/// entire basis. The branch is not only reachable when the lane is disabled —
/// `bm25_search_optional` returns `Some(vec![])` whenever the daemon is
/// reachable but matches nothing.
/// What: fuses an empty hit list and asserts length, order, layer, and score
/// are untouched — the scores especially, which the scaling arithmetic below
/// would otherwise rewrite.
/// Test: this test.
#[test]
fn fuse_bm25_lane_is_a_no_op_without_hits() {
    let (_dir, handle, ids) = handle_with_drawers(&["alpha", "beta"]);
    let mut results = vec![
        vector_result(&handle, ids[0], 0.9),
        vector_result(&handle, ids[1], 0.4),
    ];
    let before = results.clone();

    fuse_bm25_lane(&mut results, &handle, &[], 10, |_| true);

    assert_eq!(results.len(), before.len(), "length must not change");
    for (got, want) in results.iter().zip(before.iter()) {
        assert_eq!(got.drawer.id, want.drawer.id, "order must not change");
        assert_eq!(got.score, want.score, "score must not be rewritten");
        assert_eq!(got.layer, want.layer);
    }
}

/// Why: this is the behaviour #5036 turns on. The predecessor
/// `fuse_bm25_into_recall` only boosted drawers the vector lane had already
/// returned, so a drawer dense retrieval missed stayed missed — the exact case
/// a lexical lane exists to cover.
/// What: one drawer to the vector lane, a different one to BM25; asserts the
/// BM25-only drawer is present, carries `layer: 4`, and lands at the vector
/// lane's ceiling rather than above it.
/// Test: this test.
#[test]
fn fuse_bm25_lane_promotes_a_bm25_only_drawer() {
    let (_dir, handle, ids) = handle_with_drawers(&["seen by vectors", "seen only by bm25"]);
    let mut results = vec![vector_result(&handle, ids[0], 0.6)];
    let hits = vec![BM25Hit {
        doc_id: ids[1].to_string(),
        score: 7.5,
    }];

    fuse_bm25_lane(&mut results, &handle, &hits, 10, |_| true);

    let promoted = results
        .iter()
        .find(|r| r.drawer.id == ids[1])
        .expect("the BM25-only drawer must be promoted into the result set");
    assert_eq!(promoted.layer, 4, "promoted hits carry the lexical layer");
    assert!(
        (promoted.score - 0.6).abs() <= 1e-5,
        "the rank-0 lexical hit reaches the vector ceiling and stops there, got {}",
        promoted.score
    );
}

/// Why: this pins the scoring shape the first cut of #5036 got backwards.
/// Normalising a lexical hit to `1.0` put it above every possible cosine, so a
/// drawer BOTH lanes returned scored `cosine + 0.0164` and lost to a drawer only
/// BM25 returned — hybrid retrieval penalising agreement, and improving vector
/// recall making the injection worse.
/// What: gives one drawer to both lanes at a deliberately weak cosine and a
/// second to BM25 alone at a lower lexical score, then asserts the agreed
/// drawer outranks the lexical-only one.
/// Test: this test.
#[test]
fn fuse_bm25_lane_never_scores_agreement_below_a_lexical_only_hit() {
    let (_dir, handle, ids) = handle_with_drawers(&["both lanes", "bm25 only", "vector ceiling"]);
    let mut results = vec![
        vector_result(&handle, ids[2], 0.8),
        // Deliberately weak: under the old scoring this lost to the promotion.
        vector_result(&handle, ids[0], 0.1),
    ];
    let hits = vec![
        BM25Hit {
            doc_id: ids[0].to_string(),
            score: 9.0,
        },
        BM25Hit {
            doc_id: ids[1].to_string(),
            score: 8.0,
        },
    ];

    fuse_bm25_lane(&mut results, &handle, &hits, 10, |_| true);

    let agreed = results
        .iter()
        .find(|r| r.drawer.id == ids[0])
        .expect("the agreed drawer stays in the set");
    let lexical_only = results
        .iter()
        .find(|r| r.drawer.id == ids[1])
        .expect("the lexical-only drawer is promoted");
    assert!(
        agreed.score > lexical_only.score,
        "a drawer both lanes returned must outrank a weaker lexical-only hit; \
         agreed={} lexical_only={}",
        agreed.score,
        lexical_only.score
    );
}

/// Why (ADR-0027 T7): promotion is what made a scope filter necessary here.
/// While fusion could only boost drawers the already-scoped vector list held,
/// no out-of-scope drawer could enter. Now that BM25-only hits are hydrated
/// straight from the palace's drawer table, `admits` is the only thing between
/// a room-scoped recall and another room's drawer — and a regression dropping
/// the call would leak while every other test here still passed.
/// What: promotes with `admits` rejecting the hit, and asserts nothing entered.
/// Test: this test.
#[test]
fn fuse_bm25_lane_drops_a_drawer_the_scope_excludes() {
    let (_dir, handle, ids) = handle_with_drawers(&["in scope", "out of scope"]);
    let mut results = vec![vector_result(&handle, ids[0], 0.5)];
    let hits = vec![BM25Hit {
        doc_id: ids[1].to_string(),
        score: 9.9,
    }];

    fuse_bm25_lane(&mut results, &handle, &hits, 10, |d| d.id != ids[1]);

    assert!(
        !results.iter().any(|r| r.drawer.id == ids[1]),
        "a drawer the scope excludes must never enter through the lexical lane"
    );
    assert_eq!(results.len(), 1, "only the in-scope vector hit remains");
}

/// Why: both degradation arms return `None` so recall falls back to
/// vector-only, and neither had coverage. A regression turning either into a
/// propagated error would fail every recall on this path rather than degrade —
/// the fail-open contract inverted.
/// What: enables the lane but points the daemon locator at a path that does not
/// exist, then asserts `bm25_client_for_palace` (supervisor arm) and
/// `bm25_search_optional` (search arm) both answer `None`.
/// Test: this test.
#[tokio::test]
async fn bm25_lane_degrades_to_none_when_the_daemon_cannot_start() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: test-only env mutation; the vars are read during the calls below
    // and removed before returning.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
        std::env::set_var(
            "TRUSTY_BM25_DAEMON_BIN",
            tmp.path().join("no-such-daemon-binary"),
        );
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
    }
    let state = AppState::new(tmp.path().to_path_buf()).with_bm25_client_from_env();
    assert!(
        state.bm25_client.is_some(),
        "precondition: the lane must be enabled, or both arms are skipped"
    );

    assert!(
        bm25_client_for_palace(&state, "nosuchpalace")
            .await
            .is_none(),
        "a supervisor that cannot spawn must degrade to None, not error"
    );
    assert!(
        bm25_search_optional(&state, "nosuchpalace", "anything", 5)
            .await
            .is_none(),
        "an unreachable daemon must degrade to None, not error"
    );

    unsafe {
        std::env::remove_var("TRUSTY_BM25_DAEMON");
        std::env::remove_var("TRUSTY_BM25_DAEMON_BIN");
    }
}
