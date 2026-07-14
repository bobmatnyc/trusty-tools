# trusty-slack

Native Slack MCP server (chat-as-tools) for the Trusty suite.

> **Status: scaffold.** This crate is the skeleton of a native-Rust Slack
> [Model Context Protocol](https://modelcontextprotocol.io/) server. The tool
> schemas below are authoritative and the MCP handshake works, but every
> `tools/call` handler is a stub that returns a clear `not-yet-implemented`
> error. **Live Slack Web API calls and authentication are deferred** pending a
> token (bot vs user) decision. See ADR-0014.

## Installation

```sh
cargo install --path crates/trusty-slack
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

## Tool Surface (planned / stubbed)

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

The authoritative list with JSON Schemas is in [`src/tools.rs`](src/tools.rs).

## Configuration

| Env var           | Purpose                                                            |
|-------------------|-------------------------------------------------------------------|
| `SLACK_BOT_TOKEN` | Slack token placeholder consulted at startup (auth wiring deferred) |
| `RUST_LOG`        | Standard `tracing` filter (e.g. `trusty_slack=debug`)             |

> Auth is not yet implemented. The intended source is
> `trusty_common::inference::credentials` (env var / file store / OS keyring)
> once the token decision is made — `SLACK_BOT_TOKEN` is only a documented
> placeholder today.

## Architecture

Two layers, deliberately decoupled — mirroring `trusty-gworkspace`:

- **`api::`** — pure Slack Web API client. `BaseClient` wraps a
  `reqwest::Client` and holds a resolved token placeholder;
  `api::constants` centralises the API base URL. (No live HTTP yet.)
- **`server`** — MCP JSON-RPC dispatch. `handle_message` routes `initialize`,
  `tools/list`, and `tools/call`; `run_stdio` wires it into
  `trusty_common::mcp::run_stdio_loop`.

Adding a live tool means: implement the Slack call in `api::`, replace the
`NotImplemented` stub arm in `server::handle_tool_call`, and (if the surface
changes) update the schema in `tools::tool_list_response`.

## Testing

```sh
cargo test -p trusty-slack
```

Covers the `initialize` handshake, the `tools/list` shape/count, and that a
`tools/call` returns the `not-yet-implemented` stub error. Live Slack API calls
are out of scope until they are implemented.

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).
