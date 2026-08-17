//! #5053: a forgotten drawer must stop being findable on the LEXICAL lane too.
//!
//! Why: `memory_forget` deleted the drawer from redb and the vector store and
//! never deleted its BM25 document, so the lexical corpus kept the drawer's
//! full text — matching queries, feeding RRF, and holding the string in RAM and
//! in the on-disk snapshot. A user told the content is gone had it deleted on
//! one lane out of two.
//!
//! What: this test cannot be written against a mock. The claim is about what a
//! real BM25 corpus holds after a real forget, so it runs a real
//! [`Bm25Lane`](trusty_memory::bm25_lane::Bm25Lane) — the same type the shipped
//! binary installs — over the state's own data root.
//!
//! #5329 rewrote this file's setup. It used to start `trusty-bm25-daemon` in
//! the test process, bind it to the palace's canonical socket, and set
//! `TRUSTY_BM25_EXTERNAL=1` so production code addressed it as a supervised
//! subprocess. There is no subprocess now: the lane is installed directly with
//! `with_bm25_lane`, which also removes this test's last process-global env
//! mutation. Every assertion below is the one it made before.
//!
//! The lane is proven LIVE before anything is deleted: #5036 and #5186 both
//! record configurations where BM25 is dark, and against a dark lane every
//! "no longer lexically findable" assertion passes without the fix. The
//! corpus-level assertions are the load of the proof for the same reason —
//! `bm25_hits_to_recall_results` already drops hits whose drawer no longer
//! resolves, so `memory_recall` alone would answer "gone" while the lane still
//! held the text.
//!
//! Test: this *is* the test file.

use std::time::Duration;

use serde_json::json;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// The token the drawer is found by. Nonsense so nothing else can match it.
const TOKEN: &str = "zqxjforgetme";

/// Doc ids the corpus returns for `TOKEN`, polled until `expect_hits` matches.
///
/// Why: the index side of the lane is asynchronous by design (#231) — writes go
/// through the bounded indexer queue — so the corpus is eventually consistent
/// after a write. The delete side is not: `bm25_delete_document` applies the
/// delete inline before `memory_forget` returns, so the post-forget read needs
/// no grace period. The poll is here for the write and is harmless for the
/// delete.
/// What: searches up to ~5 s for the desired emptiness/non-emptiness, then
/// returns whatever the last search saw so the caller can assert on it.
async fn corpus_hits(lane: &Bm25Lane, palace: &str, expect_hits: bool) -> Vec<String> {
    let mut last = Vec::new();
    for _ in 0..100 {
        last = lane
            .search(palace, TOKEN, 10)
            .await
            .expect("bm25 search")
            .into_iter()
            .map(|h| h.doc_id)
            .collect();
        if last.is_empty() != expect_hits {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

/// Drawer ids a `memory_recall` for `TOKEN` returns.
async fn recalled_ids(state: &AppState, palace: &str) -> Vec<String> {
    let payload = dispatch_tool(
        state,
        "memory_recall",
        json!({ "palace": palace, "query": TOKEN, "top_k": 10 }),
    )
    .await
    .expect("memory_recall");
    payload["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|r| r["drawer_id"].as_str().map(str::to_string))
        .collect()
}

/// Why: forgetting a drawer left its BM25 document in place forever, so the
/// content stayed lexically matchable and stayed resident in the corpus.
/// What: writes a drawer through the real MCP write path, PROVES it is lexically
/// findable — in the corpus itself and through `memory_recall` — then forgets it
/// and asserts every lane has let go: the corpus returns no hit for the token,
/// `missing_docs` reports the id absent, `memory_recall` returns nothing, and
/// `memory_list` no longer lists it.
/// Test: this test itself. Against the parent commit the pre-forget assertions
/// pass identically and the corpus still returns the drawer id afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_drawer_leaves_the_lexical_corpus() {
    // The write path embeds; the mock keeps that off the ONNX download path.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().to_path_buf();
    let palace = "forgetlane";

    // The lane is installed explicitly rather than through the
    // `TRUSTY_BM25_DAEMON` gate: `with_bm25_lane` is the only path that points
    // the indexer worker at the lane, so reads and writes cannot disagree.
    let lane = Bm25Lane::with_limits(data_root.clone(), 3, None);
    let state = AppState::new(data_root.clone()).with_bm25_lane(std::sync::Arc::clone(&lane));
    state.set_ready();
    assert!(
        state.bm25_lane().is_some(),
        "the lexical lane must be armed, or this test proves nothing"
    );
    state
        .registry
        .create_palace(
            &data_root,
            Palace {
                id: PalaceId::new(palace.to_string()),
                name: palace.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: data_root.join(palace),
            },
        )
        .expect("create palace");

    let payload = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": palace,
            "text": format!("{TOKEN} rollback runbook for the staging deployment"),
            "force": true,
        }),
    )
    .await
    .expect("memory_remember");
    let drawer_id = payload["drawer_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the write was skipped, not stored: {payload}"))
        .to_string();

    // The lane is LIVE, and this is where that is established. Every assertion
    // after the forget is worthless without it.
    let indexed = corpus_hits(&lane, palace, true).await;
    assert!(
        indexed.contains(&drawer_id),
        "precondition: the drawer must be in the BM25 corpus before the forget, \
         or the post-forget assertions pass against a dark lane. Got {indexed:?}"
    );
    assert!(
        recalled_ids(&state, palace).await.contains(&drawer_id),
        "precondition: memory_recall must return the drawer before the forget"
    );

    let forgotten = dispatch_tool(
        &state,
        "memory_forget",
        json!({ "palace": palace, "drawer_id": drawer_id }),
    )
    .await
    .expect("memory_forget");
    assert_eq!(
        forgotten["status"], "deleted",
        "the forget must report a real deletion: {forgotten}"
    );

    // The lexical lane. This is the assertion #5053 is about: it fails on the
    // parent commit while every other assertion in this test still passes.
    let after = corpus_hits(&lane, palace, false).await;
    assert!(
        after.is_empty(),
        "#5053: a forgotten drawer must not match a lexical query any more. Got {after:?}"
    );
    let coverage = lane
        .missing_docs(palace, std::slice::from_ref(&drawer_id))
        .await
        .expect("missing_docs");
    assert_eq!(
        coverage.missing,
        vec![drawer_id.clone()],
        "#5053: the lane must no longer hold the forgotten drawer's document"
    );

    // The lanes the caller sees.
    assert!(
        recalled_ids(&state, palace).await.is_empty(),
        "a forgotten drawer must not come back from memory_recall"
    );
    let listed = dispatch_tool(&state, "memory_list", json!({ "palace": palace }))
        .await
        .expect("memory_list");
    assert!(
        !listed.to_string().contains(&drawer_id),
        "a forgotten drawer must not still be listed: {listed}"
    );

    lane.shutdown().await;
}
