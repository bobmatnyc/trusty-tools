//! The channel MCP tool descriptor: `ask_owner` (#9856, #8433 P4).
//!
//! Why: the Architect asks its owner a question over Slack through the
//! daemon, which holds the bot token and the route policy.
//! What: [`channels_tools`] returns the one `ask_owner` descriptor with a
//! closed schema. No argument names the asker: the daemon proves it.
//! Test: `super::tests::ask_owner_tool_schema_is_closed`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the one channel tool descriptor.
///
/// What: `ask_owner`, two required string arguments, closed schema.
/// Test: `super::tests::ask_owner_tool_schema_is_closed`.
pub(super) fn channels_tools() -> Vec<Value> {
    vec![tool(
        "ask_owner",
        "Ask the owner one question as a Slack DM, through the daemon's bot. \
         Only the live managed Architect session may ask: the daemon proves \
         the caller from the calling process, and refuses everyone else. The \
         recipient must be a Slack user id (`U…`) that a channel route names; \
         a channel or group-DM id is refused. Returns `{ question_id, route, \
         dm_channel_id, thread_ts }` once the question is posted and recorded; \
         an answer binds to `question_id`. Every refusal sends nothing and \
         names its rule (`asker_unproven`, `recipient_not_user_id`, \
         `no_route`, `snapshot_stale`, `user_token_refused`, \
         `table_unavailable`, ...). `send_outcome_unknown` means the question \
         may have been posted; its id is never reused.",
        json!({
            "type": "object",
            "properties": {
                "recipient": {
                    "type": "string",
                    "description": "The Slack user id (U…) a channel route names."
                },
                "question": {
                    "type": "string",
                    "description": "The question text, 1 to 4000 bytes."
                }
            },
            "required": ["recipient", "question"],
            "additionalProperties": false
        }),
    )]
}
