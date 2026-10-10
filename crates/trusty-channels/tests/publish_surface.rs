//! Which binaries a build of trusty-channels produces (#8454 S2c, ruling Q7).
//!
//! Why: `slack-mcp` posts, reacts and writes canvases with no route check, so
//! the default build (and the published crate) must not produce it.
//! What: Cargo sets `CARGO_BIN_EXE_<name>` at compile time for every binary
//! it builds alongside this test target, and leaves it unset for a binary
//! whose `required-features` are off. These tests read that variable.
//! Test: this file.

/// The default build does not produce the unrouted `slack-mcp` binary.
#[cfg(not(feature = "unrouted-slack-mcp"))]
#[test]
fn default_build_does_not_produce_slack_mcp() {
    assert_eq!(
        option_env!("CARGO_BIN_EXE_slack-mcp"),
        None,
        "slack-mcp sends without a route check and must need the \
         `unrouted-slack-mcp` feature (#8454 Q7)"
    );
}

/// The opt-in feature still builds `slack-mcp`, so operators keep it.
#[cfg(feature = "unrouted-slack-mcp")]
#[test]
fn unrouted_feature_produces_slack_mcp() {
    assert!(option_env!("CARGO_BIN_EXE_slack-mcp").is_some());
}

/// The route-checked and non-sending binaries stay in the default build.
#[test]
fn default_build_keeps_gchat_mcp_and_telegram_mcp() {
    assert!(option_env!("CARGO_BIN_EXE_gchat-mcp").is_some());
    assert!(option_env!("CARGO_BIN_EXE_telegram-mcp").is_some());
}
