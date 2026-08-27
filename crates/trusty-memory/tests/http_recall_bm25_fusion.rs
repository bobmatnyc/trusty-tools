//! #5036: the recall path the `UserPromptSubmit` hook takes must run the
//! lexical lane, not the vector lane alone.
//!
//! Why: `handle_memory_recall` has run vector and BM25 in parallel and RRF-fused
//! them since #156. The hook does not go through it. It goes
//! `commands/prompt_context/fetch.rs` → the daemon's recall route →
//! `MemoryService::recall` → `memory_core::retrieval::layers`, and there was no
//! BM25 anywhere on that chain — so a prompt was answered by vector centroid
//! with no lexical counterweight, which is dense retrieval's known weak spot and
//! BM25's strength.
//!
//! What this test can and cannot show: `fuse_bm25_into_recall` only BOOSTS
//! drawers the vector lane already returned — it never promotes a BM25-only hit,
//! because it has no drawer payload to hydrate one from. So the observable
//! effect is a drawer's SCORE, not its presence, and that is what is asserted:
//! the same drawer, the same query, scored through `MemoryService::recall`
//! against scored through `recall_with_default_embedder` directly. Equal scores
//! mean the lane never ran.
//!
//! That boost-only shape is also why this wiring is safe where three earlier
//! attempts folded (see the #5036 ruling): there is no scaling constant derived
//! from whichever rows survive truncation, so there is nothing to degenerate to
//! `1.0` when that set is empty.
//!
//! Test: this IS the test file.

use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_with_default_embedder, seed_shared_embedder_with_mock,
};
use trusty_memory::bm25_backfill::run_startup_sweep;
use trusty_memory::service::MemoryService;
use trusty_memory::AppState;

/// Arm the lexical lane and the vector lane together.
///
/// Why: this test needs BOTH — a vector hit for the fusion to boost, and a
/// lexical hit to boost it with. The two alias-lane tests deliberately run
/// without a seeded embedder, and `shared_embedder()` is a process-wide
/// `OnceCell`, so this has to be its own test binary.
fn armed_state() -> (AppState, tempfile::TempDir) {
    // SAFETY: test-only env mutation; every test in this binary sets the same
    // value, so a concurrent sibling cannot observe a different lane state.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf()).with_bm25_lane_from_env();
    state.set_ready();
    assert!(
        state.bm25_lane().is_some(),
        "the lexical lane must be armed, or this test proves nothing"
    );
    (state, tmp)
}

/// Why (#5036): the hook's recall path had no BM25 on it at all. This is the
/// fail-before case — pre-fix the two scores are identical, because
/// `MemoryService::recall` called `recall_with_default_embedder` and returned
/// its results verbatim.
/// What: seeds one drawer, builds its BM25 corpus with the real startup sweep,
/// then scores the same query both ways and asserts the service path scored
/// higher. The RRF bonus for a rank-0 lexical hit is `1/61`, so the gap is small
/// but exact and cannot arise from noise — both calls run the same deterministic
/// mock embedder over the same drawer.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_service_recall_path_runs_the_lexical_lane() {
    let (state, _tmp) = armed_state();
    let palace = "fusion-test";

    state
        .registry
        .create_palace(
            &state.data_root,
            Palace {
                id: PalaceId::new(palace.to_string()),
                name: palace.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(palace),
            },
        )
        .expect("create palace");
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace.to_string()))
        .expect("open palace");

    // A distinctive token so the lexical lane ranks this drawer first.
    let token = "zqxjrollout";
    handle
        .remember(
            format!("{token} staging plan for the quarterly rollout"),
            RoomType::Custom("t".into()),
            vec![],
            0.5,
        )
        .await
        .expect("remember");

    let outcome = run_startup_sweep(&state).await;
    assert!(
        outcome.all_verified() && outcome.swept == 1,
        "precondition: the sweep must write this palace's corpus; got {outcome:?}"
    );

    // The vector lane on its own — what the service path used to return.
    let vector_only = recall_with_default_embedder(&handle, token, 5)
        .await
        .expect("vector recall");
    let baseline = vector_only
        .first()
        .map(|r| r.score)
        .expect("the vector lane must return the drawer, or there is nothing to boost");

    let payload = MemoryService::new(state.clone())
        .recall(palace, token, 5, false)
        .await
        .expect("service recall");
    let fused = payload
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["score"].as_f64())
        .expect("service recall must return a scored row");

    assert!(
        fused > f64::from(baseline),
        "#5036: the service recall path must run the lexical lane — the fused \
         score {fused} must exceed the vector-only score {baseline}. Equal scores \
         mean BM25 never ran on this path."
    );

    if let Some(lane) = state.bm25_lane() {
        lane.shutdown().await;
    }
}
