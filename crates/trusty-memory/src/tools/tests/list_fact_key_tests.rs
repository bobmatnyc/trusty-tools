//! `memory_list` reports each drawer's Tier C `fact_key` (#9340).
//!
//! Why: `tm memory forget --fact-key <key>` resolves a slot to its drawer id on
//! the client side, from `memory_list` output, and requires exactly one live
//! match. These tests pin the three shapes that rule reads: a slotted drawer
//! carries its key, an unslotted drawer carries `null`, and a drawer displaced
//! by a later write to the same slot carries `null` while the new occupant
//! carries the key.
//! What: drives the real handlers through `dispatch_tool` against a temp
//! `AppState`.
//! Test: this IS the test module.

use super::*;

/// Write one drawer through `memory_remember` and return its id.
async fn remember(state: &AppState, palace: &str, text: &str, fact_key: Option<&str>) -> String {
    let mut args = json!({ "palace": palace, "text": text, "force": true });
    if let Some(key) = fact_key {
        args["fact_key"] = json!(key);
    }
    let out = dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
    out["drawer_id"]
        .as_str()
        .expect("drawer_id in the remember envelope")
        .to_string()
}

/// List `palace` and return its `drawers` array.
async fn list(state: &AppState, palace: &str) -> Vec<Value> {
    let out = dispatch_tool(
        state,
        "memory_list",
        json!({ "palace": palace, "limit": 50 }),
    )
    .await
    .expect("memory_list");
    out["drawers"].as_array().expect("drawers array").clone()
}

/// The listed drawer with id `id`.
fn listed<'a>(drawers: &'a [Value], id: &str) -> &'a Value {
    drawers
        .iter()
        .find(|d| d["drawer_id"] == id)
        .unwrap_or_else(|| panic!("drawer {id} not listed in {drawers:?}"))
}

/// Criterion 1: a drawer written with a `fact_key` lists that key.
#[tokio::test]
async fn memory_list_reports_a_slotted_drawers_fact_key() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({ "name": "lfk1" }))
        .await
        .expect("palace_create");

    let id = remember(
        &state,
        "lfk1",
        "PR 9340 is in flight on branch feat/9340-list-fact-key",
        Some("pr:9340/state"),
    )
    .await;

    let drawers = list(&state, "lfk1").await;
    assert_eq!(
        listed(&drawers, &id)["fact_key"],
        json!("pr:9340/state"),
        "{drawers:?}"
    );
}

/// Criterion 2: a drawer written with no `fact_key` lists the field as an
/// explicit `null`, never omits it — the documented shape.
#[tokio::test]
async fn memory_list_reports_null_fact_key_for_an_unslotted_drawer() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({ "name": "lfk2" }))
        .await
        .expect("palace_create");

    let id = remember(
        &state,
        "lfk2",
        "The workspace MSRV is pinned in the CI toolchain action",
        None,
    )
    .await;

    let drawers = list(&state, "lfk2").await;
    let drawer = listed(&drawers, &id);
    assert_eq!(
        drawer.get("fact_key"),
        Some(&Value::Null),
        "an unslotted drawer must carry `fact_key: null`: {drawer}"
    );
}

/// Criterion 3: after a second write to the same slot, the displaced drawer is
/// still listed but carries `null`, and the new occupant is the only drawer
/// listing the key.
#[tokio::test]
async fn memory_list_reports_the_key_only_on_the_slots_current_occupant() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({ "name": "lfk3" }))
        .await
        .expect("palace_create");

    let key = "pr:9340/state";
    let first = remember(
        &state,
        "lfk3",
        "PR 9340 is open and waiting on review at head a38e511d",
        Some(key),
    )
    .await;
    let second = remember(
        &state,
        "lfk3",
        "PR 9340 merged to main as a squash commit after review",
        Some(key),
    )
    .await;

    let drawers = list(&state, "lfk3").await;
    assert_eq!(
        listed(&drawers, &first).get("fact_key"),
        Some(&Value::Null),
        "the displaced drawer must stop claiming the slot: {drawers:?}"
    );
    let claimants: Vec<&str> = drawers
        .iter()
        .filter(|d| d["fact_key"] == key)
        .filter_map(|d| d["drawer_id"].as_str())
        .collect();
    assert_eq!(claimants, vec![second.as_str()], "{drawers:?}");
}

/// Criterion 6: the tool description documents the output field, since the
/// MCP surface declares no output schema.
#[test]
fn memory_list_description_documents_the_fact_key_field() {
    let defs = super::super::definitions::tool_definitions();
    let description = defs["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "memory_list")
        .expect("memory_list defined")["description"]
        .as_str()
        .expect("description string")
        .to_string();
    assert!(description.contains("`fact_key`"), "{description}");
}
