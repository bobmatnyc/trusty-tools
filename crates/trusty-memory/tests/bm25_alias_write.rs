//! #5036 (write half): a write through an ALIASED palace must be indexed into
//! the resolved palace's corpus.
//!
//! Why: this half is worse than the read half. A write filed into the wrong
//! palace's corpus usually SUCCEEDS, so nothing marks the palace dirty and the
//! repair sweep never runs — the drawer is durable in redb and permanently
//! absent from the index the reader consults.
//!
//! What: `#[ignore]`d because it needs the `trusty-bm25-daemon` binary. Run
//! with `cargo test -p trusty-memory --test bm25_alias_write -- --include-ignored`.
//! It lives in its own binary because it seeds the process-wide mock embedder
//! and `shared_embedder_initialized()` is monotonic — a seed here would move
//! the sibling recall test off the embedder-warming path it depends on.
//!
//! Test: this *is* the test file.

mod alias_lane;

use std::time::Duration;

use serde_json::json;
use trusty_common::bm25_client::{socket_path_for_palace, Bm25Client};
use trusty_memory::tools::dispatch_tool;

/// Poll a palace's own BM25 socket until `query` returns a hit.
///
/// Why: the live write path is asynchronous by design (#231) — the drawer is
/// durable in redb before the index request leaves the queue — so the corpus
/// is eventually, not immediately, consistent.
/// What: retries the search for up to ~5 s. Returns the matching doc ids, or
/// an empty vec once the deadline passes (including when the socket never
/// appears, which is itself the pre-fix symptom).
/// Test: used by the test below.
async fn poll_corpus_for(palace: &str, query: &str) -> Vec<String> {
    let client = Bm25Client::new(socket_path_for_palace(palace));
    for _ in 0..100 {
        if let Ok(hits) = client.search(query, 10).await {
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
/// Test: this test itself. Against the parent commit the canonical socket never
/// even appears.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
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

    let indexed = poll_corpus_for(&fx.canonical, token).await;
    assert!(
        indexed.contains(&drawer_id),
        "#5036: a write through alias '{}' must be indexed into '{}'s corpus. Got {indexed:?}",
        fx.alias,
        fx.canonical,
    );
    assert!(
        !socket_path_for_palace(&fx.alias).exists(),
        "#5036: no BM25 daemon may be started for the alias slug on the write path"
    );

    fx.shutdown().await;
}
