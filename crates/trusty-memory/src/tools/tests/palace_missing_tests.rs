//! MCP tool surface of the palace-not-found contract (#9751).
//!
//! Why: `memory_remember` and `memory_note` against a palace that does not
//! exist answered `open palace X: load palace metadata for X: palace metadata
//! missing at <root>/X/palace.json` — the daemon's own filesystem path, and no
//! hint of which palaces do exist. Thirteen writes failed that way in 72 h.
//! What: drives both write tools through the JSON-RPC `tools/call` envelope an
//! MCP client sends, and asserts the not-found code, the missing id, the capped
//! list of existing palace ids with its count, and the absence of the root.
//! Test: this IS the test module.

use super::*;
use crate::engine::palace_not_found_tests::{
    assert_listed_absence, assert_unlisted_absence, seed_listed_palaces, seed_unlistable_palace,
};
use crate::transport::rpc::{JsonRpcRequest, dispatch, error_codes};

/// Send one MCP `tools/call` and return the error it must answer with.
async fn tool_call_error(
    state: &AppState,
    tool: &str,
    arguments: serde_json::Value,
) -> (i32, String) {
    let req = JsonRpcRequest {
        jsonrpc: Some("2.0".to_string()),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({"name": tool, "arguments": arguments})),
    };
    let err = dispatch(state, req)
        .await
        .error
        .unwrap_or_else(|| panic!("{tool} on a missing palace must fail"));
    (err.code, err.message)
}

/// The two write tools the issue names, aimed at `palace`.
fn write_calls(palace: &str) -> [(&'static str, serde_json::Value); 2] {
    [
        (
            "memory_remember",
            json!({"palace": palace, "text": "a synthetic fact for the absence test", "force": true}),
        ),
        (
            "memory_note",
            json!({"palace": palace, "text": "a synthetic note for the absence test"}),
        ),
    ]
}

/// Why (#9751): the acceptance case. 25 palaces exist, so the list is capped
/// at 20 and the count says so; the root path must not appear.
/// Test: itself.
#[tokio::test]
async fn memory_remember_and_note_name_a_missing_palace_without_its_path() {
    let (state, _tmp) = test_state();
    seed_listed_palaces(&state.data_root, 25);
    for (tool, arguments) in write_calls("ghost-palace") {
        let (code, message) = tool_call_error(&state, tool, arguments).await;
        assert_eq!(code, error_codes::NOT_FOUND, "{tool}: {message}");
        assert_listed_absence(&message, &state.data_root, "ghost-palace", 25);
        assert!(
            !state.data_root.join("ghost-palace/palace.json").exists(),
            "{tool}: a missing palace must stay missing (no auto-create)"
        );
    }
}

/// Why (#9751 fail-open check): when the existing palaces cannot be listed, the
/// answer is still the not-found error — same code, still no path — with the
/// list omitted, never a success and never a different code.
/// Test: itself.
#[tokio::test]
async fn memory_remember_and_note_stay_not_found_when_listing_fails() {
    let (state, _tmp) = test_state();
    seed_listed_palaces(&state.data_root, 2);
    seed_unlistable_palace(&state.data_root);
    for (tool, arguments) in write_calls("ghost-palace") {
        let (code, message) = tool_call_error(&state, tool, arguments).await;
        assert_eq!(code, error_codes::NOT_FOUND, "{tool}: {message}");
        assert_unlisted_absence(&message, &state.data_root, "ghost-palace");
    }
}

/// Why (#9751): `memory_recall` runs on every prompt. A not-found there must
/// not walk and decode every `palace.json`; the undecodable palace below would
/// turn a walk into "could not be listed", and a walk would list the others.
/// What: 25 listable palaces plus one undecodable; the read tool answers the
/// not-found code with the bare message: the id, no list, no path.
/// Test: itself.
#[tokio::test]
async fn memory_recall_names_a_missing_palace_without_walking_the_palaces() {
    let (state, _tmp) = test_state();
    seed_listed_palaces(&state.data_root, 25);
    seed_unlistable_palace(&state.data_root);
    let (code, message) = tool_call_error(
        &state,
        "memory_recall",
        json!({"palace": "ghost-palace", "query": "anything"}),
    )
    .await;
    assert_eq!(code, error_codes::NOT_FOUND, "{message}");
    assert!(
        message.contains("ghost-palace"),
        "must name the id: {message}"
    );
    assert!(
        !message.contains("existing palaces") && !message.contains("could not be listed"),
        "#9751: a read-path not-found must not list the palaces: {message}"
    );
    crate::engine::palace_not_found_tests::assert_no_path(&message, &state.data_root);
}
