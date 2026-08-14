//! Concurrency and residency-bound coverage for the in-process BM25 lane.
//!
//! Why: this file is `bm25_supervisor_concurrency.rs`, reworked for #5329. That
//! file drove real `trusty-bm25-daemon` children through five paths:
//! double-spawn serialisation, the aggregate live-daemon cap, a dead child, an
//! unserved orphan socket, and flushing a live child the cap evicted. Three of
//! those describe states only a subprocess can be in — there is no child to
//! die, and no socket to be left behind unserved. The two that were really
//! about the LANE carry over, and are the ones asserted here against the whole
//! `AppState` write path rather than against the lane in isolation (the lane's
//! own unit suite in `src/bm25_lane_tests.rs` covers it directly):
//!
//! | Daemon-era path | Here |
//! |---|---|
//! | double spawn serialised to one child | one index per palace under a fanout |
//! | aggregate cap never exceeded | residency cap holds, evictions flush |
//! | evicted live child flushed before SIGKILL | no write lost across evictions |
//! | dead child / unserved socket | retired — no subprocess exists |
//!
//! It also drops the old file's `#[ignore]`: no daemon binary is needed.
//!
//! Test: this *is* the test file.

use std::sync::Arc;

use serde_json::json;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// Build an `AppState` with the lane armed at an explicit residency cap.
///
/// Why: `with_bm25_lane_from_env` reads process-global env, which sibling tests
/// race on. `with_bm25_lane` pins the cap this file needs without touching the
/// environment at all, and it is now the only way in — `AppState::bm25` is
/// `pub(crate)` precisely so a caller cannot install a lane without also
/// rebuilding the indexer worker that writes to it.
fn state_with_lane(cap: usize) -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf()).with_bm25_lane(Bm25Lane::with_limits(
        tmp.path().to_path_buf(),
        cap,
        None,
    ));
    (state, tmp)
}

/// Why: this is the in-process form of
/// `concurrent_callers_for_one_palace_spawn_exactly_one_daemon`. Two indexes
/// over one snapshot path would each flush the other's writes away — the same
/// corruption a double spawn caused, with no socket-bind collision to stop it.
/// What: 32 concurrent writers into one palace; asserts exactly one load and
/// all 32 documents present.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_for_one_palace_converge_on_one_index() {
    let (state, _tmp) = state_with_lane(3);
    let lane = Arc::clone(state.bm25_lane().expect("lane armed"));

    let mut tasks = Vec::new();
    for i in 0..32 {
        let lane = Arc::clone(&lane);
        tasks.push(tokio::spawn(async move {
            lane.index(
                "hot",
                &format!("doc-{i}"),
                &format!("token{i} shared corpus"),
            )
            .await
        }));
    }
    for t in tasks {
        t.await.expect("task joined").expect("index succeeded");
    }

    assert_eq!(
        lane.loaded_count(),
        1,
        "one palace must be loaded exactly once however many callers race"
    );
    let stats = lane.stats("hot").await.expect("stats");
    assert_eq!(stats.doc_count, 32, "a concurrent write was lost");
    lane.shutdown().await;
}

/// Why: this is the in-process form of `a_concurrent_fanout_never_exceeds_the_cap`
/// PLUS `an_evicted_live_child_is_flushed`. The daemon-era pair had to be two
/// tests because the cap reaped a process and the flush was a separate SIGTERM
/// step; here eviction and flush are one operation, so one test proves both —
/// and the second half is the load-bearing one, because a cap that evicts
/// without flushing loses writes silently.
/// What: 20 palaces × 3 documents against a cap of 3, concurrently. Asserts the
/// cap held, that evictions actually happened, and that every one of the 60
/// documents is still findable afterwards.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fanout_holds_the_cap_and_loses_no_write() {
    const CAP: usize = 3;
    const PALACES: usize = 20;
    const DOCS: usize = 3;

    let (state, _tmp) = state_with_lane(CAP);
    let lane = Arc::clone(state.bm25_lane().expect("lane armed"));

    let mut tasks = Vec::new();
    for p in 0..PALACES {
        for d in 0..DOCS {
            let lane = Arc::clone(&lane);
            tasks.push(tokio::spawn(async move {
                lane.index(
                    &format!("palace-{p}"),
                    &format!("doc-{d}"),
                    &format!("tok{p}x{d} some drawer content"),
                )
                .await
            }));
        }
    }
    for t in tasks {
        t.await.expect("task joined").expect("index succeeded");
    }

    let resident = lane.resident_count().await;
    assert!(resident <= CAP, "resident={resident} exceeded cap={CAP}");
    assert!(
        lane.evicted_count() > 0,
        "{PALACES} palaces under a cap of {CAP} must have evicted something"
    );

    // "Ranks first", not "is the only hit": the tokenizer shares subtokens
    // across a palace's sibling documents, so a neighbour can legitimately score
    // above zero for this query.
    for p in 0..PALACES {
        for d in 0..DOCS {
            let hits = lane
                .search(&format!("palace-{p}"), &format!("tok{p}x{d}"), 5)
                .await
                .expect("search");
            assert_eq!(
                hits.first().map(|h| h.doc_id.as_str()),
                Some(format!("doc-{d}").as_str()),
                "palace-{p}/doc-{d} was lost across evictions: {hits:?}"
            );
        }
    }
    lane.shutdown().await;
}

/// Why: the two tests above drive the lane directly. This one drives the same
/// residency pressure through `memory_remember`, which is the path a real
/// deployment takes — the bounded queue, the single indexer worker, and the
/// lane's eviction all in series. A cap that only holds when called directly
/// would still be a cap that fails in production.
/// What: writes drawers into more palaces than the cap allows through the MCP
/// tool surface, then polls each palace's corpus for its own token.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_through_the_tool_surface_survive_eviction() {
    const CAP: usize = 2;
    const PALACES: usize = 6;

    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let (state, _tmp) = state_with_lane(CAP);

    let mut expected = Vec::new();
    for p in 0..PALACES {
        let palace = format!("p{p}");
        state
            .registry
            .create_palace(
                &state.data_root,
                Palace {
                    id: PalaceId::new(palace.clone()),
                    name: palace.clone(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: state.data_root.join(&palace),
                },
            )
            .expect("create palace");

        let token = format!("zqxtok{p}");
        let payload = dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": palace,
                "text": format!("{token} deployment runbook entry"),
                "force": true,
            }),
        )
        .await
        .expect("memory_remember");
        let drawer_id = payload["drawer_id"]
            .as_str()
            .unwrap_or_else(|| panic!("the write was skipped, not stored: {payload}"))
            .to_string();
        expected.push((palace, token, drawer_id));
    }

    let lane = state.bm25_lane().expect("lane armed");
    for (palace, token, drawer_id) in &expected {
        // The write path is asynchronous by design (#231), so poll rather than
        // assume the indexer worker has drained.
        let mut found = Vec::new();
        for _ in 0..100 {
            if let Ok(hits) = lane.search(palace, token, 10).await {
                if !hits.is_empty() {
                    found = hits.into_iter().map(|h| h.doc_id).collect();
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            found.contains(drawer_id),
            "a drawer written to '{palace}' was lost — the cap of {CAP} evicted its \
             index without flushing. Got {found:?}"
        );
    }

    assert!(lane.resident_count().await <= CAP);
    lane.shutdown().await;
}
