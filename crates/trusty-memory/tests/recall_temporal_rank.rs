//! End-to-end recall ranking for epic #9138 O4: stale snapshots rank below
//! current rulings on every recall surface (#8246, #9142).
//!
//! Why: the unit tests in `tools::recall_rank_tests` prove the weight; these
//! prove each recall surface applies it, over real palaces in a temp data root.
//! What: each test builds an `AppState` on a `TempDir`, writes drawers through
//! `memory_remember`, backdates snapshot drawers in the in-memory table (the
//! table recall reads), and recalls. Its own binary, so seeding the mock
//! embedder cannot race the lib tests' real singleton. The user-scope rulings
//! leg (#9143) is tested in `recall_rulings_leg.rs`.
//! Test: this IS the test module.

mod recall_support;

use chrono::Duration;
use recall_support::{backdate, fact_key, rank_of, recall, remember, state_with};
use serde_json::{json, Value};
use trusty_memory::service::MemoryService;
use trusty_memory::tools::dispatch_tool;

/// Why (#9142, #8246): the review's Q02/Q11 shape — an old status snapshot
/// that matches the query better than the current ruling outranked it.
/// What: the snapshot shares the query's exact wording, so similarity alone
/// puts it first; backdated 30 days, it must rank below the newer ruling.
#[tokio::test]
async fn a_ruling_outranks_an_older_snapshot_on_the_same_subject() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let snapshot = remember(
        &state,
        "project-a",
        "rust builder cap snapshot: the band allowed six builders as of 2026-09-01",
        &["status", "resume-target"],
        None,
    )
    .await;
    let ruling = remember(
        &state,
        "project-a",
        "Owner ruling 2026-10-01: at most four rust builders run at once",
        &["bob-ruling"],
        None,
    )
    .await;
    remember(
        &state,
        "project-a",
        "Sourdough starter wants daily flour and water",
        &["kg"],
        None,
    )
    .await;
    backdate(&state, "project-a", snapshot, Duration::days(30));

    let results = recall(&state, "project-a", "rust builder cap", 5).await;
    let (s, r) = (rank_of(&results, snapshot), rank_of(&results, ruling));
    assert!(
        s.is_some() && r.is_some(),
        "both drawers recalled: {results:#?}"
    );
    assert!(
        r < s,
        "the ruling must rank above the stale snapshot: {results:#?}"
    );
}

/// Why (#9142 criterion 2): a second write to a `fact_key` demotes the first;
/// a write to a different key demotes nothing.
/// What: A and B share `ws:s1/resume`, C holds `ws:s2/resume`. A loses its key
/// (retired, ADR-0028 D6) and C keeps its own. All three are three days old;
/// the query is A's own text, so similarity alone ranks A first. Retired, A is
/// an unkeyed stale snapshot and must rank below both live slots.
#[tokio::test]
async fn a_second_write_to_a_fact_key_demotes_the_first_and_another_key_demotes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let tags = ["status", "resume-target"];
    let a_text = "resume state for session s1: working on PR 100";
    let a = remember(&state, "project-a", a_text, &tags, Some("ws:s1/resume")).await;
    let b = remember(
        &state,
        "project-a",
        "resume state for session s1: working on PR 200",
        &tags,
        Some("ws:s1/resume"),
    )
    .await;
    let c = remember(
        &state,
        "project-a",
        "resume state for session s2: working on PR 300",
        &tags,
        Some("ws:s2/resume"),
    )
    .await;

    assert_eq!(
        fact_key(&state, "project-a", a),
        None,
        "same key retires the first"
    );
    assert_eq!(
        fact_key(&state, "project-a", b).as_deref(),
        Some("ws:s1/resume")
    );
    assert_eq!(
        fact_key(&state, "project-a", c).as_deref(),
        Some("ws:s2/resume"),
        "another key retires nothing"
    );

    for id in [a, b, c] {
        backdate(&state, "project-a", id, Duration::days(3));
    }
    let results = recall(&state, "project-a", a_text, 5).await;
    let (ra, rb, rc) = (
        rank_of(&results, a),
        rank_of(&results, b),
        rank_of(&results, c),
    );
    assert!(
        ra.is_some() && rb.is_some() && rc.is_some(),
        "all recalled: {results:#?}"
    );
    assert!(
        rb < ra && rc < ra,
        "the retired slot ranks below both live slots: {results:#?}"
    );
}

