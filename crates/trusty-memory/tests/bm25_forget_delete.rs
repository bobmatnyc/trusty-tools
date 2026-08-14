//! #5053: a forgotten drawer must stop being findable on the LEXICAL lane too.
//!
//! Why: `memory_forget` deleted the drawer from redb and the vector store and
//! never called `Bm25Client::delete`, so the BM25 corpus kept the drawer's full
//! text — matching queries, feeding RRF, and holding the string in the daemon's
//! RAM and its on-disk snapshot. A user told the content is gone had it deleted
//! on one lane out of two.
//!
//! What: this test cannot be written against a mock. The claim is about what a
//! real BM25 corpus holds after a real forget, so it runs a real
//! `trusty-bm25-daemon` — the library's own `run_until` entry point, the same
//! code the shipped binary runs — inside the test process, bound to the palace's
//! canonical socket with `TRUSTY_BM25_EXTERNAL=1` so production code addresses
//! it exactly as it would a supervised subprocess. Running the daemon in-process
//! rather than discovering a built binary is what lets this test run under a
//! plain `cargo test -p trusty-memory`, so the contract is checked on every run
//! instead of behind `--include-ignored`.
//!
//! The lane is proven LIVE before anything is deleted: #5036 and #5186 both
//! record configurations where BM25 is dark, and against a dark lane every
//! "no longer lexically findable" assertion passes without the fix. The
//! corpus-level assertions are the load of the proof for the same reason —
//! `bm25_hits_to_recall_results` already drops hits whose drawer no longer
//! resolves, so `memory_recall` alone would answer "gone" while the daemon
//! still held the text.
//!
//! Test: this *is* the test file.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use trusty_common::bm25_client::{socket_path_for_palace, Bm25Client};
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// The token the drawer is found by. Nonsense so nothing else can match it.
const TOKEN: &str = "zqxjforgetme";

/// Start a real BM25 daemon in this process on `socket`, serving `data_dir`.
///
/// Why: the production paths reach the lane over a UDS with
/// `TRUSTY_BM25_EXTERNAL=1`, which makes the supervisor hand back the socket
/// path untouched — so a daemon this test binds there is indistinguishable, to
/// every caller under test, from a supervised subprocess.
/// What: spawns `run_until` on the runtime and polls `stats` until the daemon
/// answers, so the test never races the bind. Returns the shutdown trigger.
/// Test: used by the test below; a daemon that never answers panics here rather
/// than letting the assertions run against a dark lane.
async fn start_daemon(
    palace: &str,
    data_dir: PathBuf,
    socket: PathBuf,
) -> tokio::sync::oneshot::Sender<()> {
    std::fs::create_dir_all(&data_dir).expect("create bm25 data dir");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let config = trusty_bm25_daemon::DaemonConfig {
        palace: palace.to_string(),
        data_dir,
        socket: Some(socket.clone()),
        // Coalescing windows only delay the snapshot write; every op is applied
        // before it is acked. A short window keeps the test quick.
        write_window_ms: 10,
        max_batch_size: 64,
    };
    tokio::spawn(async move {
        let _ = trusty_bm25_daemon::run_until(config, async move {
            let _ = stop_rx.await;
        })
        .await;
    });

    let client = Bm25Client::new(socket);
    for _ in 0..200 {
        if client.stats().await.is_ok() {
            return stop_tx;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("bm25 daemon never became reachable — the lane would be dark and this test vacuous");
}

/// Doc ids the corpus returns for `TOKEN`, polled until `expect_hits` matches.
///
/// Why: the index side of the lane is asynchronous by design (#231), so the
/// corpus is eventually consistent after a write. The delete side is not — the
/// daemon applies a delete before acking it — so the post-forget read needs no
/// grace period; the poll is here for the write and is harmless for the delete.
/// What: searches up to ~5 s for the desired emptiness/non-emptiness, then
/// returns whatever the last search saw so the caller can assert on it.
async fn corpus_hits(client: &Bm25Client, expect_hits: bool) -> Vec<String> {
    let mut last = Vec::new();
    for _ in 0..100 {
        last = client
            .search(TOKEN, 10)
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
/// content stayed lexically matchable and stayed resident in the daemon.
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

    // SAFETY: test-only env mutation. This is the only test in this binary, so
    // no sibling can observe a different lane state.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
        // No spawn supervision: the daemon this test runs in-process IS the
        // externally-managed daemon that switch describes.
        std::env::set_var("TRUSTY_BM25_EXTERNAL", "1");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().to_path_buf();
    // Short name: the socket path must stay inside `sun_path` (~104 bytes).
    let palace = format!("g{:x}", std::process::id() & 0xffff);
    let socket = socket_path_for_palace(&palace);
    let stop = start_daemon(
        &palace,
        data_root.join(&palace).join("bm25"),
        socket.clone(),
    )
    .await;
    let client = Bm25Client::new(socket.clone());

    let state = AppState::new(data_root.clone()).with_bm25_client_from_env();
    assert!(
        state.bm25_client.is_some(),
        "the lexical lane must be armed, or this test proves nothing"
    );
    state
        .registry
        .create_palace(
            &data_root,
            Palace {
                id: PalaceId::new(palace.clone()),
                name: palace.clone(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: data_root.join(&palace),
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
    let indexed = corpus_hits(&client, true).await;
    assert!(
        indexed.contains(&drawer_id),
        "precondition: the drawer must be in the BM25 corpus before the forget, \
         or the post-forget assertions pass against a dark lane. Got {indexed:?}"
    );
    assert!(
        recalled_ids(&state, &palace).await.contains(&drawer_id),
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
    let after = corpus_hits(&client, false).await;
    assert!(
        after.is_empty(),
        "#5053: a forgotten drawer must not match a lexical query any more. Got {after:?}"
    );
    let coverage = client
        .missing_docs(std::slice::from_ref(&drawer_id))
        .await
        .expect("missing_docs");
    assert_eq!(
        coverage.missing,
        vec![drawer_id.clone()],
        "#5053: the daemon must no longer hold the forgotten drawer's document"
    );

    // The lanes the caller sees.
    assert!(
        recalled_ids(&state, &palace).await.is_empty(),
        "a forgotten drawer must not come back from memory_recall"
    );
    let listed = dispatch_tool(&state, "memory_list", json!({ "palace": palace }))
        .await
        .expect("memory_list");
    assert!(
        !listed.to_string().contains(&drawer_id),
        "a forgotten drawer must not still be listed: {listed}"
    );

    let _ = stop.send(());
}
