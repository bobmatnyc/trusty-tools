//! MCP `tools/list` response — JSON Schema for every live Slack tool.
//!
//! Why: Claude Code (and any MCP client) needs a machine-readable contract
//! describing each tool's arguments so the model can fill them correctly. This
//! is the single source of truth for the Slack MCP surface; the dispatcher in
//! `server` routes on the same names. All nine original tools have live
//! handlers (issues #2639 + #2640, see ADR-0014); epic #3611 adds ten more for
//! parity with the claude.ai Slack connector (issues #3612-#3618).
//! What: `tool_list_response()` returns `{"tools": [{name, description,
//! inputSchema}, ...]}`; `is_known_tool()` reports whether a name is part of
//! the planned surface so the dispatcher can distinguish "not yet implemented"
//! from "unknown tool".
//! Test: Unit tests below assert the tool count, required-field shape, name
//! uniqueness, and `is_known_tool` agreement with the registry.

use serde_json::{json, Value};

/// The authoritative list of live Slack tool names.
///
/// Why: Both `tool_list_response` (indirectly) and `is_known_tool` must agree
/// on the exact surface; a single constant array prevents drift.
/// What: The 21 chat-as-tools operations implemented by the native Slack MCP —
/// the original nine (#2639/#2640), ten added for claude.ai Slack-connector
/// parity (epic #3611), and two more from epic #3744 slice 1's
/// `slack_canvas_*` tools. `slack_send_message_draft` (claude.ai's 20th tool)
/// is deliberately NOT included: Slack has no public API to create a message
/// draft (`chat.postMessage`/`chat.scheduleMessage` send or schedule; neither
/// creates an editable draft) — see the crate README and PR #3616.
/// Test: `known_tools_match_registry` cross-checks this against the built list.
pub const TOOL_NAMES: &[&str] = &[
    "slack_send_message",
    "slack_read_channel",
    "slack_read_thread",
    "slack_list_channels",
    "slack_search_messages",
    "slack_search_channels",
    "slack_list_users",
    "slack_get_user",
    "slack_add_reaction",
    "slack_get_reactions",
    "slack_schedule_message",
    "slack_create_conversation",
    "slack_list_channel_members",
    "slack_create_canvas",
    "slack_update_canvas",
    "slack_read_canvas",
    "slack_read_file",
    "slack_search_emojis",
    "slack_search_users",
    "slack_canvas_create",
    "slack_canvas_lookup_sections",
];

/// Report whether `name` is a planned Slack tool.
///
/// Why: The dispatcher returns a distinct MCP error for an unknown tool vs a
/// known-but-unimplemented one; this predicate draws that line.
/// What: Linear membership check against `TOOL_NAMES`.
/// Test: `known_tools_match_registry`.
pub fn is_known_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

/// Assemble a single MCP tool definition.
///
/// Why: The `tools/list` response requires a uniform `{name, description,
/// inputSchema}` object per tool; centralising avoids drift.
/// What: Wraps `properties` and `required` into an object-typed input schema
/// alongside the name and description.
/// Test: Covered via `tool_list_response()` in `tests`.
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

/// A `channel` id/name string-schema fragment shared by several tools.
fn channel_prop() -> Value {
    json!({
        "type": "string",
        "description": "Channel ID (e.g. C0123ABCD) or name (e.g. #general).",
    })
}