/// One recall surface, as the parametrised test drives it.
#[derive(Debug, Clone, Copy)]
enum Surface {
    McpRecall,
    McpRecallDeep,
    McpRecallAll,
    ServiceRecall,
    ServiceRecallAll,
}

/// The ranked hits `surface` returns for `query` in `palace`.
async fn recall_on(
    state: &trusty_memory::AppState,
    surface: Surface,
    palace: &str,
    query: &str,
) -> Vec<Value> {
    let top_k = 5;
    let out = match surface {
        Surface::McpRecall => {
            dispatch_tool(
                state,
                "memory_recall",
                json!({ "palace": palace, "query": query, "top_k": top_k }),
            )
            .await
        }
        Surface::McpRecallDeep => {
            dispatch_tool(
                state,
                "memory_recall_deep",
                json!({ "palace": palace, "query": query, "top_k": top_k }),
            )
            .await
        }
        Surface::McpRecallAll => {
            dispatch_tool(
                state,
                "memory_recall_all",
                json!({ "q": query, "top_k": top_k }),
            )
            .await
        }
        Surface::ServiceRecall => Ok(MemoryService::new(state.clone())
            .recall(palace, query, top_k, false)
            .await
            .expect("service recall")),
        Surface::ServiceRecallAll => Ok(MemoryService::new(state.clone())
            .recall_all(query, top_k, false)
            .await),
    }
    .unwrap_or_else(|e| panic!("{surface:?}: {e:#}"));
    match out.get("results") {
        Some(r) => r.as_array().expect("results").clone(),
        None => out
            .as_array()
            .unwrap_or_else(|| panic!("{surface:?}: {out}"))
            .clone(),
    }
}

/// Why (#8246 review): every recall surface demotes, not only `memory_recall`.
/// The chat tools and chat context injection route through the service
/// methods driven here (`recall_ranked`, `recall_all`); the embedder-warming
/// path is covered in `recall_degraded_lane.rs`, which needs a cold embedder.
/// What: the Q02/Q11 shape from the first test, recalled on each surface.
#[tokio::test]
async fn demotion_applies_on_every_recall_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let snapshot = remember(
        &state,
        "project-a",
        "rust builder cap snapshot: the band allowed six builders as of 2026-09-01",
        &["status", "resume-target"],
        None,
    )
    .await;
    let ruling = remember(
        &state,
        "project-a",
        "Owner ruling 2026-10-01: at most four rust builders run at once",
        &["bob-ruling"],
        None,
    )
    .await;
    backdate(&state, "project-a", snapshot, Duration::days(30));

    for surface in [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ] {
        let results = recall_on(&state, surface, "project-a", "rust builder cap").await;
        let (s, r) = (rank_of(&results, snapshot), rank_of(&results, ruling));
        assert!(r.is_some(), "{surface:?}: ruling recalled: {results:#?}");
        assert!(
            s.is_none() || r < s,
            "{surface:?}: the ruling must rank above the stale snapshot: {results:#?}"
        );
    }
}

/// Why (#8246 review): demotion used to re-sort only the `top_k` the lanes
/// returned, so a fresh drawer at rank `top_k + 1` could never be lifted.
/// What: three stale snapshots out-match a fresh note on similarity alone, so
/// the note is fourth before demotion. With `top_k` 3 the lanes now fetch 6,
/// demotion halves the snapshots, and the note enters the top 3.
#[tokio::test]
async fn a_fresh_drawer_just_past_top_k_is_lifted_above_stale_snapshots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let query = "deploy freeze window for the release train";
    for n in 1..=3 {
        let id = remember(
            &state,
            "project-a",
            &format!("{query} s{n}"),
            &["status"],
            None,
        )
        .await;
        backdate(&state, "project-a", id, Duration::days(30));
    }
    let fresh = remember(
        &state,
        "project-a",
        &format!("{query} lifts on Mondays after the train leaves"),
        &[],
        None,
    )
    .await;

    let results = recall(&state, "project-a", query, 3).await;
    assert!(
        rank_of(&results, fresh).is_some(),
        "the fresh note must be lifted into the top 3: {results:#?}"
    );
}
