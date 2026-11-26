//! Dispatcher-level test for creating a palace under a live alias name (#9544).
//!
//! Why: the registry refuses to shadow a live alias; this pins that the
//! `palace_create` MCP tool turns that into an `exists` answer (#11823).
//! What: drives `dispatch_tool` against a tempdir-rooted `AppState`.
//! Test: this IS the test module.

use super::*;

/// Why (#9544, #11823): creating a palace under a live alias name would
/// shadow the alias for good; since #11823 the create answers `exists` with
/// the alias target instead of an error, and still writes nothing.
/// What: creates a palace, aliases a second name to it, then calls
/// `palace_create` for the alias name. Asserts `status: exists`,
/// `alias_of` names the target, and no `palace.json` exists for the alias.
/// Test: this function.
#[tokio::test]
async fn palace_create_tool_answers_exists_for_a_live_alias_name() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "canon-target"}))
        .await
        .expect("palace_create target");
    trusty_common::palace_alias::PalaceAliasStore::register_alias(
        &state.data_root,
        "canon-alias",
        "canon-target",
    )
    .expect("register_alias");

    let out = dispatch_tool(&state, "palace_create", json!({"name": "canon-alias"}))
        .await
        .expect("a live alias name answers exists");
    assert_eq!(out["status"], "exists", "{out}");
    assert_eq!(out["alias_of"], "canon-target", "{out}");
    assert!(
        !state
            .data_root
            .join("canon-alias")
            .join("palace.json")
            .exists(),
        "no palace.json may be written for the alias"
    );
}
