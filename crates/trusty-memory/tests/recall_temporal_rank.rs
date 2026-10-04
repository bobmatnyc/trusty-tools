//! End-to-end recall ranking for epic #9138 O4: stale snapshots rank below
//! current rulings (#8246, #9142), and a user-scope ruling reaches a project
//! palace's recall (#9143).
//!
//! Why: the unit tests in `tools::recall_rank_tests` prove the weight; these
//! prove the MCP `memory_recall` surface applies it and the rulings leg, over
//! real palaces in a temp data root.
//! What: each test builds an `AppState` on a `TempDir`, writes drawers through
//! `memory_remember`, backdates snapshot drawers in the in-memory table (the
//! table recall reads), and recalls through `dispatch_tool`. Its own binary,
//! so seeding the mock embedder cannot race the lib tests' real singleton.
//! Test: this IS the test module.

use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;
use uuid::Uuid;

/// A ready `AppState` on `tmp` with each named palace created.
async fn state_with(tmp: &TempDir, palaces: &[&str], rulings: &[&str]) -> AppState {
    seed_shared_embedder_with_mock();
    let state = AppState::new(tmp.path().to_path_buf())
        .with_rulings_palaces(rulings.iter().map(|p| p.to_string()).collect());
    state.set_ready();
    let cwd = tmp.path().to_string_lossy().to_string();
    for name in palaces {
        dispatch_tool(
            &state,
            "palace_create",
            json!({ "name": name, "force": true, "cwd": cwd }),
        )
        .await
        .expect("palace_create");
    }
    state
}

/// Write one drawer through `memory_remember`; returns its id.
async fn remember(
    state: &AppState,
    palace: &str,
    text: &str,
    tags: &[&str],
    key: Option<&str>,
) -> Uuid {
    let mut args = json!({ "palace": palace, "text": text, "tags": tags, "force": true });
    if let Some(k) = key {
        args["fact_key"] = json!(k);
    }
    let out = dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
    out["drawer_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("drawer_id")
}

/// Move a drawer's `created_at` into the past in the table recall reads.
fn backdate(state: &AppState, palace: &str, id: Uuid, age: Duration) {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");
    let mut drawers = handle.drawers.write();
    let d = drawers
        .iter_mut()
        .find(|d| d.id == id)
        .expect("drawer present");
    d.created_at = Utc::now() - age;
}

/// The stored `fact_key` of one drawer.
fn fact_key(state: &AppState, palace: &str, id: Uuid) -> Option<String> {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");
    let drawers = handle.drawers.read();
    drawers
        .iter()
        .find(|d| d.id == id)
        .expect("drawer present")
        .fact_key
        .clone()
}

async fn recall(state: &AppState, palace: &str, query: &str, top_k: u64) -> Vec<Value> {
    let out = dispatch_tool(
        state,
        "memory_recall",
        json!({ "palace": palace, "query": query, "top_k": top_k }),
    )
    .await
    .expect("memory_recall");
    out["results"].as_array().expect("results").clone()
}

/// Rank of the hit with drawer `id`, or `None` when absent.
fn rank_of(results: &[Value], id: Uuid) -> Option<usize> {
    let id = id.to_string();
    results
        .iter()
        .position(|r| r["drawer_id"].as_str() == Some(id.as_str()))
}

/// Why (#9142, #8246): the review's Q02/Q11 shape — an old status snapshot
/// that matches the query better than the current ruling outranked it.
/// What: the snapshot shares the query's exact wording, so similarity alone
/// puts it first; backdated 30 days, it must rank below the newer ruling.
#[tokio::test]
async fn a_ruling_outranks_an_older_snapshot_on_the_same_subject() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["proj"], &[]).await;
    let snapshot = remember(
        &state,
        "proj",
        "rust builder cap snapshot: the band allowed six builders as of 2026-09-01",
        &["status", "resume-target"],
        None,
    )
    .await;
    let ruling = remember(
        &state,
        "proj",
        "Owner ruling 2026-10-01: at most four rust builders run at once",
        &["bob-ruling"],
        None,
    )
    .await;
    remember(
        &state,
        "proj",
        "Sourdough starter wants daily flour and water",
        &["kg"],
        None,
    )
    .await;
    backdate(&state, "proj", snapshot, Duration::days(30));

    let results = recall(&state, "proj", "rust builder cap", 5).await;
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
    let state = state_with(&tmp, &["proj"], &[]).await;
    let tags = ["status", "resume-target"];
    let a_text = "resume state for session s1: working on PR 100";
    let a = remember(&state, "proj", a_text, &tags, Some("ws:s1/resume")).await;
    let b = remember(
        &state,
        "proj",
        "resume state for session s1: working on PR 200",
        &tags,
        Some("ws:s1/resume"),
    )
    .await;
    let c = remember(
        &state,
        "proj",
        "resume state for session s2: working on PR 300",
        &tags,
        Some("ws:s2/resume"),
    )
    .await;

    assert_eq!(
        fact_key(&state, "proj", a),
        None,
        "same key retires the first"
    );
    assert_eq!(fact_key(&state, "proj", b).as_deref(), Some("ws:s1/resume"));
    assert_eq!(
        fact_key(&state, "proj", c).as_deref(),
        Some("ws:s2/resume"),
        "another key retires nothing"
    );

    for id in [a, b, c] {
        backdate(&state, "proj", id, Duration::days(3));
    }
    let results = recall(&state, "proj", a_text, 5).await;
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

