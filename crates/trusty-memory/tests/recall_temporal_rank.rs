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
use recall_support::{
    backdate, fact_key, rank_of, recall, recall_on, remember, remember_typed, state_with, Surface,
};
use serde_json::Value;
use uuid::Uuid;

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

/// The `score` of the hit for drawer `id`.
fn score_of(results: &[Value], id: Uuid) -> f64 {
    let rank = rank_of(results, id).unwrap_or_else(|| panic!("{id} recalled: {results:#?}"));
    results[rank]["score"].as_f64().expect("score")
}

/// Why (#9144): a `type` the write path accepts but recall ignores would pass
/// every unit test. This drives the whole path: `memory_remember` with `type`,
/// then `memory_recall`.
/// What: the same two texts go into a control palace untyped and into a typed
/// palace as Status and Ruling, all created now. In the control palace the
/// status text matches the query better, and the ruling's score is above 0.8
/// of it, so the Status factor alone must flip the order in the typed palace.
#[tokio::test]
async fn a_typed_ruling_outranks_a_better_matching_typed_status_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["typed", "control"], &[]).await;
    let query = "deploy freeze window for the release train";
    let status_text = "deploy freeze window for the release train is open today";
    let ruling_text = "deploy freeze window for the release train is fixed by the owner";

    let c_status = remember_typed(&state, "control", status_text, None).await;
    let c_ruling = remember_typed(&state, "control", ruling_text, None).await;
    let control = recall(&state, "control", query, 5).await;
    let (cs, cr) = (score_of(&control, c_status), score_of(&control, c_ruling));
    eprintln!(
        "#9144 control scores: status {cs} ruling {cr} ratio {}",
        cr / cs
    );
    assert!(
        cs > cr && cr / cs > 0.8,
        "precondition: the status text must match better, by less than the 0.8 factor: {control:#?}"
    );

    let status = remember_typed(&state, "typed", status_text, Some("Status")).await;
    let ruling = remember_typed(&state, "typed", ruling_text, Some("Ruling")).await;
    let results = recall(&state, "typed", query, 5).await;
    eprintln!(
        "#9144 typed scores: status {} ruling {}",
        score_of(&results, status),
        score_of(&results, ruling)
    );
    assert!(
        rank_of(&results, ruling) < rank_of(&results, status),
        "the typed Ruling must rank above the better-matching typed Status: {results:#?}"
    );
}
