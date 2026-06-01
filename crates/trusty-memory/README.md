# trusty-memory

[![crates.io](https://img.shields.io/crates/v/trusty-memory.svg)](https://crates.io/crates/trusty-memory)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Memory palace MCP server (HTTP/SSE + Unix domain socket) backed by `usearch`
vector store, SQLite metadata, and `fastembed` embeddings. Stores and retrieves
natural-language memories organized into named "palaces" (namespaces), with an
optional knowledge-graph layer for structured triples.

Claude Code integration uses the companion `trusty-memory-mcp-bridge` binary
which speaks stdio MCP and pipes it over the daemon's Unix domain socket.
The legacy in-process `serve --stdio` flag was removed in issue #150 because
it deadlocked on redb's exclusive write lock whenever a long-lived daemon
was already running.

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

### Start the daemon

```bash
trusty-memory serve
```

By default, `serve` self-spawns a detached background daemon (alias for
`trusty-memory start`) and returns control to the shell so you keep your
prompt. The daemon binds HTTP/SSE on a dynamic port in the `7070..=7079`
range (with OS fallback) and writes the resolved address to its
discovery file. Pass `--foreground` to keep the daemon inline (used by
launchd / systemd / Docker), or `--http <ADDR>` to pin a specific address.

### Check the listening port

```bash
trusty-memory port               # bare port: 7070
trusty-memory port --addr        # host:port: 127.0.0.1:7070
trusty-memory port --json        # {"addr":"127.0.0.1","port":7070}

# Shell substitution — stdout is clean (logs go to stderr):
curl http://127.0.0.1:$(trusty-memory port)/api/v1/health
```

Exits non-zero with a message on stderr when no daemon is running, so shell
substitution fails cleanly.

### Browser dashboard + REST API

The same `trusty-memory serve` daemon serves the embedded Svelte admin UI
at the bound address (printed by `trusty-memory monitor web` once the
daemon is running) and a REST API under `/api/v1/`.

### Bind to a named palace

When all tool calls should default to one palace namespace, use `--palace`:

```bash
trusty-memory serve --palace my-project
```

With a default set, the `palace` argument becomes optional in every MCP tool
call.

## Claude Code Integration

Run `trusty-memory setup` once — it installs the launchd LaunchAgent
(macOS), pre-warms the embedder cache, and patches every Claude settings
file it finds with the canonical MCP server entry. The MCP entry points
at `trusty-memory-mcp-bridge`, a thin stdio-to-UDS pipe (PR #149) that
Claude Code spawns as a child process. The bridge then connects to the
long-lived daemon over the Unix domain socket the daemon owns.

If you prefer to edit `.mcp.json` (or `~/.claude/mcp.json`) by hand:

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory-mcp-bridge",
      "args": [],
      "env": {}
    }
  }
}
```

Claude Code auto-discovers `.mcp.json` on project open. The
`trusty-memory-mcp-bridge` binary must be on `PATH` and the daemon must be
running (started either by `trusty-memory setup`'s LaunchAgent or by
`trusty-memory start` / `trusty-memory serve`).

## Available MCP Tools

The MCP server registers **23 tools** (authoritative source:
`src/tools.rs` `tool_definitions`, asserted by the
`tool_definitions_lists_all_tools` test). All are exposed via both the MCP
protocol (over the `trusty-memory-mcp-bridge` → UDS path) and the HTTP API
(`/api/v1/`). The `palace` argument is required unless the server was started
with `--palace <name>`. The tables below cover the primary surface; the full
roster also includes `memory_note`, `memory_recall_all`, `palace_delete`,
`palace_update`, `palace_compact`, `kg_gaps`, `kg_bootstrap`, `add_alias`,
`discover_aliases`, `list_prompt_facts`, `remove_prompt_fact`, and
`get_prompt_context`.

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
| `palace_create` | `name, description?` | Create a new palace namespace. See [Palace as Project](#palace-as-project) for naming rules. |
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

## Dream Cycle: Semantic Consolidation (issue #87)

The `memory_dream` tool (and the background idle-dream timer) now includes an
**inference-backed semantic consolidation phase** after the existing NLP passes
(dedup, prune, closet-refresh):

### What it does

1. Groups palace drawers into batches of up to 8 (configurable via `DreamConfig`).
2. Sends each batch to an LLM backend (OpenRouter or local Ollama) with a
   structured prompt asking for three actions:
   - **Alias** — two terms refer to the same concept (e.g. `"ts"` → `"trusty-search"`).
     A `superseded_by` KG triple is written to link the alias to its canonical form.
   - **Merge** — a cluster of overlapping drawers is collapsed into one canonical
     drawer. The original drawers are preserved (additive-only policy); a
     `superseded_by` KG triple is written from each original to the new canonical
     drawer so lineage is traceable.
   - **Flag** — a drawer contradicts another and should be reviewed by a human.
     Flagged drawers are logged at `WARN` level but not deleted.
3. Each batch response is cached by a SHA-256 key over the drawer IDs + content,
   so repeated dream cycles do not re-spend LLM tokens on stable content.
4. A per-cycle call budget (`max_calls_per_cycle`, default 20) prevents runaway
   LLM spend on large palaces.

### Configuration

Semantic consolidation is enabled when at least one inference backend is available:

| Priority | When used |
|---|---|
| OpenRouter | `OPENROUTER_API_KEY` env var is set, or `DreamConfig.openrouter_api_key` is non-empty |
| Ollama (local) | `DreamConfig.local_model_enabled = true` AND no OpenRouter key is present |
| Disabled (no-op) | Neither backend is available — the phase is silently skipped |

The consolidation model defaults to `anthropic/claude-haiku-4-5` for low cost.
Override via `DreamConfig.semantic.model`.

```toml
# ~/.trusty-memory/config.toml — set these to enable semantic consolidation
[openrouter]
api_key = "sk-or-v1-..."   # enables semantic consolidation via OpenRouter  # pragma: allowlist secret

