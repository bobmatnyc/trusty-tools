//! The two MCP methods `serve --stdio` answers without the daemon (#8351).
//!
//! Why: an MCP client launches this bridge once per session and never
//! re-spawns it. Before #8351 every method, `initialize` included, was
//! forwarded to the daemon's socket — so a daemon that was down for the few
//! seconds the client spent handshaking left the client with a server marked
//! failed for the rest of the session, long after the daemon was healthy
//! again. trusty-search has never had that failure mode because its stdio
//! server answers the handshake in-process and forwards only tool bodies
//! (`trusty_search::mcp::stdio::run`). This module is that behaviour for the
//! forwarding bridge, and the owner's requirement verbatim: "it should behave
//! exactly like search in this regard".
//!
//! What: [`local_answer`] claims exactly `initialize` and `tools/list` and
//! declines everything else, which [`super::serve_stdio_bridge::build_bridge`]
//! hands to [`trusty_mcp::DaemonBridgeJsonRpc::with_local_handler`]. A tool
//! CALL is never answered here — it needs the store, which this process must
//! never open (#1078) — so it is forwarded and fails visibly while the daemon
//! is down, and succeeds on the next call once it returns.
//!
//! ## Drift guard
//!
//! There is no second table to drift from. Both answers come from the same
//! functions the daemon's own router calls for these methods —
//! [`trusty_mcp::initialize_response`] and [`crate::tools::tool_definitions_with`],
//! which `transport::rpc::dispatch` calls for `initialize` and `tools/list`
//! respectively. A tool added to the schema reaches the bridge in the same
//! build that adds it, because it is the same call. Two facts are genuinely
//! this process', not the daemon's, and that is deliberate:
//!
//! - **the version** is this bridge's build, not the running daemon's. The
//!   #8351 incident could not be attributed to a binary; an answer that names
//!   the build the client is actually talking to is what makes the next one
//!   attributable.
//! - **the default palace** is this bridge's `--palace`, because this bridge is
//!   what injects it into every forwarded call
//!   (`serve_stdio_bridge::inject_default_palace`). The schema's `required`
//!   list states what the CLIENT must send, and that is governed here.
//!
//! Test: the unit tests below, plus `the_local_tool_list_is_the_daemon_table`
//! and `the_handshake_is_answered_with_no_daemon_listening` in
//! `super::serve_stdio_bridge`.

use serde_json::{json, Value};
use trusty_mcp::Request;

/// Answer `initialize` or `tools/list` here; decline everything else.
///
/// Why: see the module docs — the handshake must not depend on a daemon the
/// client cannot make the bridge retry.
/// What: `Some(result)` becomes the JSON-RPC `result` with the request's own
/// id; `None` means the request is forwarded exactly as before. `default_palace`
/// is this bridge's `--palace`, which drives both the `serverInfo` field and
/// whether the schema still requires `palace`.
/// Test: `initialize_is_answered_without_the_daemon`,
/// `tools_list_is_the_crate_table`,
/// `everything_else_is_left_to_the_daemon`.
pub(crate) fn local_answer(req: &Request, default_palace: Option<&str>) -> Option<Value> {
    match req.method.as_str() {
        // #8351: the same call the daemon's router makes, so the two answers
        // cannot disagree about anything but the version and the palace default.
        "initialize" => Some(trusty_mcp::initialize_response(
            "trusty-memory",
            env!("CARGO_PKG_VERSION"),
            default_palace.map(|p| json!({ "default_palace": p })),
        )),
        "tools/list" => Some(crate::tools::tool_definitions_with(
            default_palace.is_some(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(method: &str) -> Request {
        Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: method.to_string(),
            params: None,
        }
    }

    /// Why (#8351): the handshake is what a failed daemon used to cost the
    /// whole session, and the answer must be complete enough for a client to
    /// accept — a bare `{}` would be accepted here and rejected on the wire.
    /// What: the answer carries the protocol version and names this server,
    /// and the `--palace` default reaches `serverInfo` when one is set.
    /// Test: this test.
    #[test]
    fn initialize_is_answered_without_the_daemon() {
        let answer = local_answer(&req("initialize"), None).expect("initialize is answered here");
        assert_eq!(answer["protocolVersion"], "2024-11-05");
        assert_eq!(answer["serverInfo"]["name"], "trusty-memory");
        assert_eq!(answer["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));

        let with_palace = local_answer(&req("initialize"), Some("owner-profile"))
            .expect("initialize is answered here");
        assert_eq!(with_palace["serverInfo"]["default_palace"], "owner-profile");
    }

    /// Why: the drift guard is "one table, two callers". A test that hand-wrote
    /// the expected tool names would BE the second table.
    /// What: the answer is exactly `crate::tools::tool_definitions_with` for the
    /// matching palace state, and the two states differ — so the test cannot
    /// pass by ignoring the argument.
    /// Test: this test.
    #[test]
    fn tools_list_is_the_crate_table() {
        for has_default in [false, true] {
            let palace = has_default.then_some("owner-profile");
            let answer = local_answer(&req("tools/list"), palace).expect("tools/list is answered");
            assert_eq!(answer, crate::tools::tool_definitions_with(has_default));
        }
        assert_ne!(
            crate::tools::tool_definitions_with(false),
            crate::tools::tool_definitions_with(true),
            "the palace default must actually change the schema, or the \
             assertions above hold for the wrong reason"
        );
    }

    /// Why (#1078): a tool call needs the store, and this process must never
    /// open redb. Claiming one here would answer from nothing.
    /// What: every other method declines, including the `tools/call` envelope
    /// and a bare tool method.
    /// Test: this test.
    #[test]
    fn everything_else_is_left_to_the_daemon() {
        for method in ["tools/call", "memory_recall", "ping", "rpc.discover"] {
            assert!(
                local_answer(&req(method), Some("owner-profile")).is_none(),
                "{method} must reach the daemon"
            );
        }
    }
}
