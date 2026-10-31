//! Default recall leaves out a replaced fact; a historical search brings it
//! back tagged (#10609, ADR-0074, owner ruling D83).
//!
//! Why: field bug on trusty-memory 0.36.2. A second `memory_note` to one
//! `fact_key` slot left the retired occupant in recall at a halved score with
//! no marker, and an agent resumed from its stale checkpoint.
//! What: two writes to one slot, and a plain edge-only supersession with no
//! slot, then recall on every surface. Default: the old drawer is absent and
//! the new one present. With `include_superseded`: the old drawer is present,
//! tagged `superseded: true` and `superseded_by: <new id>`, and ranks below
//! its replacement. Its own binary, so seeding the mock embedder cannot race
//! the lib tests' singleton.
//! Test: this IS the test module.

mod recall_support;

use recall_support::{Surface, rank_of, recall_on, remember, state_with, supersede};
use serde_json::{Value, json};
use trusty_memory::tools::dispatch_tool;
use uuid::Uuid;

const SLOT: &str = "pr:10609/state";
const QUERY: &str = "pull request 10609 merge state";
const OLD: &str = "pull request 10609 merge state: in flight, resume from branch fix-a";
const NEW: &str = "pull request 10609 merge state: merged, nothing to resume";

/// Why (#10609, the regression): fails on the pre-change code, where the old
/// drawer is returned with no marker on every surface.
/// What: two notes to one slot; the default answer on all five surfaces holds
/// the new drawer and not the old.
#[tokio::test]
async fn a_second_note_to_a_slot_leaves_the_first_out_of_default_recall() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let old = remember(&state, "project-a", OLD, &[], Some(SLOT)).await;
    let new = remember(&state, "project-a", NEW, &[], Some(SLOT)).await;

    for surface in [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ] {
        let results = recall_on(&state, surface, "project-a", QUERY).await;
        assert!(
            rank_of(&results, new).is_some(),
            "{surface:?}: the replacement is recalled: {results:#?}"
        );
        assert!(
            rank_of(&results, old).is_none(),
            "{surface:?}: the retired occupant must not be recalled: {results:#?}"
        );
    }
}

/// The MCP envelope of `tool` with the historical opt-in set.
async fn historical(state: &trusty_memory::AppState, tool: &str, args: Value) -> Vec<Value> {
    let mut args = args;
    args["include_superseded"] = json!(true);
    let out = dispatch_tool(state, tool, args).await.expect(tool);
    out["results"].as_array().expect("results").clone()
}

fn row(results: &[Value], id: Uuid) -> &Value {
    &results[rank_of(results, id).unwrap_or_else(|| panic!("{id} absent: {results:#?}"))]
}

/// Why (D83): the opt-in is the one way back to a replaced fact, and each
/// returned row names its replacement.
/// What: on the three MCP surfaces, `include_superseded: true` returns the old
/// drawer below the new one, tagged `superseded: true`, `superseded_by: new`.
#[tokio::test]
async fn include_superseded_returns_the_retired_occupant_tagged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let old = remember(&state, "project-a", OLD, &[], Some(SLOT)).await;
    let new = remember(&state, "project-a", NEW, &[], Some(SLOT)).await;

    let by_tool = [
        (
            "memory_recall",
            json!({ "palace": "project-a", "query": QUERY, "top_k": 10 }),
        ),
        (
            "memory_recall_deep",
            json!({ "palace": "project-a", "query": QUERY, "top_k": 10 }),
        ),
        ("memory_recall_all", json!({ "q": QUERY, "top_k": 10 })),
    ];
    for (tool, args) in by_tool {
        let results = historical(&state, tool, args).await;
        assert!(
            rank_of(&results, new) < rank_of(&results, old),
            "{tool}: the replacement ranks above the old drawer: {results:#?}"
        );
        let tagged = row(&results, old);
        assert_eq!(tagged["superseded"], json!(true), "{tool}: {tagged:#}");
        assert_eq!(
            tagged["superseded_by"],
            json!(new.to_string()),
            "{tool}: {tagged:#}"
        );
        assert!(
            row(&results, new).get("superseded").is_none(),
            "{tool}: the live drawer carries no supersession tag"
        );
    }
}

/// Why (D83): "replaced facts" is wider than slot retirees. A supersession the
/// dream or share path recorded as an edge, with no slot, is excluded by
/// default and returned, tagged, on a historical search.
#[tokio::test]
async fn a_non_slot_supersession_is_excluded_too() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let old = remember(&state, "project-a", OLD, &[], None).await;
    let new = remember(&state, "project-a", NEW, &[], None).await;
    supersede(&state, "project-a", old, new).await;

    for surface in [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ] {
        let results = recall_on(&state, surface, "project-a", QUERY).await;
        assert!(
            rank_of(&results, new).is_some(),
            "{surface:?}: {results:#?}"
        );
        assert!(
            rank_of(&results, old).is_none(),
            "{surface:?}: an edge-superseded drawer is excluded: {results:#?}"
        );
    }

    let results = historical(
        &state,
        "memory_recall",
        json!({ "palace": "project-a", "query": QUERY, "top_k": 10 }),
    )
    .await;
    let tagged = row(&results, old);
    assert_eq!(
        tagged["superseded_by"],
        json!(new.to_string()),
        "{tagged:#}"
    );
}
