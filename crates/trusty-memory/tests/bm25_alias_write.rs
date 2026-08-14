//! #5036 (write half): a write through an ALIASED palace must be indexed into
//! the resolved palace's corpus.
//!
//! Why: this half is worse than the read half. A write filed into the wrong
//! palace's corpus usually SUCCEEDS, so nothing marks the palace dirty and the
//! repair sweep never runs — the drawer is durable in redb and permanently
//! absent from the index the reader consults.
//!
//! What: #5329 removed this file's `#[ignore]`, which existed only because the
//! test needed a built `trusty-bm25-daemon` binary on disk. It still lives in
//! its own binary because it seeds the process-wide mock embedder and
//! `shared_embedder_initialized()` is monotonic — a seed here would move the
//! sibling recall test off the embedder-warming path it depends on.
//!
//! Test: this *is* the test file.

mod alias_lane;

use std::time::Duration;

use serde_json::json;
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::dispatch_tool;

/// Poll a palace's own corpus until `query` returns a hit.
///
/// Why: the live write path is asynchronous by design (#231) — the drawer is
/// durable in redb before the index request leaves the queue — so the corpus is
/// eventually, not immediately, consistent.
/// What: retries the search for up to ~5 s against the palace the caller names.
/// Returns the matching doc ids, or an empty vec once the deadline passes —
/// which is itself the pre-fix symptom, since the write landed somewhere else.
/// Test: used by the test below.
async fn poll_corpus_for(lane: &Bm25Lane, palace: &str, query: &str) -> Vec<String> {
    for _ in 0..100 {
        if let Ok(hits) = lane.search(palace, query, 10).await {
            if !hits.is_empty() {
                return hits.into_iter().map(|h| h.doc_id).collect();
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Vec::new()
}

/// Why: `write_drawer` enqueued the index request under the slug the caller
/// asked for, and the index worker sent it over a client pinned to the default
/// palace. Two independent ways to miss the palace the drawer actually lives in.
/// What: writes through the alias slug and asserts the drawer shows up in the
/// CANONICAL palace's own corpus, read over that palace's socket by a client
/// this test builds itself.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_aliased_write_indexes_into_the_resolved_palaces_corpus() {
    // The write path embeds; the mock keeps that off the ONNX model-download
    // path. It has no bearing on which socket the index request is sent to,
    // which is the only thing this test measures.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let fx = alias_lane::Aliased::new("w");

    let token = "zqxjrollback";
    let payload = dispatch_tool(
        &fx.state,
        "memory_remember",
        json!({
            "palace": fx.alias,
            "text": format!("{token} procedure for the staged deployment plan"),
            "force": true,
        }),
    )
    .await
    .expect("memory_remember through the alias");
    let drawer_id = payload["drawer_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the write was skipped, not stored: {payload}"))
        .to_string();

    let lane = fx.state.bm25.as_ref().expect("the lane is armed");
    let indexed = poll_corpus_for(lane, &fx.canonical, token).await;
    assert!(
        indexed.contains(&drawer_id),
        "#5036: a write through alias '{}' must be indexed into '{}'s corpus. Got {indexed:?}",
        fx.alias,
        fx.canonical,
    );
    // The daemon-era form was "no socket exists under the alias name"; the
    // in-process form is "no index directory was ever created under it".
    assert!(
        !fx.state.data_root.join(&fx.alias).join("bm25").exists(),
        "#5036: no BM25 index may be created for the alias slug on the write path"
    );

    fx.shutdown().await;
}
