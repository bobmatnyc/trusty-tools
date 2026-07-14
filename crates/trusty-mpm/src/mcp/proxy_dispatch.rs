//! Argument parsing + routing for the four session-manager PROXY MCP tools
//! (#2550, a #1440 follow-up).
//!
//! Why: the proxy tools (`session_proxy_focus`, `session_proxy_unfocus`,
//! `session_proxy_message`, `session_proxy_summary`) expose the
//! [`crate::client::proxy::SessionProxy`] focus/inject/summarize state machine
//! over MCP. Routing them in a sibling module — mirroring `session_dispatch` and
//! `project_dispatch` — keeps `mcp/mod.rs`'s `dispatch_tool_call` small and under
//! the 500-SLOC production cap, and gives the proxy tools one auditable home.
//! What: [`try_dispatch`] matches a tool name against the four proxy tools; on a
//! match it parses arguments (via the shared `required_str` helper) and calls the
//! corresponding [`super::OrchestratorBackend`] method, returning `Some(result)`.
//! A non-proxy tool name returns `None` so the caller can report "unknown tool".
//! Every tool takes an explicit `conversation_key` — the MCP layer threads no
//! per-connection identity through `dispatch`, so the caller names the
//! conversation exactly as the HTTP proxy routes' `conversation_key` body field
//! does (see the trait doc in `super`).
//! Test: the `super::tests` `dispatch_session_proxy_*` cases drive this module
//! through the public `dispatch` entry point with a mock backend.

use serde_json::Value;

use super::{OrchestratorBackend, required_str};

/// Route a session-manager proxy tool call to the backend.
///
/// Why: a single entry point lets `dispatch_tool_call` delegate every proxy-tool
/// name in one arm, keeping the core dispatch match small.
/// What: returns `Some(Result)` for the four proxy tool names — parsing args and
/// calling the matching backend method — or `None` when `name` is not a proxy
/// tool, signalling the caller to fall through to its "unknown tool" branch.
/// `session_proxy_focus` reads an OPTIONAL `session_id` (empty → query the
/// current focus without setting one), mirroring the HTTP route's default;
/// arg-parsing errors surface as `Some(Err(_))`, identical to the other tool
/// groups.
/// Test: exercised by every `dispatch_session_proxy_*` test in `super::tests`.
pub async fn try_dispatch<B: OrchestratorBackend>(
    backend: &B,
    name: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    let result = match name {
        "session_proxy_focus" => match required_str(args, "conversation_key") {
            Ok(conv) => {
                // Optional: an absent/empty session_id queries the current focus
                // without touching the backend (matches the HTTP route default).
                let session_id = args.get("session_id").and_then(Value::as_str).unwrap_or("");
                backend.session_proxy_focus(&conv, session_id).await
            }
            Err(e) => Err(e),
        },
        "session_proxy_unfocus" => match required_str(args, "conversation_key") {
            Ok(conv) => backend.session_proxy_unfocus(&conv).await,
            Err(e) => Err(e),
        },
        "session_proxy_message" => {
            match (
                required_str(args, "conversation_key"),
                required_str(args, "text"),
            ) {
                (Ok(conv), Ok(text)) => backend.session_proxy_message(&conv, &text).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
        "session_proxy_summary" => match required_str(args, "conversation_key") {
            Ok(conv) => backend.session_proxy_summary(&conv).await,
            Err(e) => Err(e),
        },
        // Not a proxy tool — let the caller report "unknown tool".
        _ => return None,
    };
    Some(result)
}
