//! #10377: `palace_reembed` must rebuild the lexical (BM25) lane, not only vectors.
//!
//! Why: a palace snapshot leaves `bm25/` out by design, so a restored palace
//! has drawers and (after re-embed) vectors but an empty lexical corpus. The
//! live check saw one query score 0.86 before backup and 0.71 after restore
//! while `palace_reembed` reported `missing: 0`. The only thing that refilled
//! BM25 was the startup sweep or the 300 s repair sweep, so a recall issued
//! right after the re-embed ran without the lexical lane.
//!
//! What: writes a drawer through a state with no lexical lane (the corpus
//! never sees it, as after a restore), then opens the same root with a lane
//! armed and runs `palace_reembed` with `dry_run: false`. The assertion is on
//! what the corpus answers, not on a report field.
//!
//! Test: this *is* the test file.

use std::sync::Arc;

use serde_json::json;
use trusty_memory::AppState;
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::engine::palace::{Palace, PalaceId};
use trusty_memory::tools::dispatch_tool;

const TOKEN: &str = "zqxjrestorelexical";

/// Why: see the module doc.
/// What: restore stand-in (drawer present, corpus empty) then `palace_reembed`.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn palace_reembed_rebuilds_the_lexical_lane() {
    trusty_memory::engine::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().to_path_buf();
    let palace = "restored";

    let drawer_id = {
        let before = AppState::new(data_root.clone());
        before.set_ready();
        before
            .registry
            .create_palace(
                &data_root,
                Palace::new(
                    PalaceId::new(palace.to_string()),
                    palace.to_string(),
                    data_root.join(palace),
                ),
            )
            .expect("create palace");
        let out = dispatch_tool(
            &before,
            "memory_remember",
            json!({"palace": palace, "text": format!("{TOKEN} rollback runbook"), "force": true}),
        )
        .await
        .expect("memory_remember");
        out["drawer_id"]
            .as_str()
            .unwrap_or_else(|| panic!("write skipped: {out}"))
            .to_string()
    };

    let lane = Bm25Lane::with_limits(data_root.clone(), 3, None);
    let state = AppState::new(data_root.clone()).with_bm25_lane(Arc::clone(&lane));
    state.set_ready();
    let pre = lane.search(palace, TOKEN, 10).await.expect("search");
    assert!(
        pre.is_empty(),
        "precondition: corpus must start empty: {pre:?}"
    );

    let report = dispatch_tool(
        &state,
        "palace_reembed",
        json!({"palace": palace, "dry_run": false}),
    )
    .await
    .expect("palace_reembed");

    let hits: Vec<String> = lane
        .search(palace, TOKEN, 10)
        .await
        .expect("search")
        .into_iter()
        .map(|h| h.doc_id)
        .collect();
    assert_eq!(
        hits,
        vec![drawer_id],
        "#10377: palace_reembed must rebuild BM25. report: {report}"
    );
    assert_eq!(report["bm25"]["status"], "completed", "{report}");
    lane.shutdown().await;
}
