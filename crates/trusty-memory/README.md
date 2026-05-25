# trusty-memory

[![crates.io](https://img.shields.io/crates/v/trusty-memory.svg)](https://crates.io/crates/trusty-memory)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Memory palace MCP server (stdio + HTTP/SSE) backed by `usearch` vector store,
SQLite metadata, and `fastembed` embeddings. Stores and retrieves natural-language
memories organized into named "palaces" (namespaces), with an optional
knowledge-graph layer for structured triples.

Integrates with Claude Code and any other MCP-aware client as a first-class
long-term memory backend.

## System Requirements

- **RAM**: 512 MB minimum; 1 GB+ recommended (ONNX embedding model loads ~22 MB,
  usearch index scales with corpus size)
- **Disk**: ~100 MB for the model cache on first run
  (`~/Library/Application Support/trusty-memory/` on macOS,
  `~/.local/share/trusty-memory/` on Linux)
- **Rust**: 1.88+ (if building from source)

## Installation

```bash
cargo install trusty-memory
```

The installed binary is named `trusty-memory`.

## Quick Start

### stdio mode (Claude Code integration)

```bash
trusty-memory serve
```

Reads JSON-RPC 2.0 from stdin, writes responses to stdout. This is the mode
Claude Code uses.

### HTTP mode (browser dashboard + REST API)

```bash
trusty-memory serve --port 7880
```

Binds an HTTP server with the embedded Svelte admin UI at `http://127.0.0.1:7880`
and a REST API under `/api/v1/`.

### Bind to a named palace

When all tool calls should default to one palace namespace, use `--palace`:

```bash
trusty-memory serve --palace my-project
```

With a default set, the `palace` argument becomes optional in every MCP tool
call.

## Claude Code Integration

Add to your project's `.mcp.json` (or `~/.claude/mcp.json` for machine-wide):

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve"],
      "env": {}
    }
  }
}
```

To pre-bind a palace so Claude Code never needs to pass `palace` explicitly:

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve", "--palace", "my-project"],
      "env": {}
    }
  }
}
```

Claude Code auto-discovers `.mcp.json` on project open. The `trusty-memory`
binary must be on `PATH`.

## Available MCP Tools

All 12 tools are exposed via both the stdio MCP server and the HTTP API
(`/api/v1/`). The `palace` argument is required unless the server was started
with `--palace <name>`.

### Memory tools

| Tool | Arguments | Description |
|---|---|---|
| `memory_remember` | `palace, text, room?, tags?` | Store a memory. Returns the `drawer_id`. |
| `memory_recall` | `palace, query, top_k?` | Hybrid BM25+vector recall (L0/L1/L2 layers). |
| `memory_recall_deep` | `palace, query, top_k?` | Deep recall (L3 — slower, higher recall). |
| `memory_list` | `palace, room?, tag?, limit?` | List stored memories, optionally filtered. |
| `memory_forget` | `palace, drawer_id` | Delete a specific memory by ID. |

### Palace management tools

| Tool | Arguments | Description |
|---|---|---|
| `palace_create` | `name, description?` | Create a new palace namespace. |
| `palace_list` | — | List all palaces and their IDs. |
| `palace_info` | `palace` | Palace metadata and statistics. |

### Knowledge graph tools

| Tool | Arguments | Description |
|---|---|---|
| `kg_assert` | `palace, subject, predicate, object, confidence?, provenance?` | Assert a knowledge triple. |
| `kg_query` | `palace, subject` | Query triples by subject. |

### Dream and status tools

| Tool | Arguments | Description |
|---|---|---|
| `memory_dream` | `palace?` | Run a consolidation cycle (merge near-duplicates, prune, compact). |
| `memory_status` | — | Global statistics (total drawers, vectors, KG triples). |

### Inter-project messaging (issue #99)

| Tool | Arguments | Description |
|---|---|---|
| `memory_send_message` | `to_palace, purpose, content, from_palace?` | Deliver a message to another palace's inbox. |

Plus two CLI subcommands:

- `trusty-memory send-message --to <palace> --purpose <p> --content <text> [--from <palace>]`
  — non-MCP entry point. Posts to the daemon's `POST /api/v1/messages`.
- `trusty-memory inbox-check [--palace <id>]` — installed as a Claude Code
  `SessionStart` hook by `setup`. Reads unread messages from the cwd-derived
  palace, prints them to stdout (Claude Code injects stdout as session
  context), and atomically marks them read.

#### Design

A message is a **drawer in the recipient's palace** carrying a namespaced
tag envelope. No new schema, no new database — just convention:

| Tag | Example | Meaning |
|---|---|---|
| `msg:v1` | (literal) | Marker tag for the v1 envelope. |
| `msg:from=<palace>` | `msg:from=trusty-tools` | Sender palace id. |
| `msg:to=<palace>` | `msg:to=claude-mpm` | Recipient palace id (audit). |
| `msg:purpose=<text>` | `msg:purpose=task` | Free-text purpose / category. |
| `msg:sent_at=<rfc3339>` | `msg:sent_at=2026-05-25T12:34:56+00:00` | UTC send timestamp. |
| `msg:read=<bool>` | `msg:read=false` | Receiver-flipped read flag. |