/// Why (#9143 criteria 1-3): a ruling stored in the user-scope palace must
/// reach a project palace's plain `memory_recall`, and only rulings may cross.
/// What: `rules` is the configured user-scope palace and holds a ruling plus a
/// non-ruling on the same subject; `apex` is an unconfigured project palace
/// with an on-topic note. Recall in `proj` returns the ruling in the top 3 at
/// layer 1, and neither non-ruling. A state with no rulings palace configured
/// does not see the ruling.
#[tokio::test]
async fn a_ruling_in_the_user_scope_palace_is_recalled_from_a_project_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["proj", "rules", "apex"], &["rules"]).await;
    remember(
        &state,
        "proj",
        "Quokkas are photogenic marsupials on Rottnest Island",
        &[],
        None,
    )
    .await;
    remember(
        &state,
        "proj",
        "Basalt columns form when thick lava cools slowly",
        &[],
        None,
    )
    .await;
    let ruling = remember(
        &state,
        "rules",
        "issue titles name the symptom, never the proposed fix",
        &["bob-ruling", "standing-rule"],
        None,
    )
    .await;
    let rules_note = remember(
        &state,
        "rules",
        "issue titles drafted on 2026-09-02 were long",
        &["note"],
        None,
    )
    .await;
    let apex_note = remember(
        &state,
        "apex",
        "issue titles in apex carry the ticket key",
        &[],
        None,
    )
    .await;

    let query = "issue titles name the symptom";
    let results = recall(&state, "proj", query, 5).await;
    let r = rank_of(&results, ruling).expect("the user-scope ruling is recalled");
    assert!(r < 3, "the ruling ranks in the top 3: {results:#?}");
    assert_eq!(
        results[r]["layer"],
        json!(1),
        "user-scope rulings join at L1"
    );
    assert_eq!(
        rank_of(&results, rules_note),
        None,
        "a non-ruling never crosses palaces"
    );
    assert_eq!(
        rank_of(&results, apex_note),
        None,
        "an unconfigured palace never contributes"
    );

    let unconfigured = state.clone().with_rulings_palaces(Vec::new());
    let results = recall(&unconfigured, "proj", query, 5).await;
    assert_eq!(
        rank_of(&results, ruling),
        None,
        "no rulings palace, no rulings leg"
    );
}
