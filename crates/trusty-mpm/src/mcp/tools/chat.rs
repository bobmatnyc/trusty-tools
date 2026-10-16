//! The chat MCP tool descriptor: `sessions_chat` (#9979, sub-issue of #6288).
//!
//! Why: the console's mpm chat page reaches the daemon only through this MCP
//! surface once the daemon's TCP bind retires (D96). `mpm.sessions.chat`
//! already serves the chat turn on the socket; this tool exposes the same turn.
//! What: `chat_tools` returns the one `sessions_chat` descriptor. Its
//! arguments are the `mpm.sessions.chat` request fields, and its result is
//! that method's response unchanged.
//! Test: `super::tests::sessions_chat_tool_mirrors_the_chat_request`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the one chat tool descriptor.
///
/// What: `sessions_chat`, `message` required, closed schema.
/// Test: `super::tests::sessions_chat_tool_mirrors_the_chat_request`.
pub(super) fn chat_tools() -> Vec<Value> {
    vec![tool(
        "sessions_chat",
        "Send one chat turn to the session coordinator, the same turn as the \
         daemon's `mpm.sessions.chat` method. A `@<session>: <command>` \
         message is sent to that session's pane; any other message is answered \
         by the Session Manager, or by the legacy LLM chat when the Session \
         Manager is off. Returns `{ reply, conv_id, actions_taken, cost }` \
         (plus `routed_to_session` and `command_output` for a routed command); \
         absent fields are omitted. Pass the returned `conv_id` to continue \
         the same conversation. `actions: true` lets the Session Manager run \
         session verbs inside the turn, which can take minutes: call this tool \
         with a per-call timeout longer than your client's default. With no \
         model configured the call fails with a `503` error text.",
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The user's message text."
                },
                "conv_id": {
                    "type": "string",
                    "description": "A conversation id a previous turn returned."
                },
                "actions": {
                    "type": "boolean",
                    "description": "Let the Session Manager run session verbs in this turn."
                },
                "history": {
                    "type": "array",
                    "description": "Prior turns, oldest first, for the legacy LLM chat.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["role", "content"]
                    }
                }
            },
            "required": ["message"],
            "additionalProperties": false
        }),
    )]
}
