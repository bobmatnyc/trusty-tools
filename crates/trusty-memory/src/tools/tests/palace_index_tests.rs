//! Dispatcher-level tests for the no-palace palace index (#6318).
//!
//! Why: the owner ruling is about what a CALLER sees, so the contract has to be
//! pinned where a caller stands — at `dispatch_tool`, against the real
//! handlers. Two halves have to hold together: every palace-scoped READ tool
//! answers a no-palace call with a structured index instead of an error, and
//! every mutating tool still refuses, because an index is not a safe default
//! for a write.
//! What: drives `dispatch_tool` against a tempdir-rooted `AppState` that has no
//! `--palace` default.
//! Test: this IS the test module.

use super::*;
use crate::tools::palace_index::PALACE_INDEX_DETAIL_LIMIT;

/// The palace-scoped READ tools that answer a no-palace call with an index.
///
/// Why: one list, so a new read tool is added here rather than growing a
/// fourteenth near-identical test. `palace_verify_embedded` is absent on
/// purpose — it is being changed under #6836 and joins this list there.
/// What: every tool name whose handler calls `resolve_palace_or_index`.
const READ_TOOLS: &[&str] = &[
    "memory_recall",
    "memory_recall_deep",
    "memory_list",
    "room_list",
    "wing_list",
    "kg_query",
    "kg_gaps",
    "kg_list_subjects",
    "task_list",
    "palace_info",
    "chat_session_get",
    "chat_session_list",
    "chat_session_recall",
];

/// The mutating tools that still refuse a no-palace call.
///
/// Why: a write with no resolvable target has no defensible destination, so
/// #6318 deliberately does not reach them. Naming them here is what stops the
/// index leaking onto the write side later.
const WRITE_TOOLS: &[&str] = &[
    "memory_remember",
    "memory_note",
    "memory_forget",
    "room_create",
    "room_rename",
    "wing_create",
    "kg_assert",
    "kg_retract_triple",
    "add_alias",
    "kg_bootstrap",
    "discover_aliases",
    "task_add",
    "task_complete",
    "chat_session_create",
    "chat_session_delete",
    "palace_compact",
];

/// Assert the payload is the #6318 index and hand back its palace rows.
fn index_rows(payload: &Value, tool: &str) -> Vec<Value> {
    assert_eq!(payload["status"], "no_palace_specified", "tool={tool}");
    assert_eq!(payload["tool"], tool, "tool={tool}");
    assert!(
        payload["hint"].as_str().is_some_and(|h| !h.is_empty()),
        "tool={tool} carries no hint: {payload}"
    );
    payload["palaces"]
        .as_array()
        .unwrap_or_else(|| panic!("tool={tool} has no palaces array: {payload}"))
        .clone()
}

/// Every palace-scoped read tool answers a no-palace call with an index.
///
/// Why: this is the ruling. On `origin/main` each of these returns
/// `Err("<tool>: missing 'palace' (no --palace default configured)")`, so this
/// test is the one that fails before the change.
/// What: dispatches each read tool with an EMPTY argument object — no palace
/// and none of its other arguments either — and asserts a successful,
/// structured index naming the one palace on the host. The empty object is
/// deliberate: it also pins that the index is decided before any other
/// argument is validated, so a caller who knows nothing still gets the index
/// rather than a complaint about a missing `query` or `session_id`.
/// Test: this function.
#[tokio::test]
async fn every_read_tool_returns_an_index_with_no_palace() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "indexed"}))
        .await
        .expect("palace_create");

    for tool in READ_TOOLS {
        let payload = dispatch_tool(&state, tool, json!({}))
            .await
            .unwrap_or_else(|e| panic!("{tool} must not error with no palace: {e:#}"));
        let rows = index_rows(&payload, tool);
        assert_eq!(payload["palace_count"], 1, "tool={tool}");
        assert_eq!(rows[0]["palace"], "indexed", "tool={tool}");
    }
}

/// Every mutating tool still refuses a no-palace call.
///
/// Why: an index is a truthful answer to "what can I read"; it is not a target
/// for a write. Losing that split would silently drop a caller's memory.
/// What: dispatches each write tool with an empty argument object and asserts
/// an error. The error may be the missing-palace one or a missing-argument one
/// — the contract under test is that nothing succeeds, not which complaint
/// comes first.
/// Test: this function.
#[tokio::test]
async fn every_write_tool_still_errors_with_no_palace() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "indexed"}))
        .await
        .expect("palace_create");

    for tool in WRITE_TOOLS {
        let result = dispatch_tool(&state, tool, json!({})).await;
        assert!(
            result.is_err(),
            "{tool} must still refuse a no-palace call, got: {:?}",
            result.ok()
        );
    }
}

/// The index carries the counts a caller needs to pick a palace.
///
/// Why: a bare list of names would not answer "which one holds my work". The
/// ruling asks for an index, and the counts are what make it one.
/// What: writes one drawer into a named room, then reads the index through
/// `memory_recall` and asserts the palace's row reports the drawer, the room,
/// the default wing, and that room's own drawer count.
/// Test: this function.
#[tokio::test]
async fn palace_index_reports_counts_and_rooms() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "counted"}))
        .await
        .expect("palace_create");
    dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "counted",
            "text": "The palace index reports drawer and room counts so a caller can choose",
            "room": "Planning",
        }),
    )
    .await
    .expect("memory_remember");

    let payload = dispatch_tool(&state, "memory_recall", json!({}))
        .await
        .expect("memory_recall with no palace returns an index");
    let rows = index_rows(&payload, "memory_recall");
    let row = &rows[0];
    assert_eq!(row["palace"], "counted");
    assert_eq!(row["drawer_count"], 1, "row={row}");
    assert!(
        row["wing_count"].as_u64().is_some_and(|n| n >= 1),
        "row={row} must report the default wing"
    );
    let rooms = row["rooms"].as_array().expect("rooms array");
    let planning = rooms
        .iter()
        .find(|r| r["label"] == "Planning")
        .unwrap_or_else(|| panic!("no Planning room in {row}"));
    assert_eq!(planning["drawer_count"], 1, "row={row}");
    assert_eq!(
        row["room_count"].as_u64().map(|n| n as usize),
        Some(rooms.len()),
        "room_count must match the rooms it lists"
    );
}

