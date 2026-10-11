//! A Tier C slot carries a re-verify command (#9390 PR B).
//!
//! Why: a slot names a fact a later event makes false, such as an open PR's
//! head SHA. Storing the command that re-reads the fact lets a resume pass
//! check the value instead of trusting it. The command belongs to one drawer,
//! so a write that retires the drawer must also retire its command.
//! What: drives the real handlers through `dispatch_tool` against a temp
//! `AppState`: the schema advertises `verify`, `memory_list` reports it per
//! drawer, retirement closes it, and a command on a non-slot is refused.
//! Test: this IS the test module.

use super::*;

const KEY: &str = "pr:9390/state";
const CMD_A: &str = "gh pr view 9390 --json headRefOid";
const CMD_B: &str = "gh pr view 9390 --json state,headRefOid";

/// Create `palace` through `palace_create`.
async fn create(state: &AppState, palace: &str) {
    dispatch_tool(state, "palace_create", json!({ "name": palace }))
        .await
        .expect("palace_create");
}

/// Write one slotted drawer through `memory_remember` and return its id.
async fn remember(state: &AppState, palace: &str, text: &str, verify: Option<&str>) -> String {
    let mut args = json!({ "palace": palace, "text": text, "force": true, "fact_key": KEY });
    if let Some(cmd) = verify {
        args["verify"] = json!(cmd);
    }
    let out = dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
    out["drawer_id"]
        .as_str()
        .expect("drawer_id in the remember envelope")
        .to_string()
}

/// `memory_list` of `palace`, as its `drawers` array.
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

/// The `verify` field of the listed drawer `id`; `None` when the key is absent.
fn verify_of(drawers: &[Value], id: &str) -> Option<Value> {
    drawers
        .iter()
        .find(|d| d["drawer_id"] == id)
        .unwrap_or_else(|| panic!("drawer {id} not listed in {drawers:?}"))
        .get("verify")
        .cloned()
}

/// Both write tools advertise an optional string `verify`, and `memory_list`
/// documents the field it returns.
#[test]
fn memory_remember_schema_advertises_the_verify_argument() {
    let defs = super::super::definitions::tool_definitions();
    let tools = defs["tools"].as_array().expect("tools array");
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} listed"))
            .clone()
    };
    for name in ["memory_remember", "memory_note"] {
        let schema = tool(name)["inputSchema"].clone();
        let verify = &schema["properties"]["verify"];
        assert_eq!(verify["type"], "string", "{name}: {verify}");
        let required = schema["required"].as_array().expect("required array");
        assert!(!required.contains(&json!("verify")), "{name}: {required:?}");
    }
    let description = tool("memory_list")["description"]
        .as_str()
        .expect("description string")
        .to_string();
    assert!(description.contains("`verify`"), "{description}");
}

/// A slot written with `verify` lists that command; a drawer written without
/// one lists an explicit `null`.
#[tokio::test]
async fn a_verify_command_round_trips_through_memory_list() {
    let (state, _tmp) = test_state();
    create(&state, "vw1").await;

    let id = remember(
        &state,
        "vw1",
        "PR 9390 is open on branch feat/9390-pr-b-verify-with",
        Some(CMD_A),
    )
    .await;
    let plain = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "vw1",
            "text": "The workspace MSRV is pinned in the CI toolchain action",
            "force": true,
        }),
    )
    .await
    .expect("memory_remember without a slot");
    let plain = plain["drawer_id"].as_str().expect("drawer_id").to_string();

    let drawers = list(&state, "vw1").await;
    assert_eq!(verify_of(&drawers, &id), Some(json!(CMD_A)), "{drawers:?}");
    assert_eq!(verify_of(&drawers, &plain), Some(Value::Null), "{drawers:?}");
}

/// A second write to the slot without `verify` retires the first drawer's
/// command, and the new occupant does not inherit it. A read keyed by the slot
/// instead of the drawer id would report `CMD_A` on the new drawer.
#[tokio::test]
async fn a_retired_slot_loses_its_verify_command() {
    let (state, _tmp) = test_state();
    create(&state, "vw2").await;

    let first = remember(
        &state,
        "vw2",
        "PR 9390 is open and waiting on review at head a38e511d",
        Some(CMD_A),
    )
    .await;
    let second = remember(
        &state,
        "vw2",
        "PR 9390 merged to main as a squash commit after review",
        None,
    )
    .await;

    let drawers = list(&state, "vw2").await;
    assert_eq!(verify_of(&drawers, &first), Some(Value::Null), "{drawers:?}");
    assert_eq!(verify_of(&drawers, &second), Some(Value::Null), "{drawers:?}");
}

/// A second write with its own command: the new drawer lists only that
/// command, and the retired drawer lists none.
#[tokio::test]
async fn a_superseding_write_carries_only_its_own_verify_command() {
    let (state, _tmp) = test_state();
    create(&state, "vw3").await;

    let first = remember(
        &state,
        "vw3",
        "PR 9390 is open and waiting on review at head a38e511d",
        Some(CMD_A),
    )
    .await;
    let second = remember(
        &state,
        "vw3",
        "PR 9390 was rebased onto main and now sits at head 5a6a8e36",
        Some(CMD_B),
    )
    .await;

    let drawers = list(&state, "vw3").await;
    assert_eq!(verify_of(&drawers, &first), Some(Value::Null), "{drawers:?}");
    assert_eq!(verify_of(&drawers, &second), Some(json!(CMD_B)), "{drawers:?}");
}

/// `verify` without `fact_key`, or a blank `verify`, is an error on both write
/// tools, and nothing is stored.
#[tokio::test]
async fn verify_without_fact_key_is_refused() {
    let (state, _tmp) = test_state();
    create(&state, "vw4").await;

    let text = "PR 9390 head is 5a6a8e3613 on the stacked branch";
    let cases = [
        ("memory_remember", json!({ "text": text, "force": true, "verify": CMD_A })),
        ("memory_note", json!({ "content": text, "verify": CMD_A })),
        (
            "memory_remember",
            json!({ "text": text, "force": true, "fact_key": KEY, "verify": "  \t " }),
        ),
        ("memory_note", json!({ "content": text, "fact_key": KEY, "verify": "" })),
    ];
    for (tool, mut args) in cases {
        args["palace"] = json!("vw4");
        let err = dispatch_tool(&state, tool, args.clone())
            .await
            .expect_err("a verify command with no slot, or a blank one, must be refused");
        assert!(format!("{err:#}").contains("verify"), "{tool} {args}: {err:#}");
    }
    let drawers = list(&state, "vw4").await;
    assert!(drawers.is_empty(), "nothing may be stored: {drawers:?}");
}
