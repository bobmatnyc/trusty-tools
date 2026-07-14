//! Session-manager PROXY MCP tool descriptors (#2550, a #1440 follow-up).
//!
//! Why: the channel-agnostic [`crate::client::proxy::SessionProxy`]
//! focus/inject/summarize state machine was reachable only over Telegram and the
//! daemon's local HTTP routes. Exposing it as MCP tools gives an LLM caller
//! (Claude Code) the same "focus one session, then talk to it" workflow the
//! human channels have — a fourth surface over the identical state machine.
//! What: [`proxy_tools`] returns the four `{ name, description, inputSchema }`
//! descriptors — `session_proxy_focus`, `session_proxy_unfocus`,
//! `session_proxy_message`, `session_proxy_summary`. The shared [`tool`] builder
//! is re-exported from the parent module.
//! Test: `cargo test -p trusty-mpm` (in [`super`]) asserts the full catalog is
//! well-formed and that each of these names is present
//! (`super::tests::proxy_tools_present`).

use serde_json::{Value, json};

use super::tool;

/// Build the four session-manager proxy tool descriptors.
///
/// Why: focusing a conversation on one managed session and then injecting free
/// text into it / summarizing it back are the proxy operations an LLM caller
/// needs; surfacing them as MCP tools makes the proxy model drivable from Claude
/// Code without a Telegram bot or a raw HTTP call. Keeping them in their own
/// builder keeps every `tools/*.rs` file well under the 500-SLOC cap.
/// What: returns the four descriptors in catalog order. Each takes an explicit
/// `conversation_key` (the MCP layer threads no per-connection identity, so the
/// caller names the conversation — the SAME opaque key the HTTP proxy routes and
/// Telegram use, sharing one focus store); `session_proxy_focus` also takes an
/// optional `session_id`, `session_proxy_message` a required `text`. Every schema
/// sets `additionalProperties: false` so a typo yields a clear error.
/// Test: `super::tests::proxy_tools_present`,
/// `super::tests::catalog_names_match_constant`.
pub(super) fn proxy_tools() -> Vec<Value> {
    vec![
        tool(
            "session_proxy_focus",
            "Focus a conversation on ONE managed session so later \
             `session_proxy_message` calls route to it without repeating the id. \
             `conversation_key` is any opaque string you choose to identify this \
             conversation (it is shared with the Telegram/HTTP proxy surfaces — \
             the same key sees the same focus). Pass `session_id` (a managed \
             session id, friendly name, or unambiguous prefix) to SET the focus; \
             OMIT it (or pass empty) to QUERY the current focus without changing \
             it. Returns a tagged `outcome`: `focused` (set), `current` (query — \
             `target` may be null), or `not_found` (unresolved; focus unchanged).",
            json!({
                "type": "object",
                "properties": {
                    "conversation_key": {
                        "type": "string",
                        "description": "Opaque conversation identifier you choose; the focus is stored under this key and shared with the Telegram/HTTP proxy surfaces."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id, friendly name, or unambiguous prefix to focus. Omit or leave empty to QUERY the current focus instead of setting one."
                    }
                },
                "required": ["conversation_key"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_proxy_unfocus",
            "Clear a conversation's focus so free text no longer injects into any \
             session (back to fleet-wide chat). Reports the session that was \
             cleared, or null if nothing was focused. Never an error — unfocusing \
             an already-unfocused conversation is a harmless no-op.",
            json!({
                "type": "object",
                "properties": {
                    "conversation_key": {
                        "type": "string",
                        "description": "The conversation whose focus should be cleared."
                    }
                },
                "required": ["conversation_key"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_proxy_message",
            "INJECT free text into the conversation's focused session (equivalent \
             to `session_send` but addressed by focus instead of an explicit id). \
             Focus the conversation first with `session_proxy_focus`. Returns a \
             tagged `outcome`: `sent` on success; `auto_unfocused` if the focused \
             session had vanished (focus is cleared for you); `failed` on a \
             transient error (focus preserved); or `no_focus` if nothing was \
             focused — in which case handle the text yourself rather than treating \
             it as an error.",
            json!({
                "type": "object",
                "properties": {
                    "conversation_key": {
                        "type": "string",
                        "description": "The conversation the message arrived on (must have a focused session)."
                    },
                    "text": {
                        "type": "string",
                        "description": "The free-text message to inject into the focused session's pane."
                    }
                },
                "required": ["conversation_key", "text"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_proxy_summary",
            "SUMMARIZE the conversation's focused session — a lightweight digest of \
             what it is doing (lifecycle state, a short activity summary, and any \
             pending decision it is blocked on) WITHOUT attaching to tmux or \
             requiring an LLM key. Focus the conversation first with \
             `session_proxy_focus`. Returns a tagged `outcome`: `summary` on \
             success; `auto_unfocused` if the session had vanished; `failed` on a \
             transient error; or `no_focus` if nothing was focused.",
            json!({
                "type": "object",
                "properties": {
                    "conversation_key": {
                        "type": "string",
                        "description": "The conversation whose focused session to summarize."
                    }
                },
                "required": ["conversation_key"],
                "additionalProperties": false
            }),
        ),
    ]
}