#### Addressing

Sender and recipient palaces are addressed by **repo slug**. The slug is
derived from the working directory by:

1. Take the basename of `git rev-parse --show-toplevel` (or cwd, when not in
   a git checkout).
2. Strip a trailing `.git` suffix if present.
3. Lowercase.
4. Replace every run of whitespace or `_` with a single `-`.
5. Strip every character outside `[a-z0-9-]`.
6. Collapse consecutive `-` and trim leading/trailing `-`.

Examples (all resolve to `trusty-tools`):
`/Users/bob/Projects/trusty-tools`,
`/Users/bob/Projects/Trusty_Tools`,
`/Users/bob/Projects/trusty tools`,
`/Users/bob/Projects/.trusty-tools.git`.

No central registry; sender and receiver agree on the slug out of band.

#### Delivery

The receiver's `trusty-memory setup` installs `trusty-memory inbox-check` as
a `SessionStart` hook in every Claude Code settings file it finds (alongside
the existing `UserPromptSubmit` `prompt-context` hook). On every new Claude
Code session, the hook:

1. Resolves the receiver palace slug from cwd.
2. Fetches unread messages from `GET /api/v1/messages?palace=<slug>&unread_only=true`.
3. Prints each as a Markdown block to stdout — Claude Code injects stdout as
   session context.
4. Atomically marks each delivered message read via
   `POST /api/v1/messages/mark_read`.

The mark-read step uses an in-memory compare-and-swap on the palace's
drawer table so two concurrent sessions opening at once cannot
double-deliver: exactly one observes `read=false` and flips the flag, the
other returns `false` and emits nothing.

Every failure path in `inbox-check` degrades to exit 0 with empty stdout
so a missing or slow daemon never blocks Claude Code session start.

#### Migration from `claude-mpm` `/mpm-message`

This primitive replaces the Python `/mpm-message` skill in `claude-mpm`
(which wrote to `~/.claude-mpm/messaging.db` via a process-local SQLite
file). The companion ticket in `claude-mpm` is `#557`; data migration is
out of scope here.

## Web UI

When running in HTTP mode, the embedded Svelte admin dashboard is available at:

```
http://127.0.0.1:<port>/
```

The dashboard provides:
- Real-time palace overview (drawer counts, vector counts, KG triple counts)
- Live event stream (palace created, drawer added/deleted, dream completed)
- Manual dream (consolidation) trigger
- Palace-scoped memory browsing

## Configuration

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `RUST_LOG` | `warn` | Tracing filter. E.g. `RUST_LOG=info` or `RUST_LOG=trusty_memory=debug`. |
| `OPENROUTER_API_KEY` | — | Enables chat completions via OpenRouter (`/api/v1/chat`). |
| `TRUSTY_DATA_DIR_OVERRIDE` | — | Override the data directory (intended for tests). |

### Config file

`~/.trusty-memory/config.toml` (created on first run if absent):

```toml
[openrouter]
api_key = ""
model = "anthropic/claude-3.5-sonnet"

[local_model]
enabled = false
base_url = "http://127.0.0.1:11434"
model = "llama3"
```

### Data directory

Memories and vector indexes persist under the OS-standard data directory:

- **macOS**: `~/Library/Application Support/trusty-memory/<palace-id>/`
- **Linux**: `~/.local/share/trusty-memory/<palace-id>/`

Each palace directory contains:
- `drawers.db` — SQLite metadata store
- `vectors.usearch` — usearch vector index
- `kg.db` — knowledge-graph triples (SQLite)
- `chat_sessions.db` — chat session history

## Architecture

```
trusty-memory (this crate)          trusty-memory-core
  stdio JSON-RPC 2.0 loop  ──────►  PalaceRegistry
  axum HTTP/SSE server     ──────►  usearch vector index
  embedded Svelte UI               SQLite metadata + KG
  12 MCP tools                     fastembed (AllMiniLML6V2Q)
```

`trusty-memory-core` owns the storage engine: `usearch` for approximate
nearest-neighbor search, SQLite for metadata and knowledge-graph triples, and
`fastembed` for 384-dim text embeddings. The MCP server (`trusty-memory`) is a
thin protocol layer on top.

The embedded Svelte UI is compiled at build time and served via `rust-embed` —
no separate web server or Node.js installation is needed at runtime.

## Development

```bash
# Build and run
cargo run -p trusty-memory -- serve

# Run in HTTP mode
cargo run -p trusty-memory -- serve --port 7880

# Tests
cargo test -p trusty-memory
cargo test -p trusty-memory-core

# Check only (faster)
cargo check -p trusty-memory
```

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
