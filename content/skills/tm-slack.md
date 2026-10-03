---
name: tm-slack
description: Deliver messages, canvases, and files to the user via Slack — routes through the native slack-mcp connector, not claude.ai's hosted Slack connector; canvas creation alone is never delivery
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [slack, delivery, canvas, messaging, pm-required]
effort: medium
---

# tm Slack Delivery Protocol

## Routing: Native `slack-mcp` First

Two Slack tool families can be present in a session: this workspace's native
`mcp__slack-mcp__*` server (`crates/trusty-channels`, `trusty-channels/src/bin/slack-mcp.rs`)
and claude.ai's hosted `mcp__claude_ai_Slack__*` connector. Prefer the native
server whenever it's registered — check the tool listing before assuming
either is present. This is the workspace's stated native-first routing
preference; see `docs/adr/0014-native-mcp-support.md` for the rationale and
scope, and do not restate it here beyond this pointer.

The two surfaces are NOT identical. Differences that matter when picking a
tool name:

- The native server has no `slack_send_message_draft`. Slack's API has no
  endpoint that creates an editable draft (`chat.postMessage` sends,
  `chat.scheduleMessage` schedules — neither is a draft); this is a
  deliberate omission, not a gap to work around. Use `slack_schedule_message`
  for "send later," or just send.
- The native server has no `slack_read_user_profile`, `slack_search_public`,
  or `slack_search_public_and_private` — use `slack_get_user` and
  `slack_search_messages` (with its `scope` parameter) instead.
- The native server has three tools the connector doesn't: `slack_canvas_create`,
  `slack_canvas_lookup_sections`, `slack_canvas_push` — the preferred canvas
  path (see below).

## The Native Tool Surface

Verified against `crates/trusty-channels/src/slack/tools.rs` and
`tool_schemas_canvas.rs` — the 22 tools in `TOOL_NAMES` are the entire native
surface; do not invent others.

| Category | Tools |
|---|---|
| Send / schedule | `slack_send_message`, `slack_schedule_message` |
| Read | `slack_read_channel`, `slack_read_thread`, `slack_read_file` |
| Discover | `slack_list_channels`, `slack_search_channels`, `slack_list_users`, `slack_get_user`, `slack_search_users`, `slack_search_messages`, `slack_search_emojis`, `slack_list_channel_members` |
| React | `slack_add_reaction`, `slack_get_reactions` |
| Manage | `slack_create_conversation` |
| Canvas | `slack_create_canvas`, `slack_update_canvas`, `slack_read_canvas`, `slack_canvas_create`, `slack_canvas_lookup_sections`, `slack_canvas_push` |

Load full schemas with `ToolSearch(query: "select:mcp__slack-mcp__slack_send_message,mcp__slack-mcp__slack_canvas_create,...")`
before calling — see `tm-tool-usage-guide`'s deferred-tool-loading section.

## The Completion Rule

**A tool call that creates or schedules content is not delivery.** Delivery
is a message landing in a destination the user actually watches, and you
have proof of it (a `slack_send_message` response with a `ts`, or an
equivalent success payload). This applies to every delivery shape below —
it is not a canvas-specific rule. Never mark a delivery task done on a
create/schedule call's response alone.

## Delivering a Plain Message

1. Resolve the destination (see below).
2. Call `slack_send_message` with `channel` + `text`, and `thread_ts` if
   replying in-thread.
3. The response's `ts` is the delivery receipt. That's it — no canvas
   involved, no second call needed.

## Delivering a Canvas

Canvas creation alone is not delivery — the failure this section exists to
prevent is treating a `canvas_id` in a create response as proof the user has
the document.

1. **Resolve the destination first.** Reuse the triggering Slack
   conversation's channel/thread when known. Otherwise resolve the user's ID
   (`slack_search_users`, then DM using the user_id as the channel) or the
   named project channel (`slack_search_channels` / `slack_list_channels`).
   Ask once if genuinely ambiguous — never invent a channel silently.

2. **Create the canvas with `slack_canvas_create`.** It's the preferred
   native tool over its `slack_create_canvas` clone (same shape, kept only
   for connector parity). **Always pass `channel_id`, even though the schema
   marks it optional** — a free-tier (non-Business+) workspace hard-rejects a
   non-tabbed canvas (`free_teams_cannot_create_non_tabbed_canvases`), so the
   parameter being schema-optional does not make it workspace-optional.
   `slack_canvas_create` requires `markdown` (initial content); pass it
   directly rather than creating empty and pushing separately when you
   already have the content.

3. **The response is `{ok, canvas_id}` only — no permalink.** Slack's API
   does not return one on create. Reference the channel tab itself ("added a
   canvas to #channel: title") in your delivery message, or capture a
   `canvas_url` if a later push/update call happens to surface one.

4. **To edit an existing canvas** rather than creating a new one, use
   `slack_canvas_push` — `mode: "append"` for a single insert-at-end edit, or
   `mode: "replace_all"` to replace header-delimited sections (h1/h2/h3).
   `replace_all` is **not atomic**: it deletes existing sections one at a
   time then inserts new content, so a mid-operation failure can leave the
   canvas partially cleared. It falls back to appending (with a warning) if
   the canvas has no header-delimited sections to clear. Use
   `slack_canvas_lookup_sections` first if you need to target or verify
   specific sections rather than the whole document.

5. **Deliver.** Call `slack_send_message` into the resolved destination with
   the canvas title and a reference to it. This call's success — not the
   canvas creation or push response — is the completion signal.

6. **Verify.** Confirm the `slack_send_message` call actually happened this
   turn and returned success, not just that it was planned.

## Resolving a Destination

- Known channel/thread from the triggering context → reuse it.
- Named user → `slack_search_users`, then DM via the returned user ID as
  `channel`.
- Named channel → `slack_search_channels` (name/topic match) or
  `slack_list_channels` (browse, supports a `types` filter).
- New channel needed → `slack_create_conversation` (requires
  `channels:manage` / `groups:write`).
- Genuinely ambiguous → ask once. Never guess a destination silently.

## Failure Checks Before Declaring Delivered

- The create/schedule/push call actually succeeded — watch for
  `free_teams_cannot_create_non_tabbed_canvases`, `canvas_creation_failed`,
  `canvas_disabled_user_team`, `missing_scope`, and (for `replace_all`)
  `canvas_editing_locked` (retried automatically a bounded number of times,
  but can still exhaust retries).
- A `slack_send_message` (or equivalent final-delivery) call was actually
  made this turn, not just planned.
- The destination matches where the user watches, not just any channel that
  happened to resolve.

## Fallback When Slack Delivery Isn't Available

Never silently drop the deliverable. If only the canvas-specific call
failed and Slack itself is reachable — the most common case is a free-tier
workspace rejecting canvas creation while `slack_send_message` still works
fine — degrade to a plain Slack message with the content inline before
escalating further. Only when no Slack tool is registered, or Slack itself
is unreachable, post the content as an Artifact or another available
channel and hand back that link instead. Degrade the medium, never drop the
payload.

## Related Skills

- `internal-comms` — writing the digest/report content that gets delivered
- `tm-bug-reporting` — the sibling pattern of an MCP-native tool with a
  documented fallback path
- `tm-tool-usage-guide` — deferred MCP tool loading mechanics