[local_model]
enabled = false             # set true + unset api_key to use Ollama instead
base_url = "http://127.0.0.1:11434"
model = "llama3"
```

### Testing

All consolidation tests use `MockInference` — no real API calls are made during
`cargo test`. Live-inference tests are marked `#[ignore]`:

```bash
# Unit + integration tests (no network)
cargo test -p trusty-common --features memory-core -- semantic_consolidation

# Live-network tests (requires OPENROUTER_API_KEY)
cargo test -p trusty-common --features memory-core -- --include-ignored semantic_consolidation
```

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

## Palace as Project

(Issue #88) Palace names are anchored to the project they belong to.

### One palace per project

When you create a new palace, trusty-memory walks upward from the current
working directory looking for project markers (`.git`, `Cargo.toml`,
`pyproject.toml`, `package.json`, `go.mod`) and derives a **canonical slug**
from the project root's directory name:

```
/Users/bob/Projects/trusty-tools  →  trusty-tools
/Users/bob/Projects/My_App!       →  my-app
/Users/bob/Projects/my project    →  my-project
```

New palaces must be named with the derived slug:

```bash
# Inside /Users/bob/Projects/trusty-tools — this succeeds
palace_create("trusty-tools")

# Any other name fails with a descriptive error:
# "palace name 'notes' does not match the project slug 'trusty-tools'.
#  Either use 'trusty-tools' or use 'personal' for non-project memories."
palace_create("notes")
```

### The `personal` palace

When operating outside any project directory (no `.git` / `Cargo.toml` /
etc. found anywhere above CWD), only the special palace name `personal` is
allowed. This is the "no project, no problem" escape hatch for global notes,
one-off sessions, and personal task lists.

```bash
# From any directory without a project root:
palace_create("personal")  # always succeeds
palace_create("my-notes")  # fails — no project root detected
```

### Existing palaces are grandfathered

The enforcement applies **only to new palace creation**. Every palace created
before this feature was added continues to work without any migration.
Existing palaces remain read-write and are never auto-renamed.

### `doctor --fix-palaces`

The `doctor --fix-palaces` subcommand audits existing palaces and reports
which ones are orphaned (name does not correspond to a detectable project
directory on disk):

```bash
# Dry-run audit (no filesystem changes):
trusty-memory doctor --fix-palaces

# Include rename suggestions for orphaned palaces (still read-only):
trusty-memory doctor --fix-palaces --fix
```

Output example:

```
✅  trusty-tools     — project palace ok
⚠️   old-project     — orphaned (no matching project directory found on disk)
   → rename suggested: old-project → personal
❌  broken-palace    — empty (no palace.json; directory may be a leftover)

· palace audit: 1 ok, 1 orphaned, 1 empty.
```

The `--fix` flag prints rename suggestions but **does not mutate the
filesystem**. Actual renaming (merging orphaned data into `personal`) is
planned for a future release. The `doctor` audit is purely advisory for
existing palaces.

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
trusty-memory (this crate)          trusty-common `memory-core` feature
  axum HTTP/SSE server     ──────►  PalaceRegistry
  Unix domain socket       ──────►  usearch vector index
  embedded Svelte UI               SQLite metadata + KG
  23 MCP tools                     fastembed (AllMiniLML6V2Q)

trusty-memory-mcp-bridge (separate binary, PR #149)
  Claude Code stdio  ◄──pipe──►  trusty-memory UDS
```

The `memory-core` feature of `trusty-common` owns the storage engine: `usearch` for approximate
nearest-neighbor search, SQLite for metadata and knowledge-graph triples, and
`fastembed` for 384-dim text embeddings. The MCP server (`trusty-memory`) is a
thin protocol layer on top.

The embedded Svelte UI is compiled at build time and served via `rust-embed` —
no separate web server or Node.js installation is needed at runtime.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `axum-server` | **enabled** | Compiles the HTTP server, SSE endpoint, and axum-based REST API. Disable with `default-features = false` when embedding only the in-process MCP tools (e.g. from `open-mpm`). |

```toml
# Full daemon build — no change needed (axum-server is on by default)
trusty-memory = { workspace = true }

# rlib consumer — omit the HTTP stack
trusty-memory = { workspace = true, default-features = false }
```

## Migration

### From kuzu-memory (MCP config, issue #278)

If you're switching from the legacy `kuzu-memory` Python MCP server, rewrite
every Claude `mcpServers` config entry in one command:

```bash
# Dry-run: see what would change
trusty-memory migrate kuzu-memory --dry-run

# Apply: rewrite all Claude settings files atomically
trusty-memory migrate kuzu-memory
```

### Knowledge-graph hygiene (issue #278)

Auto-KG extraction skips drawers tagged `cross-project-qa`, `test`, or
`fixture` so synthetic content never pollutes the graph. Drawers deleted via
`memory_forget` cascade-delete their derived triples automatically.

The REST endpoint `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` lets
you surgically remove a single active triple; `triple_id` is the base64url
encoding of `subject + "\0" + predicate`.

### From kuzu-memory data (issue #277)

Import entities and relations from a kuzu-memory `store.redb` into a
trusty-memory palace:

```bash
# Dry-run: see what would be imported
trusty-memory migrate kuzu-data \
  --from ~/.open-mpm/memory/store.redb \
  --palace my-palace \
  --dry-run

# Apply the import (idempotent — safe to re-run)
trusty-memory migrate kuzu-data \
  --from ~/.open-mpm/memory/store.redb \
  --palace my-palace

# Cap at 100 entities
trusty-memory migrate kuzu-data \
  --from ~/.open-mpm/memory/store.redb \
  --palace my-palace \
  --limit 100
```

Each kuzu-memory entity becomes one drawer; each relation becomes one KG
triple. Re-running is idempotent: entity IDs are SHA-256-derived UUIDs so
duplicate imports produce the same drawer ID and are silently skipped.

## Development

```bash
# Build and run (background daemon, dynamic port)
cargo run -p trusty-memory -- serve

# Run inline on a specific address (foreground, useful for debuggers)
cargo run -p trusty-memory -- serve --foreground --http 127.0.0.1:7880

# Tests
cargo test -p trusty-memory
# Storage-engine tests live in trusty-common behind the memory-core feature:
cargo test -p trusty-common --features memory-core

# Check only (faster)
cargo check -p trusty-memory
```

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
