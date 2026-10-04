//! #8275: fan-out and probe queries do not stamp `last_queried_unix`.
//!
//! Why: one synthetic query sent to 10 indexes on 2026-10-01 stamped all of
//! them, so stale indexes entered the next warm-boot set.
//! What: drives `search_report` and `global_search_report` directly against
//! lexical-only indexes and reads `last_queried_write_cache`, which holds a
//! claimed stamp and is released when the stamp is dropped. The disk write
//! that follows a kept stamp goes to the test-isolated data dir (#4255) and is
//! a no-op there, because these ids are not registered in `indexes.toml`.
//! Test: this file IS the test module.

use std::sync::Arc;
use std::time::Duration;

use super::QueryBurstGate;
use crate::core::indexer::{CodeIndexer, SearchQuery};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::server::{global_search_report, search_report, GlobalSearchRequest};
use crate::service::SearchAppState;

const WINDOW: Duration = Duration::from_millis(1000);
const IDS: [&str; 3] = ["recency-8275-a", "recency-8275-b", "recency-8275-c"];

async fn add_lexical_index(registry: &IndexRegistry, id: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut indexer = CodeIndexer::new(id, tmp.path());
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn authentication_handler() {}\n".into())])
        .await
        .expect("index batch");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    tmp
}

fn per_index_query(text: &str) -> SearchQuery {
    serde_json::from_value(serde_json::json!({ "text": text, "top_k": 1, "stage": "lexical" }))
        .expect("query")
}

fn fan_out_request(text: &str) -> GlobalSearchRequest {
    serde_json::from_value(serde_json::json!({ "query": text, "top_k": 3 })).expect("request")
}

fn stamped(state: &SearchAppState, id: &str) -> bool {
    state
        .last_queried_write_cache
        .get(&IndexId::new(id.to_string()))
        .is_some()
}

/// A burst of fan-out queries stamps nothing; one targeted query stamps its
/// own index and no other.
///
/// Why it fails on origin/main: there `search_report` claims the cache slot
/// and writes the stamp at once for every per-index search, so the burst
/// below leaves all three ids stamped.
/// What: sends one query text to three indexes concurrently through the
/// per-index path (the 2026-10-01 shape), runs three global fan-outs with the
/// same text, waits out the burst window, and asserts no index is stamped.
/// Then a query with a different text against one index stamps that index
/// only.
/// Test: this test.
#[tokio::test]
async fn a_fan_out_burst_leaves_recency_unchanged_and_a_targeted_query_advances_it() {
    let registry = IndexRegistry::new();
    let mut dirs = Vec::new();
    for id in IDS {
        dirs.push(add_lexical_index(&registry, id).await);
    }
    let mut state = SearchAppState::new(registry);
    state.query_burst_gate = Arc::new(QueryBurstGate::new(WINDOW));
    let state = Arc::new(state);

    let sweep = "authentication handler xyz";
    let per_index = IDS.map(|id| search_report(&state, id, per_index_query(sweep)));
    let _ = futures::future::join_all(per_index).await;
    for _ in 0..3 {
        let _ = global_search_report(&state, fan_out_request(sweep)).await;
    }
    // Poll for the release rather than guess its timing; bounded so a stamp
    // that is never released (origin/main) fails instead of hanging.
    let deadline = tokio::time::Instant::now() + WINDOW * 10;
    while IDS.iter().any(|id| stamped(&state, id)) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for id in IDS {
        assert!(
            !stamped(&state, id),
            "{id} was stamped by a query that swept every index (#8275)"
        );
    }

    let _ = search_report(&state, IDS[0], per_index_query("fn targeted_only")).await;
    // Timing under test: a kept stamp must survive past the burst window.
    tokio::time::sleep(WINDOW * 2).await;
    assert!(
        stamped(&state, IDS[0]),
        "a query aimed at one index must still stamp it"
    );
    assert!(!stamped(&state, IDS[1]) && !stamped(&state, IDS[2]));
    drop(dirs);
}

/// The gate groups by trimmed query text, per window.
///
/// Why: pins the classification apart from timing and the search path.
/// What: two indexes under one text form a fan-out; the same text on one
/// index, or two texts on two indexes, do not.
/// Test: this test.
#[test]
fn query_burst_gate_flags_one_text_reaching_two_indexes() {
    let gate = QueryBurstGate::new(Duration::from_secs(60));
    let first = gate.observe("shared text", "a");
    let alone = gate.observe("other text", "b");
    assert!(!first.is_fan_out(), "one index so far is not a sweep");
    let repeat = gate.observe("shared text", "a");
    assert!(!repeat.is_fan_out(), "the same index twice is not a sweep");
    let second = gate.observe("  shared text ", "b");
    assert!(first.is_fan_out() && second.is_fan_out());
    assert!(!alone.is_fan_out());
}
