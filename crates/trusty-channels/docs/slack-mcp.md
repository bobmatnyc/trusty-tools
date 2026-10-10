# slack-mcp tools and OAuth scopes

Moved from the crate README (#8454). `slack-mcp` builds only with the
`unrouted-slack-mcp` feature; it has no route check.

22 chat-as-tools operations, all live. Schemas are authoritative — see
[`src/slack/tools.rs`](../src/slack/tools.rs) and
[`src/slack/tool_schemas_canvas.rs`](../src/slack/tool_schemas_canvas.rs); the
`TOOL_NAMES` constant in the former is the complete list.

| Tool | Purpose | Required OAuth scope(s) |
|------|---------|--------------------------|
| `slack_send_message`         | Post a message (or thread reply) to a channel | `chat:write` |
| `slack_read_channel`         | Read recent messages from a channel, cursor-paginated | `channels:history` (+ `groups:`/`im:`/`mpim:history`) |
| `slack_read_thread`          | Read all replies in a thread, cursor-paginated | same as `slack_read_channel` |
| `slack_list_channels`        | List visible channels | `channels:read` (+ `groups:`/`im:`/`mpim:read`) |
| `slack_search_messages`      | Search messages across the workspace, with a `scope` param for public-only vs. public+private | `search:read` (**user token**) |
| `slack_search_channels`      | Search channels by name/topic (client-side filter — no server-side channel search) | `channels:read` |
| `slack_list_users`           | List workspace users | `users:read` |
| `slack_get_user`             | Fetch a single user's profile | `users:read` |
| `slack_add_reaction`         | Add an emoji reaction to a message | `reactions:write` |
| `slack_get_reactions`        | Read the reactions on a message | `reactions:read` |
| `slack_schedule_message`     | Schedule a message for future delivery | `chat:write` |
| `slack_create_conversation`  | Create a new channel | `channels:manage` (public) / `groups:write` (private) |
| `slack_list_channel_members` | List a channel's member IDs, cursor-paginated | `channels:read` (+ `groups:`/`im:`/`mpim:read`) |
| `slack_create_canvas`        | Create a canvas, optionally tabbed into a channel | **`canvases:write`** (+ `channels:read`*) |
| `slack_update_canvas`        | Replace a canvas's document content | **`canvases:write`** |
| `slack_read_canvas`          | Read a canvas's exported content (HTML, not markdown — see below) | **`canvases:read`**, `files:read` |
| `slack_read_file`            | Read a file's metadata and (text files only) content | `files:read` |
| `slack_search_emojis`        | Search custom emoji by name (client-side filter — no `emoji.search`) | `emoji:read` |
| `slack_search_users`         | Search users by name/real name/email (client-side filter — no non-admin `users.search`) | `users:read` |
| `slack_canvas_create`        | Create a canvas seeded with markdown; with `channel_id` it becomes the channel's canvas and members inherit edit access | **`canvases:write`** (+ `channels:read`*) |
| `slack_canvas_lookup_sections` | Look up section ids within a canvas, filtered by heading type and/or contained text; returns ids only, never content | **`canvases:read`** |
| `slack_canvas_push`          | Translate CommonMark to canvas markdown and append it, or replace the canvas's header-delimited sections (non-atomic) | **`canvases:write`** (+ **`canvases:read`** for `replace_all`) |

**Bold** scopes (`canvases:read`, `canvases:write`) are new as of epic #3611 —
a Slack app that only had the original nine tools' scopes will need these
added before `slack_create_canvas` / `slack_update_canvas` / `slack_read_canvas`
will work; other tools reuse scopes the original nine already needed.

**\* `channels:read` (issue #5161):** when a repeat `slack_create_canvas` /
`slack_canvas_create` against a channel that already has one fails with
`channel_canvas_already_exists`, the tool follows up with a
`conversations.info` call to name the existing canvas's file id in the error
(`channels:read` for a public channel; `groups:`/`im:`/`mpim:read` for a
private channel/DM/group DM). This is a courtesy on the failure path, not a
hard requirement — without the scope the lookup itself fails and the error
degrades to the bare `channel_canvas_already_exists` slug, same as before.

**Not implemented:** `slack_send_message_draft` — Slack has no public API to
create an editable message draft (`chat.postMessage` sends immediately,
`chat.scheduleMessage` schedules a send; neither creates something the user
can edit before sending). See issue #3616.

**`slack_read_canvas` caveat:** Slack does not document a `canvases.read` /
full-content-read method. This tool works around that by treating the
canvas's `canvas_id` as a Slack file id (`files.info` + a private-file
download), which returns Slack's HTML export of the canvas — **not** the
original markdown source. There is currently no API to recover the editable
markdown.

