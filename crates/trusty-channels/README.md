# trusty-channels

Chat channels for the trusty-* tools. Version 0.1.x is an early release: the
route policy and the Google Chat server are complete; the Slack and Telegram
servers do not yet use the policy.

## What 0.1.x contains

| Part | State |
|------|-------|
| `policy` module | Deny by default. A send needs a route that names the recipient; an inbound message may only answer an open question; a rate limiter bounds floods. Reads the host ceiling (the `channels:` section of `~/.trusty-tools/trusty-mpm/config.yaml`) and each project's `.trusty-channels/routes.toml`, and accepts a route file only when its bytes match the committed default branch. |
| `gchat-mcp` binary | Google Chat MCP server over Pub/Sub. Sends only along the project's committed routes. Installed by default. |
| `slack::api`, `telegram::api` | HTTP clients. They make no recipient decision: the caller owns the route check. |
| `slack-mcp` binary | Slack MCP server with 22 tools. It has **no route check**, so it is not built by default (see below). |
| `telegram-mcp` binary | Scaffold. The handshake answers; every tool call returns `not-yet-implemented` (#2641). It cannot send. |

Not yet wired:

- No Slack or Telegram path enforces the `policy` module yet. That is a later
  slice of #8454.
- The host `connection` block names credentials by registry key (`bot_ref`,
  `app_ref`). A `secret://` reference is refused.
- The semver check has no crates.io baseline for 0.1.0, so the first release
  it guards is the next one.

## Installation

```sh
cargo install trusty-channels                                  # gchat-mcp, telegram-mcp
cargo install trusty-channels --features unrouted-slack-mcp    # adds slack-mcp
```

Install `slack-mcp` only where an unrouted Slack sender is acceptable: any
session that can call it can post to any channel its token reaches.

## Google Chat routes (`gchat-mcp`)

`gchat-mcp` sends only along routes listed in the project's committed
`.trusty-channels/routes.toml` (an uncommitted edit refuses every send). Each
`[[gchat.routes]]` entry has `name`, `recipient` (the person's Chat email),
`kinds` (`question`, `review_notice`) and an optional `space`:

```toml
version = 1

[gchat.connection]
project_id = "my-project"
subscription = "chat-in"
key_file = "~/.config/trusty/chat-sa.json"

# DM route: the DM is learned when the recipient first messages the app.
[[gchat.routes]]
name = "janet"
recipient = "janet@example.com"
kinds = ["question", "review_notice"]

# Space route: posts to one named Space; a reply binds only from the
# route's recipient, in that Space.
[[gchat.routes]]
name = "bob"
recipient = "bob@example.com"
kinds = ["question"]
space = "spaces/AAAAexample"
```

Recipients are unique across routes. `gchat-mcp doctor [--project-dir DIR]
[--offline]` prints per-route health and exits non-zero on any failed check.

## `slack-mcp` (opt-in)

Register it with `tm mcp add slack-mcp -- slack-mcp`, or under `mcpServers`
in `~/.claude.json`. The map key is the tool prefix
(`mcp__slack-mcp__slack_send_message`). The tool list is `TOOL_NAMES` in
[`src/slack/tools.rs`](src/slack/tools.rs); the per-tool OAuth scopes are in
the repository's `crates/trusty-channels/docs/slack-mcp.md`.

| Env var            | Purpose |
|--------------------|---------|
| `SLACK_BOT_TOKEN`  | Bot token; every tool except `slack_search_messages` |
| `SLACK_USER_TOKEN` | User token with `search:read`; `slack_search_messages` only |
| `RUST_LOG`         | `tracing` filter, e.g. `trusty_channels=debug` |

Tokens resolve through `trusty_common::credentials::resolve_key` (process
env, then `.env.local`, then the secure store). The server starts without
them; a tool call that needs a missing token fails.

## Testing

```sh
cargo test -p trusty-channels --no-fail-fast
cargo test -p trusty-channels --no-fail-fast --features unrouted-slack-mcp
```

## License

MIT.
