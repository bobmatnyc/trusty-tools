# trusty-memory

[![crates.io](https://img.shields.io/crates/v/trusty-memory.svg)](https://crates.io/crates/trusty-memory)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Memory palace MCP server (HTTP/SSE) backed by `hnsw_rs` (HNSW) vector store,
`redb` metadata and knowledge-graph stores, and `fastembed` embeddings. Stores
and retrieves natural-language memories organized into named "palaces"
(namespaces), with an optional knowledge-graph layer for structured triples.

Claude Code integration uses `trusty-memory serve --stdio` — a direct
stdio JSON-RPC MCP server that forwards every request to the running HTTP
daemon and returns daemon responses verbatim. No Unix domain socket is
involved.

A DEPRECATED `trusty-memory-mcp-bridge` shim binary is also installed by
`cargo install trusty-memory` so that existing `.mcp.json` configs that still
reference the old bridge name keep working without per-project changes.
To update your config, manually set the `trusty-memory` entry to
`"command": "trusty-memory", "args": ["serve", "--stdio"]` in your `.mcp.json`
or `~/.claude/mcp.json`.

Integrates with Claude Code and any other MCP-aware client as a first-class
long-term memory backend.

## System Requirements

- **RAM**: 512 MB minimum; 1 GB+ recommended (ONNX embedding model loads ~22 MB,
  hnsw_rs index scales with corpus size)
- **Disk**: ~100 MB for the model cache on first run
  (`~/Library/Application Support/trusty-memory/` on macOS,
  `~/.local/share/trusty-memory/` on Linux)
- **Rust**: 1.88+ (if building from source)

## Installation

### From GitHub Releases (recommended for binary users)

Prebuilt binaries are available for macOS (Apple Silicon) and Linux (x86_64).

1. Download the latest release from [GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases):
   - Look for assets tagged `trusty-memory-v0.15.0`
   - Download the archive for your platform:
     - **macOS arm64 (Apple Silicon)**: `trusty-memory-v0.15.0-aarch64-apple-darwin.tar.gz`
     - **Linux x86_64**: `trusty-memory-v0.15.0-x86_64-unknown-linux-gnu.tar.gz`

2. Extract and install:
   ```bash
   tar xzf trusty-memory-v0.15.0-*.tar.gz
   chmod +x trusty-memory
   sudo mv trusty-memory /usr/local/bin/    # or ~/.local/bin/ if you prefer user install
   ```

3. Verify the installation:
   ```bash
   trusty-memory --version
   ```

### From Source with Cargo

Requires Rust 1.94 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-memory --locked
```

This builds from the latest commit on `main` and installs the binary to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

Installing `trusty-memory` produces four binaries in one command: `trusty-memory`, `trusty-memory-mcp-bridge` (deprecated shim — forwards to `serve --stdio`), `trusty-bm25-daemon`, and `trusty-console`.

To install a specific version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag trusty-memory-v0.15.0 trusty-memory --locked
```

### With Homebrew (recommended)

```bash
brew tap bobmatnyc/trusty
brew install trusty-memory
```

Or install directly without tapping:

```bash
brew install bobmatnyc/trusty/trusty-memory
```

Homebrew provides:
- Automatic updates via `brew upgrade trusty-memory`
- Standard macOS / Linux PATH integration
- Easy dependency management

### Prerequisites & Special Cases

#### Prerequisites

None — the daemon is self-contained and requires no external databases or configuration files to start.

#### Optional: OpenRouter API Key

The embedded memory UI includes a chat panel that requires an OpenRouter API key for the language model integration. Set `OPENROUTER_API_KEY` in your environment or enter it in the UI to enable chat features.

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
trusty-memory              # Start the daemon with chat enabled
```

Chat is optional; the daemon fully functions without it.

#### Note: Embedded Svelte UI

This crate embeds a Svelte admin UI (built and compiled into the binary). The UI is pre-built and included in releases; no additional steps are needed. The embedded UI runs on `http://127.0.0.1:<port>` — see the daemon output for the live port.

### Verify Installation

All installations can be verified by running:

```bash
trusty-memory --version
```

Expected output: the semantic version of the installed binary (e.g., `trusty-memory 0.15.0`).

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
curl http://127.0.0.1:$(trusty-memory port)/health
```

Exits non-zero with a message on stderr when no daemon is running, so shell
substitution fails cleanly.

### Browser dashboard + REST API

The same `trusty-memory serve` daemon serves the embedded Svelte admin UI
at the bound address (printed by `trusty-memory monitor web` once the
daemon is running) and a REST API under `/api/v1/`.

Key REST API field names (verified against `src/web.rs` and `src/service.rs`):
- Recall endpoints (`GET /api/v1/palaces/{id}/recall` and `GET /api/v1/recall`) accept
  the query string as **`q`** (not `query`): `?q=my+search+term&top_k=5`.
- Drawer-create body (`POST /api/v1/palaces/{id}/drawers`) expects a **`content`** field
  (not `text`) for the drawer body.

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
file it finds with the canonical MCP server entry.

### Canonical MCP config (0.15.3+)

The recommended entry in `.mcp.json` or `~/.claude/mcp.json` is:

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve", "--stdio"],
      "env": {}
    }
  }
}
```

`trusty-memory serve --stdio` is a pure daemon-bridge proxy: it ensures the
HTTP daemon is running (auto-starting it if absent), then forwards every
JSON-RPC request to `POST /rpc` on the daemon and returns the response
verbatim. No Unix domain socket is involved. The stdio process never opens
the redb write-lock directly, so it co-exists safely with the running HTTP
daemon.

If you are switching from the legacy `kuzu-memory` server, run
`trusty-memory migrate kuzu-memory` to rewrite all Claude settings files
automatically (see [Migrating from kuzu-memory](#from-kuzu-memory-mcp-config-issue-278) below).

### Compatibility shim (for existing installations)

If your `.mcp.json` still references `trusty-memory-mcp-bridge`, the shim
binary (installed alongside `trusty-memory` since 0.15.3) forwards to
`serve --stdio` automatically. You will see a one-line deprecation warning
on stderr (which Claude Code ignores). To update your config manually, set
the `trusty-memory` entry to:

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve", "--stdio"]
    }
  }
}
```

There is no automated command to rewrite `trusty-memory-mcp-bridge` entries
(unlike the kuzu-memory migration). The shim will be removed in a future
minor version.

Claude Code auto-discovers `.mcp.json` on project open. The daemon must be
running (started either by `trusty-memory setup`'s LaunchAgent or by
`trusty-memory start` / `trusty-memory serve`).

## Available MCP Tools

All tools are exposed via both the MCP protocol (over the `serve --stdio`
path) and the HTTP API (`/api/v1/`). The `palace` argument is required unless
the server was started with `--palace <name>`, which makes it optional
everywhere.

Chat-session tools (`chat_session_*`, `chat_turn_append`) back a redb-backed,
per-palace conversation store: turns are stored verbatim and bypass the
`memory_remember` signal/noise and dedup gates.

<!-- BEGIN GENERATED: mcp-tools -->
The MCP server registers **47 tools**. Authoritative source: `trusty_memory::tools::tool_definitions` —
this table is generated from it, not maintained by hand.

| Tool | Arguments | Summary |
|---|---|---|
| `add_alias` | `palace`, `short`, `full`, `extra?` | Add a short→full alias (e.g. tga → trusty-git-analytics) to the prompt-facts surface. |
| `chat_session_add_turn` | `palace`, `session_id`, `role`, `content` | Append a message (prompt or response) to a chat session's history. |
| `chat_session_create` | `palace`, `session_id?`, `title?` | Create a new chat session in a palace (spec-001 chat-session manager). |
| `chat_session_delete` | `palace`, `session_id` | Delete a chat session (and its full history) from a palace. |
| `chat_session_get` | `palace`, `session_id` | Retrieve a full chat session: metadata plus every turn in chronological order. |
| `chat_session_list` | `palace`, `limit?`, `offset?` | List chat sessions in a palace as paginated metadata (id, title, timestamps, message_count) ordered most-recently-updated first. |
| `chat_session_recall` | `palace`, `session_id` | Retrieve a full chat session with all turns in order (alias for chat_session_get, preferred name for agent-facing recall). |
| `chat_turn_append` | `palace`, `session_id`, `prompt`, `response` | Append a prompt/response PAIR to a chat session as two consecutive messages (user role then assistant role). |
| `console_metrics` | — | Return a ConsoleMetricsReport with palace aggregate statistics (palace_count, cached_palace_count, total_drawers, total_vectors,… |
| `discover_aliases` | `palace`, `project_root?` | Auto-discover project aliases by scanning Cargo workspace members, binary names, first-letter abbreviations, and the git remote. |
| `dream_consolidate_room` | `palace`, `max_age_days?`, `room?` | Trigger LLM-driven semantic consolidation for one room (or all rooms) of a palace, on demand and synchronously (spec-001). |
| `get_prompt_context` | `query?` | Fetch the current project context (aliases, conventions, facts, shorthands) from the memory palace as a Markdown block ready to drop into… |
| `kg_assert` | `palace`, `subject`, `predicate`, `object`, `confidence?`, `provenance?` | Assert a fact in the temporal knowledge graph. |
| `kg_bootstrap` | `palace?`, `project_path?` | Seed the knowledge graph from well-known project files (Cargo.toml, package.json, pyproject.toml, go.mod, CLAUDE.md, .git/config). |
| `kg_gaps` | `palace?` | List knowledge gaps detected in the memory palace graph. |
| `kg_list_subjects` | `palace`, `limit?`, `with_counts?` | List the subjects this palace's knowledge graph actually holds, ordered by subject. |
| `kg_query` | `palace`, `subject` | Query active knowledge-graph triples for a subject. |
| `kg_retract_triple` | `palace`, `subject`, `predicate`, `object` | Retract one fact from the temporal knowledge graph — the inverse of kg_assert. |
| `list_prompt_facts` | — | List every active prompt-fact triple (aliases, conventions, facts, shorthands) across all palaces. |
| `memory_forget` | `palace`, `drawer_id` | Delete a drawer from a palace by its UUID. |
| `memory_list` | `palace`, `limit?`, `room?`, `tag?`, `wing?` | List drawers in a palace, optionally filtered by wing, room type, or tag. |
| `memory_note` | `palace`, `content`, `context?`, `cwd?`, `expires_at?`, `fact_key?`, `room?`, `tags?`, `workstream?` | Curated shortcut for short, high-signal facts ("User prefers snake_case", "Deploy target is prod-east"). |
| `memory_recall` | `palace`, `query`, `room?`, `top_k?`, `wing?` | Recall memories using L0+L1+L2 progressive retrieval. |
| `memory_recall_all` | `q`, `deep?`, `top_k?` | Semantic search across ALL palaces simultaneously. |
| `memory_recall_deep` | `palace`, `query`, `room?`, `top_k?` | Deep recall using L3 full HNSW search. |
| `memory_remember` | `palace`, `text`, `allow_secret_like?`, `context?`, `cwd?`, `expires_at?`, `fact_key?`, `force?`, `room?`, `tags?`, `wing?`, `workstream?` | Store a memory (drawer) in a palace room. |
| `memory_send_message` | `to_palace`, `purpose`, `content`, `cwd?`, `from_palace?`, `workstream?` | Send an inter-project message (issue #99). |
| `palace_compact` | `palace` | Remove orphaned vector index entries (vectors with no matching drawer row). |
| `palace_create` | `name`, `cwd?`, `description?`, `force?` | Create a new memory palace. |
| `palace_delete` | `palace_id`, `force?` | Delete an entire memory palace, including its drawers, vectors, and knowledge graph. |
| `palace_dream` | `palace`, `max_age_days?`, `room?` | On-demand LLM-driven consolidation for a palace (issue #1721). |
| `palace_info` | `palace` | Get metadata and stats for a single palace. |
| `palace_list` | — | List all palaces on this machine. |
| `palace_reembed` | `palace`, `dry_run?`, `limit?` | #4906: report drawers that have no vector (durable but unfindable), and optionally re-embed them. |
| `palace_unalias` | `palace`, `dry_run?` | #5005: free drawers whose vector was destroyed by an id collision (`palace_reembed` reports these as `aliased`), so a re-embed can repair… |
| `palace_update` | `palace_id`, `name` | Update the display name of an existing palace. |
| `remove_prompt_fact` | `subject`, `predicate` | Retract the active triple for a (subject, predicate) pair from the prompt-facts surface. |
| `room_create` | `palace`, `label`, `description?`, `wing?` | Create a room in a palace, or return the existing one (ADR-0027). |
| `room_list` | `palace`, `wing?` | List every room registered in a palace (ADR-0027). |
| `room_rename` | `palace`, `room`, `new_label` | Rename a room (ADR-0027). |
| `task_add` | `palace`, `content`, `room?`, `tags?` | Create a Task drawer in a palace (spec-001 issue #1722). |
| `task_complete` | `palace`, `drawer_id` | Mark a Task drawer as completed by setting its completed_at timestamp (spec-001 issue #1722). |
| `task_list` | `palace`, `include_completed?` | List Task drawers in a palace (spec-001 issue #1722). |
| `upgrade` | `check?`, `confirm?` | Check for or install a new version of trusty-memory (issue #537). |
| `wing_create` | `palace`, `label` | Create a wing (scope) in a palace, or return the existing one with that label (ADR-0027). |
| `wing_list` | `palace` | List the wings of a palace (ADR-0027). |
| `wing_rename` | `palace`, `wing`, `new_label` | Rename a wing (ADR-0027). |
<!-- END GENERATED: mcp-tools -->

Global daemon statistics (total drawers, vectors, KG triples) are available
via `GET /api/v1/status` — an HTTP-only endpoint (`StatusPayload` in
`src/service/types.rs`); there is no corresponding MCP tool, which is why the
generated roster above does not list one.

### Task drawers (protected memory)

`DrawerType::Task` is a drawer classification for goals, milestones, and
checkpoints that an application must re-derive across sessions. Task drawers are
**protected from the dream cycle**: they are never evicted (content-prune,
dedup-merge, or age/importance prune) and never consolidated into summaries,
regardless of age or importance. An optional `completed_at` timestamp marks a
task done — this makes it eligible for *manual* cleanup but never triggers
automatic eviction. Store a Task drawer by classifying a write as `Task`; it
will survive every subsequent dream cycle until explicitly deleted.

## Dream Cycle: Semantic Consolidation (issue #87)

The `palace_dream` / `dream_consolidate_room` tools (and the background idle-dream timer) now include an
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
- `kg.redb` — redb store for drawer metadata and knowledge-graph triples
- `index.usearch` — HNSW vector index (`hnsw_rs`, approximate nearest-neighbour)
- `recall.redb` — recall analytics log (redb; migrated from `recall.db` on first open)
- `l1_cache.json` — top-15 drawers by importance (L1 hot cache, rebuilt at each write)
- `palace.json` — palace metadata (name, description, created_at)

## Architecture

```
trusty-memory (this crate)          trusty-common `memory-core` feature
  axum HTTP/SSE server     ──────►  PalaceRegistry
  serve --stdio (JSON-RPC) ──────►  HNSW vector index (index.usearch)
  embedded Svelte UI               redb metadata + KG (kg.redb)
  MCP tool surface                 fastembed (AllMiniLML6V2Q)

Claude Code stdio ◄──JSON-RPC──► `trusty-memory serve --stdio`
                                  ──POST /rpc (HTTP)──► trusty-memory daemon
```

The `memory-core` feature of `trusty-common` owns the storage engine: `hnsw_rs` for approximate
nearest-neighbor search, `redb` for drawer metadata and knowledge-graph triples, and
`fastembed` for 384-dim text embeddings. The MCP server (`trusty-memory`) is a
thin protocol layer on top.

The embedded Svelte UI is compiled at build time and served via `rust-embed` —
no separate web server or Node.js installation is needed at runtime.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `axum-server` | **enabled** | Compiles the HTTP server, SSE endpoint, and axum-based REST API. Disable with `default-features = false` when embedding only the in-process MCP tools (e.g. from `trusty-agents`). |

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

Licensed under the [MIT License](./LICENSE).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
