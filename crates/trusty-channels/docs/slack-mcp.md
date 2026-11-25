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
| `slack_list_channels`        | List one page of visible channels, cursor-paginated (see below) | `channels:read` (+ `groups:`/`im:`/`mpim:read`) |
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
| `slack_canvas_access_set`    | Grant users (or regular channels) `read`/`write` access on a bot-owned canvas, so a shared link becomes editable; never grants `owner` | **`canvases:write`** |

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

**`slack_list_channels` types and paging:** with no `types` argument the tool
asks for `public_channel,private_channel`, so private channels the bot is a
member of are listed. A token without `groups:read` gets `missing_scope` for
that set; the tool then repeats the request once with `public_channel` and
returns the public listing. The result's `types` field names the set actually
listed. An explicit `types` is sent unchanged, and a `missing_scope` for it is
returned as the error. Each call returns one page: pass the result's
`next_cursor` back as `cursor`, together with its `types`, until `next_cursor`
is `null`.

**`slack_read_channel` / `slack_read_thread` after `channel_not_found`:**
Slack returns `channel_not_found` both for a wrong channel id and for a
private channel the bot is not in. The tool then calls `auth.test` (no scope
needed) to name the bot user in the error, with the `/invite` step. The lookup
makes one attempt, with no rate-limit retry, and is abandoned after 3 seconds.
If it fails, is rate-limited or times out, the error is the bare
`channel_not_found` slug. Any other read error triggers no lookup.

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

## Read-only allowlist under the egress guard

`tm hook --egress-guard` (#8454) refuses every Slack MCP tool to a
subagent, reads included. Since #11273 a PM's or the Architect's main
session may call the `slack-mcp` read, search and list tools; a send or
write needs the Architect's tm launch record (#10447), bar a PM's send to
the owner's DM (#11194). No PM launch record exists yet. The host
`config.yaml` may grant one project two read tools for every caller,
subagents included (#10387):

```yaml
channels:
  version: 1
  slack_read_allowlist:
    - project: /abs/canonical/path/to/project   # compared to CLAUDE_PROJECT_DIR
      name: project-name                        # printed in the audit line
      tools: [slack_read_channel, slack_read_thread]
```

- Only `mcp__slack-mcp__<tool>` is judged; the tool must equal a listed
  name exactly. Since #10735 the claude.ai connector
  `mcp__claude_ai_Slack__<tool>` is not denied at all, so it needs no entry. `slack_read_canvas`,
  `slack_read_file` and every posting tool stay refused.
- The load refuses any other tool name and any `*` or `?`, and an unknown
  key anywhere in `channels:` refuses the whole section.
- A bad allowlist entry fails the whole `channels:` host ceiling closed:
  every channel route is denied until it is fixed.
- A missing, unreadable or malformed file, or a missing or non-canonical
  `CLAUDE_PROJECT_DIR`, keeps the deny; the reason names the fault.
- Each allowed read prints one stderr line:
  `egress-slack-read-allowed project=<name> tool=<tool> prefix=<server>`.
- `config.yaml` is a trust anchor: no session writes it.