/// An estate with no palaces still succeeds.
///
/// Why: "there is nothing here" is the most useful thing a first-run caller can
/// be told, and it is exactly the case the old error handled worst.
/// What: dispatches `memory_list` against a state with no palaces at all and
/// asserts an empty index rather than an error.
/// Test: this function.
#[tokio::test]
async fn palace_index_on_an_empty_estate_is_still_a_success() {
    let (state, _tmp) = test_state();
    let payload = dispatch_tool(&state, "memory_list", json!({}))
        .await
        .expect("an empty estate is an empty index, not an error");
    assert_eq!(index_rows(&payload, "memory_list").len(), 0);
    assert_eq!(payload["palace_count"], 0);
}

/// A named palace that does not exist still errors.
///
/// Why: the ruling covers "not specified", not "specified wrongly". A typo that
/// came back as an index would hide the typo.
/// What: dispatches `memory_recall` with an explicit palace no one created.
/// Test: this function.
#[tokio::test]
async fn a_named_palace_that_does_not_exist_still_errors() {
    let (state, _tmp) = test_state();
    let result = dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "no-such-palace", "query": "anything"}),
    )
    .await;
    assert!(
        result.is_err(),
        "a named-but-missing palace must still error, got: {:?}",
        result.ok()
    );
}

/// A server default still resolves; the index is the last resort.
///
/// Why: `serve --palace <name>` sessions must be unaffected — the index is what
/// happens when nothing resolves, never a new precedence step.
/// What: builds an `AppState` bound to a default palace and asserts
/// `memory_list` with no `palace` argument returns the listing shape, not the
/// index.
/// Test: this function.
#[tokio::test]
async fn a_default_palace_still_wins_over_the_index() {
    skip_palace_enforcement();
    seed_embedder();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state =
        AppState::new(tmp.path().to_path_buf()).with_default_palace(Some("defaulted".to_string()));
    state.set_ready();
    dispatch_tool(&state, "palace_create", json!({"name": "defaulted"}))
        .await
        .expect("palace_create");

    let payload = dispatch_tool(&state, "memory_list", json!({}))
        .await
        .expect("memory_list");
    assert_eq!(payload["palace"], "defaulted", "payload={payload}");
    assert!(
        payload["status"].is_null(),
        "a resolvable default must not produce an index: {payload}"
    );
}

/// Beyond the detail cap a palace is still listed, just not described.
///
/// Why: a full row opens the palace's redb file, so the index has to stop
/// describing at some point. What it must never do is stop LISTING — a palace
/// the caller cannot see is a palace they cannot name.
/// What: creates one more palace than [`PALACE_INDEX_DETAIL_LIMIT`] allows and
/// asserts every palace appears, exactly `PALACE_INDEX_DETAIL_LIMIT` of them
/// carry counts, and the remainder are marked `detail: "omitted"`.
/// Test: this function.
#[tokio::test]
async fn palace_index_details_are_capped_and_the_rest_are_still_listed() {
    let (state, _tmp) = test_state();
    let total = PALACE_INDEX_DETAIL_LIMIT + 1;
    for i in 0..total {
        dispatch_tool(&state, "palace_create", json!({"name": format!("p{i:03}")}))
            .await
            .expect("palace_create");
    }

    let payload = dispatch_tool(&state, "room_list", json!({}))
        .await
        .expect("room_list");
    let rows = index_rows(&payload, "room_list");
    assert_eq!(rows.len(), total, "every palace must be listed");
    let detailed = rows.iter().filter(|r| r["drawer_count"].is_u64()).count();
    let omitted = rows.iter().filter(|r| r["detail"] == "omitted").count();
    assert_eq!(detailed, PALACE_INDEX_DETAIL_LIMIT);
    assert_eq!(omitted, total - PALACE_INDEX_DETAIL_LIMIT);
}

/// The read tools' schemas stop requiring `palace` (#6318).
///
/// Why: a compliant MCP client reads `required` and will not let a caller omit
/// an argument listed there. Leaving `palace` required would make the index
/// unreachable for exactly the clients it exists to serve.
/// What: asserts `palace` is absent from `required` for every read tool in
/// [`READ_TOOLS`], in BOTH schema variants.
/// Test: this function.
#[test]
fn read_tool_schemas_never_require_palace() {
    for defs in [tool_definitions_with(false), tool_definitions_with(true)] {
        let tools = defs["tools"].as_array().expect("tools array");
        for name in READ_TOOLS {
            let tool = tools
                .iter()
                .find(|t| t["name"] == *name)
                .unwrap_or_else(|| panic!("{name} missing from tools/list"));
            let required: Vec<&str> = tool["inputSchema"]["required"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            assert!(
                !required.contains(&"palace"),
                "{name} still requires palace: {required:?}"
            );
        }
    }
}
