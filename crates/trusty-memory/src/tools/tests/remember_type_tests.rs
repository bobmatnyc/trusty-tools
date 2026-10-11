//! `memory_remember` takes an optional drawer `type` (#9144 PR B).
//!
//! Why: a write that names a type must store that type or fail, and a write
//! that names none must behave exactly as before. These tests pin the three
//! shapes: every writable type round-trips through `memory_list`, an unknown
//! or non-string type is an error that writes nothing, and the type check runs
//! before the content gates so a gate's "skipped" success envelope can never
//! hide a bad type.
//! What: drives the real handlers through `dispatch_tool` against a temp
//! `AppState`.
//! Test: this IS the test module.

use super::*;

/// Create `palace` through `palace_create`.
async fn create(state: &AppState, palace: &str) {
    dispatch_tool(state, "palace_create", json!({ "name": palace }))
        .await
        .expect("palace_create");
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

/// The listed `drawer_type` of drawer `id`.
fn listed_type(drawers: &[Value], id: &str) -> String {
    drawers
        .iter()
        .find(|d| d["drawer_id"] == id)
        .unwrap_or_else(|| panic!("drawer {id} not listed in {drawers:?}"))["drawer_type"]
        .as_str()
        .expect("drawer_type string")
        .to_string()
}

/// Why (#9144 criteria 1 and 3): an explicit type is stored and listed back,
/// matching is case-insensitive, and an omitted or `null` type keeps the
/// heuristic classifier's answer (`Commit` for a conventional commit line,
/// `Unknown` for plain prose).
#[tokio::test]
async fn dispatch_remember_stores_requested_type_and_list_round_trips_it() {
    let (state, _tmp) = test_state();
    create(&state, "rt1").await;
    let cases = [
        (
            json!("ruling"),
            "Ruling",
            "Owner ruling: never merge a red CI run",
        ),
        (
            json!("DECISION"),
            "Decision",
            "Decision: ship PR A before PR B lands",
        ),
        (
            json!("Status"),
            "Status",
            "Status: batch A is in review this morning",
        ),
        (
            json!("tUrN"),
            "Turn",
            "Turn: the operator asked for the gate table",
        ),
        (
            json!("Reference"),
            "Reference",
            "Reference: ADR-0067 owns the drawer format",
        ),
        (
            Value::Null,
            "Unknown",
            "The sourdough starter wants flour every day",
        ),
        (
            Value::Null,
            "Commit",
            "feat(trusty-memory): add a typed drawer write arg",
        ),
    ];
    let mut written = Vec::new();
    for (ty, want, text) in cases {
        let mut args = json!({ "palace": "rt1", "text": text, "force": true });
        // `Value::Null` stands for "omitted" except in the one case below.
        if !ty.is_null() {
            args["type"] = ty;
        }
        let out = dispatch_tool(&state, "memory_remember", args)
            .await
            .expect("memory_remember");
        assert_eq!(out["status"], "stored", "{out}");
        written.push((
            out["drawer_id"].as_str().expect("drawer_id").to_string(),
            want,
        ));
    }
    // An explicit JSON `null` is the same as omitting the argument.
    let out = dispatch_tool(
        &state,
        "memory_remember",
        json!({ "palace": "rt1", "text": "The kettle descaler lives under the sink", "type": null, "force": true }),
    )
    .await
    .expect("memory_remember with type null");
    written.push((
        out["drawer_id"].as_str().expect("drawer_id").to_string(),
        "Unknown",
    ));

    let drawers = list(&state, "rt1").await;
    for (id, want) in written {
        assert_eq!(listed_type(&drawers, &id), want, "{drawers:?}");
    }
}

/// Why (#9144 criterion 2): a type the write path does not accept must fail
/// the call and store nothing — never fall back to `Unknown` or the heuristic.
/// What: older read-only variants (`UserFact`), an unknown name (`Note`) and a
/// non-string value each return `Err` naming the tool; the palace stays empty.
#[tokio::test]
async fn dispatch_remember_rejects_unknown_type_and_writes_nothing() {
    let (state, _tmp) = test_state();
    create(&state, "rt2").await;
    let text = "A drawer that must never be stored under a bad type";
    for bad in [json!("UserFact"), json!("Note")] {
        let err = dispatch_tool(
            &state,
            "memory_remember",
            json!({ "palace": "rt2", "text": text, "type": bad, "force": true }),
        )
        .await
        .expect_err("an unknown type must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.starts_with("memory_remember: "), "{msg}");
        assert!(msg.contains("unknown drawer type"), "{msg}");
    }
    for bad in [json!(42), json!(true), json!(["Ruling"])] {
        let err = dispatch_tool(
            &state,
            "memory_remember",
            json!({ "palace": "rt2", "text": text, "type": bad, "force": true }),
        )
        .await
        .expect_err("a non-string type must be rejected");
        assert!(
            format!("{err:#}").starts_with("memory_remember: "),
            "{err:#}"
        );
    }
    assert!(
        list(&state, "rt2").await.is_empty(),
        "nothing may be written"
    );
}

/// Why (#9144 criterion 2): the blocklist, content and dedup gates answer with
/// a success "skipped" envelope. Run before the type check, they would mask a
/// bad type as a harmless no-op.
/// What: without `force`, a short text, a blocklisted text and a duplicate of a
/// just-stored text each carry a bad type; each call must be `Err`.
#[tokio::test]
async fn dispatch_remember_bad_type_beats_content_gate() {
    let (state, _tmp) = test_state();
    create(&state, "rt3").await;
    let dup = "The release train leaves every Monday after the freeze lifts";
    dispatch_tool(
        &state,
        "memory_remember",
        json!({ "palace": "rt3", "text": dup }),
    )
    .await
    .expect("seed the dedup window");
    for text in ["ok", "Tool use: Bash", dup] {
        let res = dispatch_tool(
            &state,
            "memory_remember",
            json!({ "palace": "rt3", "text": text, "type": "Note" }),
        )
        .await;
        let err = res.expect_err("a bad type must beat every content gate");
        assert!(
            format!("{err:#}").contains("unknown drawer type"),
            "{text:?}: {err:#}"
        );
    }
}

/// Why (#9144 criterion 1): clients learn the argument from the schema; the
/// property is spliced in after the main `json!` literal, so pin that it lands.
/// What: `memory_remember` advertises `type` as an optional string with the five
/// writable names; `memory_note` advertises none.
#[test]
fn memory_remember_schema_advertises_the_type_enum() {
    let defs = tool_definitions();
    let tools = defs["tools"].as_array().expect("tools array");
    let schema = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} listed"))["inputSchema"]
            .clone()
    };
    let remember = schema("memory_remember");
    let ty = &remember["properties"]["type"];
    assert_eq!(ty["type"], "string", "{ty}");
    assert_eq!(
        ty["enum"],
        json!(["Ruling", "Decision", "Status", "Turn", "Reference"])
    );
    let required = remember["required"].as_array().expect("required array");
    assert!(!required.contains(&json!("type")), "{required:?}");
    assert!(schema("memory_note")["properties"].get("type").is_none());
}
