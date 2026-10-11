//! Dispatcher-level test for creating a palace under a live alias name (#9544).
//!
//! Why: the refusal lives in the registry and its wire code in `rpc`; this
//! pins that the `palace_create` MCP tool keeps the typed `LiveAliasError` in
//! the error it returns, which is what both transports downcast.
//! What: drives `dispatch_tool` against a tempdir-rooted `AppState`.
//! Test: this IS the test module.

use super::*;

/// Why (#9544): creating a palace under a live alias name shadows the alias
/// for good, splitting the name off the palace it pointed at.
/// What: creates a palace, aliases a second name to it, then calls
/// `palace_create` for the alias name. Asserts the error downcasts to
/// `LiveAliasError` and that no `palace.json` exists for the alias.
/// Test: this function.
#[tokio::test]
async fn palace_create_tool_refuses_a_live_alias_name() {
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

    let err = dispatch_tool(&state, "palace_create", json!({"name": "canon-alias"}))
        .await
        .expect_err("a live alias name must be refused");
    let live = err
        .downcast_ref::<trusty_common::palace_alias::LiveAliasError>()
        .unwrap_or_else(|| panic!("expected LiveAliasError, got {err:#}"));
    assert_eq!(live.target, "canon-target");
    assert!(
        !state
            .data_root
            .join("canon-alias")
            .join("palace.json")
            .exists(),
        "no palace.json may be written for the alias"
    );
}
