# trusty-channels

Native chat-channel MCP servers (chat-as-tools) for the Trusty suite.

One crate, one module per channel. **Slack** ([`slack`](src/slack/), binary
`slack-mcp`) is the working channel. A sibling `telegram` module and
`telegram-mcp` binary exist as a scaffold — the handshake answers but every
tool call returns a `not-yet-implemented` MCP error (#2641), so do not register
it expecting a working connector. See epic #2636 and ADR-0014.

> **Status: Slack is live.** All 22 tools below make real Slack Web API calls.
> Slack authentication and HTTP-client hardening (401/429) are wired (issue
> #2638); the original nine tool bodies landed in #2639/#2640, epic #3611 added
> ten more for parity with the claude.ai Slack connector (issues #3612-#3618),
> and three markdown-aware canvas tools followed.
> `slack_send_message_draft` (claude.ai's 20th tool) is **not** implemented —
> Slack has no public API to create an editable message draft.

## Installation

```sh
cargo install --path crates/trusty-channels
```

This installs the `slack-mcp` binary into `~/.cargo/bin`.

## Quick Start

1. **Wire into Claude Code** (or any MCP client). Inside a trusty-mpm-managed
   environment, register it with `tm mcp add` — that writes the tm-owned config
   dir every managed session reads, which a plain `claude mcp add` cannot
   target:

   ```sh
   tm mcp add slack-mcp -- slack-mcp
   tm mcp list                       # confirm what is registered
   ```

   Outside tm, declare it by hand:

   ```jsonc
   // ~/.claude.json
   {
     "mcpServers": {
       "slack-mcp": {
         "command": "slack-mcp"
       }
     }
   }
   ```

   **The name you choose is the tool prefix.** Registered as `slack-mcp`, the
   tools arrive as `mcp__slack-mcp__slack_send_message` and so on; registered as
   `slack`, they arrive as `mcp__slack__*`. The crate name and the binary name
   do not decide it — the map key does. Pick one name and keep it, because agent
   instructions that name a prefix go stale when it changes.

2. **Handshake and every tool are live today.** `initialize` and `tools/list`
   return real responses, and every `tools/call` makes a real Slack Web API
   call through the authenticated `BaseClient`.

3. **Set the tokens.** Registering the server does not authenticate it. Without
   `SLACK_BOT_TOKEN` (and `SLACK_USER_TOKEN` for `slack_search_messages`),
   `tools/list` still answers but every live call fails — see
   [Configuration](#configuration).

## Slack tool surface

22 chat-as-tools operations, all live. Schemas are authoritative — see
[`src/slack/tools.rs`](src/slack/tools.rs) and
[`src/slack/tool_schemas_canvas.rs`](src/slack/tool_schemas_canvas.rs); the
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
| `slack_create_canvas`        | Create a canvas, optionally tabbed into a channel | **`canvases:write`** |
| `slack_update_canvas`        | Replace a canvas's document content | **`canvases:write`** |
| `slack_read_canvas`          | Read a canvas's exported content (HTML, not markdown — see below) | **`canvases:read`**, `files:read` |
| `slack_read_file`            | Read a file's metadata and (text files only) content | `files:read` |
| `slack_search_emojis`        | Search custom emoji by name (client-side filter — no `emoji.search`) | `emoji:read` |
| `slack_search_users`         | Search users by name/real name/email (client-side filter — no non-admin `users.search`) | `users:read` |
| `slack_canvas_create`        | Create a canvas seeded with markdown; with `channel_id` it becomes the channel's canvas and members inherit edit access | **`canvases:write`** |
| `slack_canvas_lookup_sections` | Look up section ids within a canvas, filtered by heading type and/or contained text; returns ids only, never content | **`canvases:read`** |
| `slack_canvas_push`          | Translate CommonMark to canvas markdown and append it, or replace the canvas's header-delimited sections (non-atomic) | **`canvases:write`** (+ **`canvases:read`** for `replace_all`) |

**Bold** scopes (`canvases:read`, `canvases:write`) are new as of epic #3611 —
a Slack app that only had the original nine tools' scopes will need these
added before `slack_create_canvas` / `slack_update_canvas` / `slack_read_canvas`
will work; other tools reuse scopes the original nine already needed.

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

## Configuration

| Env var            | Purpose                                                              |
|--------------------|-----------------------------------------------------------------------|
| `SLACK_BOT_TOKEN`  | Slack bot token (`xoxb-`); resolved via the shared credential resolver; used by every tool except `slack_search_messages` |
| `SLACK_USER_TOKEN` | Slack user token (`xoxp-`, needs `search:read`); used only by `slack_search_messages` — `search.messages` rejects a bot token |
| `RUST_LOG`         | Standard `tracing` filter (e.g. `trusty_channels=debug`)              |

Both tokens are resolved through
`trusty_common::credentials::resolve_key`, which applies the
standard **process env → `.env.local` → secure store** precedence. Construction
succeeds without either token (so `tools/list` works); a missing token
surfaces only when a tool makes a live call that requires it.

## Architecture

Each channel module has two layers, deliberately decoupled — mirroring
`trusty-gworkspace`:

- **`slack::api`** — pure Slack Web API client. `BaseClient` wraps a
  `reqwest::Client`, resolves the bot token via the credential resolver, and
  hardens the request path: HTTP 401 (and auth-class `ok:false`) map to a typed
  `SlackError::Auth` (never a silent anonymous retry); `429 Retry-After` is
  honoured with a bounded backoff. `slack::api::constants` centralises the API
  base URL, provider key, and retry limits.
- **`slack::server`** — MCP JSON-RPC dispatch. `handle_message` routes
  `initialize`, `tools/list`, and `tools/call`; `run_stdio` wires it into
  `trusty_mcp::run_stdio_loop`.

Adding a new tool means: implement the Slack call via `BaseClient::call_method`
(or `call_method_user` for a user-scope-only method) in a handler under
`slack::handlers`, add a dispatch arm in `handlers::dispatch`, add the name to
`tools::TOOL_NAMES`, and add its schema to `tools::tool_list_response` —
`TOOL_NAMES` and the dispatch match arm must stay in 1:1 agreement (enforced by
`tools::tests::known_tools_match_registry`).

## Testing

```sh
cargo test -p trusty-channels
```

Covers the credential-resolution path (fake `KeyStore`, no live token), the
HTTP-client behaviour (200 / 401 / `ok:false` / 429-`Retry-After` / private-file
download against a local `wiremock` server), the `initialize` handshake, the
`tools/list` shape/count, and every tool's request/response shaping and error
mapping (`tests/tools_http.rs`).

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).
