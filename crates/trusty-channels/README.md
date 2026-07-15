# trusty-channels

Native chat-channel MCP servers (chat-as-tools) for the Trusty suite.

One crate, one module per channel. Today only **Slack** exists (module
[`slack`](src/slack/), binary `slack-mcp`); a future Telegram channel slots in
as a sibling `telegram` module + `telegram-mcp` binary without a new crate. See
epic #2636 and ADR-0014.

> **Status: scaffold.** The tool schemas are authoritative and the MCP handshake
> works, but every `tools/call` handler is a stub that returns a clear
> `not-yet-implemented` error. **Slack authentication and HTTP-client hardening
> (401/429) are wired** (issue #2638); the live tool bodies (send/read/search)
> land in #2639/#2640.

## Installation

```sh
cargo install --path crates/trusty-channels
```

This installs the `slack-mcp` binary into `~/.cargo/bin`.

## Quick Start

1. **Wire into Claude Code** (or any MCP client):

   ```jsonc
   // ~/.claude.json
   {
     "mcpServers": {
       "slack": {
         "command": "slack-mcp"
       }
     }
   }
   ```

2. **Handshake works today.** `initialize` and `tools/list` return real
   responses; the model can discover the planned tool surface. Calling any tool
   currently returns a JSON-RPC error explaining that the live call is deferred.

## Slack tool surface (planned / stubbed)

Nine chat-as-tools operations. Schemas are authoritative; handlers are stubs.

| Tool | Purpose | Status |
|------|---------|--------|
| `slack_send_message`    | Post a message (or thread reply) to a channel | stubbed |
| `slack_read_channel`    | Read recent messages from a channel            | stubbed |
| `slack_read_thread`     | Read all replies in a thread                   | stubbed |
| `slack_list_channels`   | List visible channels                          | stubbed |
| `slack_search_messages` | Search messages across the workspace           | stubbed |
| `slack_search_channels` | Search channels by name/topic                  | stubbed |
| `slack_list_users`      | List workspace users                           | stubbed |
| `slack_get_user`        | Fetch a single user's profile                  | stubbed |
| `slack_add_reaction`    | Add an emoji reaction to a message             | stubbed |

The authoritative list with JSON Schemas is in
[`src/slack/tools.rs`](src/slack/tools.rs).

## Configuration

| Env var           | Purpose                                                            |
|-------------------|-------------------------------------------------------------------|
| `SLACK_BOT_TOKEN` | Slack bot token; resolved via the shared credential resolver      |
| `RUST_LOG`        | Standard `tracing` filter (e.g. `trusty_channels=debug`)          |

The Slack bot token is resolved through
`trusty_common::inference::credentials::resolve_key("slack")`, which applies the
standard **process env (`SLACK_BOT_TOKEN`) → `.env.local` → secure store**
precedence. Construction succeeds without a token (so `tools/list` works); a
missing token surfaces only when a tool makes a live call that requires auth.

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
  `trusty_common::mcp::run_stdio_loop`.

Adding a live tool means: implement the Slack call via `BaseClient::call_method`,
replace the `NotImplemented` stub arm in `server::handle_tool_call`, and (if the
surface changes) update the schema in `tools::tool_list_response`.

## Testing

```sh
cargo test -p trusty-channels
```

Covers the credential-resolution path (fake `KeyStore`, no live token), the
HTTP-client behaviour (200 / 401 / `ok:false` / 429-`Retry-After` against a local
`wiremock` server), the `initialize` handshake, the `tools/list` shape/count, and
the `not-yet-implemented` stub error.

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).