/// Build the full `tools/list` response.
///
/// Why: One function = one source of truth for the MCP contract.
/// What: Returns the nine planned Slack tools with minimal-but-valid schemas.
/// Test: `tool_list_has_expected_count` asserts exactly nine tools.
pub fn tool_list_response() -> Value {
    let tools = vec![
        tool(
            "slack_send_message",
            "Post a message to a Slack channel (or reply in a thread).",
            json!({
                "channel": channel_prop(),
                "text": { "type": "string", "description": "Message text to send." },
                "thread_ts": {
                    "type": "string",
                    "description": "Optional parent message ts to reply in-thread.",
                },
            }),
            &["channel", "text"],
        ),
        tool(
            "slack_read_channel",
            "Read messages from a Slack channel, newest-first. Supports cursor \
             pagination: pass the previous call's `next_cursor` back as `cursor` \
             to walk further into history, or bound the window with `oldest`/ \
             `latest`. `next_cursor` is null once there are no more pages.",
            json!({
                "channel": channel_prop(),
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of messages to return per page \
                        (default 50, max 999).",
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque pagination cursor from a previous \
                        call's `next_cursor`; fetches the next page.",
                },
                "oldest": {
                    "type": "string",
                    "description": "Only messages after this Slack ts \
                        (seconds.microseconds, e.g. '1616703000.216300').",
                },
                "latest": {
                    "type": "string",
                    "description": "Only messages before this Slack ts \
                        (seconds.microseconds).",
                },
            }),
            &["channel"],
        ),
        tool(
            "slack_read_thread",
            "Read replies in a Slack thread. Supports cursor pagination for long \
             threads: pass the previous call's `next_cursor` back as `cursor` to \
             fetch the next page, or bound the window with `oldest`/`latest`; \
             `next_cursor` is null once there are no more.",
            json!({
                "channel": channel_prop(),
                "thread_ts": {
                    "type": "string",
                    "description": "The ts of the thread's parent message, as an \
                        exact Slack timestamp string 'seconds.microseconds' \
                        (e.g. '1616703000.216300'). Pass it through unchanged — \
                        reformatting it as a number can silently drop trailing- \
                        zero precision and Slack will reject the call.",
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of replies to return per page \
                        (default 50, max 999).",
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque pagination cursor from a previous \
                        call's `next_cursor`; fetches the next page.",
                },
                "oldest": {
                    "type": "string",
                    "description": "Only replies after this Slack ts \
                        (seconds.microseconds, e.g. '1616703000.216300').",
                },
                "latest": {
                    "type": "string",
                    "description": "Only replies before this Slack ts \
                        (seconds.microseconds).",
                },
            }),
            &["channel", "thread_ts"],
        ),
        tool(
            "slack_list_channels",
            "List channels visible to the authenticated principal.",
            json!({
                "types": {
                    "type": "string",
                    "description": "Comma-separated channel types (e.g. public_channel,private_channel).",
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of channels to return.",
                },
            }),
            &[],
        ),
        tool(
            "slack_search_messages",
            "Search messages across the workspace. Requires a Slack user token \
             (SLACK_USER_TOKEN); a bot token cannot call search.",
            json!({
                "query": { "type": "string", "description": "Slack search query string." },
                "count": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 20).",
                },
                "scope": {
                    "type": "string",
                    "enum": ["public", "public_and_private"],
                    "description": "'public' returns only matches from channels known to be \
                        public; 'public_and_private' (default) returns every match the user \
                        token can see, unfiltered. Slack's search API has no server-side \
                        public/private filter, so 'public' is applied client-side after the \
                        search — a match whose channel privacy Slack didn't report is treated \
                        as private (excluded) rather than assumed public.",
                },
            }),
            &["query"],
        ),
        tool(
            "slack_search_channels",
            "Search channels by name or topic. Scans a single page of up to 200 \
             channels client-side (no cursor pagination); a large workspace may \
             truncate.",
            json!({
                "query": { "type": "string", "description": "Channel search query string." },
            }),
            &["query"],
        ),
        tool(
            "slack_list_users",
            "List users in the workspace.",
            json!({
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of users to return.",
                },
            }),
            &[],
        ),
        tool(
            "slack_get_user",
            "Fetch a single user's profile by ID.",
            json!({
                "user": { "type": "string", "description": "Slack user ID (e.g. U0123ABCD)." },
            }),
            &["user"],
        ),
        tool(
            "slack_add_reaction",
            "Add an emoji reaction to a message.",
            json!({
                "channel": channel_prop(),
                "timestamp": {
                    "type": "string",
                    "description": "The ts of the message to react to.",
                },
                "name": {
                    "type": "string",
                    "description": "Emoji name without colons (e.g. thumbsup).",
                },
            }),
            &["channel", "timestamp", "name"],
        ),
        tool(
            "slack_get_reactions",
            "Read the emoji reactions on a message. Requires scope reactions:read.",
            json!({
                "channel": channel_prop(),
                "timestamp": {
                    "type": "string",
                    "description": "The ts of the message to read reactions from.",
                },
            }),
            &["channel", "timestamp"],
        ),
        tool(
            "slack_schedule_message",
            "Schedule a message to be posted at a future time. Requires scope chat:write.",
            json!({
                "channel": channel_prop(),
                "text": { "type": "string", "description": "Message text to send." },
                "post_at": {
                    "type": "integer",
                    "description": "Unix timestamp (seconds) of when to post the message.",
                },
                "thread_ts": {
                    "type": "string",
                    "description": "Optional parent message ts to reply in-thread.",
                },
            }),
            &["channel", "text", "post_at"],
        ),
        tool(
            "slack_create_conversation",
            "Create a new Slack channel. Requires scope channels:manage (public) / \
             groups:write (private).",
            json!({
                "name": {
                    "type": "string",
                    "description": "Channel name (lowercase letters, numbers, hyphens, \
                        underscores; max 80 characters).",
                },
                "is_private": {
                    "type": "boolean",
                    "description": "Create a private channel instead of public (default false).",
                },
            }),
            &["name"],
        ),
        tool(
            "slack_list_channel_members",
            "List a channel's member user IDs. Supports cursor pagination: pass the \
             previous call's `next_cursor` back as `cursor` to fetch more. Requires \
             scope channels:read / groups:read / im:read / mpim:read (by channel type).",
            json!({
                "channel": channel_prop(),
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of member IDs to return per page (default 100).",
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque pagination cursor from a previous call's `next_cursor`.",
                },
            }),
            &["channel"],
        ),
        tool(
            "slack_create_canvas",
            "Create a standalone canvas, optionally tabbed into a channel and/or \
             seeded with markdown content. Requires scope canvases:write.",
            json!({
                "title": { "type": "string", "description": "Optional canvas title." },
                "markdown": {
                    "type": "string",
                    "description": "Optional initial markdown content.",
                },
                "channel_id": {
                    "type": "string",
                    "description": "Optional channel ID to tab the new canvas into.",
                },
            }),
            &[],
        ),
        tool(
            "slack_update_canvas",
            "Replace a canvas's entire document content. Requires scope canvases:write.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
                "markdown": {
                    "type": "string",
                    "description": "New markdown content, replacing the canvas in full.",
                },
                "operation_id": {
                    "type": "string",
                    "description": "Optional idempotency key for the edit.",
                },
            }),
            &["canvas_id", "markdown"],
        ),
        tool(
            "slack_read_canvas",
            "Read a canvas's content. Slack has no documented full-content-read API, so \
             this downloads the canvas's private file export (HTML, not the original \
             markdown). Requires scopes canvases:read and files:read.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
            }),
            &["canvas_id"],
        ),
        tool(
            "slack_read_file",
            "Read a Slack file's metadata and, for text files, its content. Binary \
             files are reported as such (with a permalink) rather than embedded. \
             Requires scope files:read.",
            json!({
                "file": { "type": "string", "description": "Slack file ID (e.g. F0123ABCD)." },
            }),
            &["file"],
        ),
        tool(
            "slack_search_emojis",
            "Search custom workspace emoji by name. Slack has no emoji.search endpoint, \
             so this filters emoji.list locally. Requires scope emoji:read.",
            json!({
                "query": { "type": "string", "description": "Substring to match against emoji names." },
            }),
            &["query"],
        ),
        tool(
            "slack_search_users",
            "Search workspace users by name, real name, or email. Slack has no \
             non-admin users.search endpoint, so this filters users.list locally \
             (bounded scan, large workspaces may truncate). Requires scope users:read.",
            json!({
                "query": {
                    "type": "string",
                    "description": "Substring to match against name, real name, or email.",
                },
            }),
            &["query"],
        ),
        tool(
            "slack_canvas_create",
            "Create a standalone canvas, optionally tabbed into a channel and/or \
             seeded with markdown content. Thin wrapper over canvases.create — no \
             markdown-to-canvas translation beyond Slack's own markdown ingestion. \
             `channel_id` is optional per Slack's API, but free-tier (non-Business+) \
             workspaces reject a non-tabbed canvas (error \
             free_teams_cannot_create_non_tabbed_canvases), so pass `channel_id` on \
             those teams. Requires scope canvases:write; other Slack errors surfaced \
             as-is include canvas_creation_failed, canvas_disabled_user_team, and \
             missing_scope.",
            json!({
                "title": { "type": "string", "description": "Optional canvas title." },
                "markdown": {
                    "type": "string",
                    "description": "Required initial markdown content, sent to Slack as \
                        document_content: {type: \"markdown\", markdown}.",
                },
                "channel_id": {
                    "type": "string",
                    "description": "Optional channel ID to tab the new canvas into. \
                        Effectively required on free-tier Slack teams — see the tool \
                        description.",
                },
            }),
            &["markdown"],
        ),
        tool(
            "slack_canvas_lookup_sections",
            "Look up section ids/anchors within an existing canvas via \
             canvases.sections.lookup, filtered by section type and/or contained \
             text. Slack has no full-canvas-content-read API — this returns only \
             section_ids, never document content or any other raw response field; \
             pair a returned id with slack_update_canvas / a section-targeted \
             canvases.edit call to act on it. Slack's criteria reliably accepts \
             only h1/h2/h3/any_header as section_types. Requires scope canvases:read.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
                "section_types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter on section heading type. Slack \
                        reliably accepts only \"h1\", \"h2\", \"h3\", and \
                        \"any_header\" here.",
                },
                "contains_text": {
                    "type": "string",
                    "description": "Optional filter: only return sections whose \
                        content contains this text.",
                },
            }),
            &["canvas_id"],
        ),
    ];
    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tool_list_has_expected_count() {
        let v = tool_list_response();
        let tools = v["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), TOOL_NAMES.len(), "expected all planned tools");
        for t in tools {
            assert!(t["name"].is_string(), "every tool has a name");
            assert!(t["description"].is_string(), "every tool has a description");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "every tool has an object inputSchema"
            );
            assert!(
                t["inputSchema"]["required"].is_array(),
                "every tool declares a required array"
            );
        }
    }

    #[test]
    fn every_tool_name_is_unique() {
        let v = tool_list_response();
        let mut seen = HashSet::new();
        for t in v["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap().to_string();
            assert!(seen.insert(name.clone()), "duplicate tool: {name}");
        }
    }

    #[test]
    fn known_tools_match_registry() {
        let v = tool_list_response();
        let built: HashSet<&str> = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let declared: HashSet<&str> = TOOL_NAMES.iter().copied().collect();
        assert_eq!(built, declared, "TOOL_NAMES must match the built registry");
        for name in TOOL_NAMES {
            assert!(is_known_tool(name), "{name} must be a known tool");
        }
        assert!(!is_known_tool("slack_not_a_tool"));
    }

    #[test]
    fn read_thread_schema_advertises_full_pagination_shape() {
        // read::read_thread actually forwards cursor/oldest/latest to
        // conversations.replies (via apply_pagination_args, shared with
        // read_channel) — the schema must advertise every param the handler
        // accepts, not just cursor (issue #2996 review).
        let v = tool_list_response();
        let read_thread = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "slack_read_thread")
            .expect("slack_read_thread is a known tool");
        let props = &read_thread["inputSchema"]["properties"];
        for key in ["thread_ts", "limit", "cursor", "oldest", "latest"] {
            assert!(
                props.get(key).is_some(),
                "slack_read_thread schema must declare '{key}'"
            );
        }
    }

    #[test]
    fn read_channel_and_read_thread_share_the_same_pagination_property_names() {
        // Both tools are backed by the same apply_pagination_args helper, so
        // their pagination-related schema properties must match exactly.
        let v = tool_list_response();
        let props_for = |name: &str| {
            v["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap()["inputSchema"]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<HashSet<_>>()
        };
        let pagination_keys: HashSet<&str> = ["cursor", "oldest", "latest"].into_iter().collect();
        let channel_props = props_for("slack_read_channel");
        let thread_props = props_for("slack_read_thread");
        for key in pagination_keys {
            assert!(
                channel_props.contains(key) && thread_props.contains(key),
                "'{key}' must be declared on both slack_read_channel and slack_read_thread"
            );
        }
    }
}
