# Running Individual MCP Servers Locally

Each MCP server reads from stdin / writes to stdout (JSON-RPC 2.0 framing).
All daemons log to **stderr** — never stdout.

```bash
# trusty-search daemon (HTTP + MCP stdio)
RUST_LOG=info cargo run -p trusty-search -- start
# Query via CLI
cargo run -p trusty-search -- query "fn authenticate" --indexes <name>
# MCP stdio mode (used by Claude Code via .mcp.json)
cargo run -p trusty-search -- serve

# MPM daemon (the `tm` CLI runs the long-running daemon mode via its `daemon`
# subcommand; there is no separate daemon executable)
RUST_LOG=info cargo run -p trusty-mpm --bin tm -- daemon

# MPM CLI (`tm`). `--bin tm` is required: the crate also builds `trusty-mpm`, a
# compatibility alias that only execs the sibling `tm`
cargo run -p trusty-mpm --bin tm -- --help

# trusty-memory (MCP server + embedded Svelte UI)
RUST_LOG=info cargo run -p trusty-memory

# Clients (`tm`, `tctl`, and the `trusty-search` commands `query`, `doctor` and
# `monitor`) reach trusty-search over its Unix socket: `TRUSTY_SEARCH_SOCKET`,
# else the socket in the daemon's data directory. `trusty-search start --socket
# <absolute path>` binds a chosen path. The HTTP listener is optional, and
# `tm services init` starts the daemon with `--no-http`, so the `port` commands
# below apply only when the HTTP listener is enabled.

# Report the daemon's listening port (stdout is clean — safe for shell substitution):
trusty-search port                                   # bare port: 7878
trusty-search port --addr                            # host:port: 127.0.0.1:7878
trusty-search port --json                            # {"addr":"127.0.0.1","port":7878}
# Shell substitution idiom — queries the daemon without guessing the port:
curl http://127.0.0.1:$(trusty-search port)/health

trusty-memory port                                   # bare port: 7070
trusty-memory port --addr                            # host:port: 127.0.0.1:7070
trusty-memory port --json                            # {"addr":"127.0.0.1","port":7070}
curl http://127.0.0.1:$(trusty-memory port)/health

# Fire-and-forget memory note from any agent (no MCP tool needed):
# Sub-agents spawned via Claude Code's Agent tool do not inherit MCP
# connections, so they cannot call `mcp__trusty-memory__memory_remember`
# directly. The `note` subcommand POSTs to the daemon's HTTP endpoint
# (`POST /api/v1/remember`) and returns immediately — the dispatch runs
# on a detached `tokio::spawn`. Failures degrade to stderr + zero exit.
trusty-memory note "key fact here" --palace my-project
trusty-memory note "another fact" --palace my-project --tag style --tag preferences

# Build a specific binary in release mode
cargo build --release -p trusty-search
./target/release/trusty-search start
```

## Default local URLs

| Service | Default URL |
|---|---|
| trusty-console | `http://127.0.0.1:7788` |
| trusty-search | `http://127.0.0.1:7878` when the HTTP listener is enabled; clients use the Unix socket |
| trusty-memory | `http://127.0.0.1:7070` |

To wire a locally-built binary into Claude Code, update your project's
`.mcp.json` or `~/.claude/mcp.json` to point `command` at the absolute path
of the built binary (e.g. `target/release/trusty-search`).
