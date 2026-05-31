# trusty-tools

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
Unified Rust workspace consolidating 25 crates of AI development tooling,
with three flagship MCP servers for code search, memory management, and analysis.

## Three Flagship MCP Servers

### trusty-search — Hybrid Code Search

Machine-wide code search daemon with hybrid BM25 + vector + knowledge-graph fusion, fused via Reciprocal Rank Fusion. One install per machine, unlimited named project indexes.

**What you get:**
- Sub-10ms p50 warm query latency on 100k-chunk indexes
- Intelligent query routing (Definition / Usage / Conceptual / BugDebt intent detection)
- Knowledge graph expansion with caller/callee chains
- Branch-aware search (boost results from your current feature branch)
- Auto-tuned memory tiers (5 tiers from 8 GB to 64+ GB RAM)
- Embedded Svelte 5 admin UI
- OpenRouter-backed chat with auto-injected search context

**Quick start:**
```bash
cargo install trusty-search
trusty-search start
trusty-search index ~/Projects/myproj --name myproj
trusty-search query "fn authenticate" --index myproj
```

**MCP tools:** `search_code`, `search_similar`, `index_file`, `remove_file`, `list_indexes`, `create_index`, `delete_index`, `reindex`, `index_status`, `list_chunks`, `search_health`, `chat`

See [crates/trusty-search/README.md](crates/trusty-search/README.md) for full documentation.

---

### trusty-memory — Memory Palace Storage Engine

Long-term memory storage with semantic search, persistent embedding index, and embedded Svelte UI. Store development context, notes, snippets, and retrieve them via natural language.

**What you get:**
- HNSW vector index (usearch) + SQLite persistent storage + fastembed embeddings
- Semantic search over all stored memories
- Collection organization (notes, snippets, code patterns, decisions)
- Svelte UI for browsing and editing
- MCP server for Claude Code integration
- MIT license (memory preservation is for everyone)

**Quick start:**
```bash
cargo run -p trusty-memory -- serve
# Or via MCP stdio:
# Add to ~/.claude/claude_desktop_config.json:
# "trusty-memory": {
#   "command": "cargo",
#   "args": ["run", "-p", "trusty-memory", "--", "serve"]
# }
```

**MCP tools:** `store_memory`, `search_memories`, `update_memory`, `list_collections`, `create_collection`, `get_memory`, `delete_memory`

See [crates/trusty-memory/README.md](crates/trusty-memory/README.md) for full documentation.

---

### trusty-analyze — Code Analysis Sidecar

Static code-analysis daemon (sidecar to trusty-search): cyclomatic and cognitive
complexity, code smells, quality grading, git temporal decay, concept clustering,
SCIP protobuf ingest, and a `(subject, predicate, object)` facts store backed by
redb. Reads its chunk corpus from trusty-search over HTTP, then serves results
on port 7879 via both an axum HTTP API and an MCP stdio/SSE server.

**What you get:**
- Cyclomatic + cognitive complexity per chunk / file / index
- Configurable code-smell categories with named thresholds
- A–F quality grade aggregation
- Git blame–driven temporal decay scoring (stale high-complexity code)
- k-means concept clustering (BoW or neural embeddings)
- Facts store: typed knowledge triples with provenance, persisted in redb
- SCIP protobuf ingest for LSP-quality symbol data
- Optional ONNX-backed NER over doc comments (feature-gated: `--features ner`)
- Tree-sitter adapters for Rust, TypeScript, JavaScript, Python, Java, Go, Ruby,
  PHP, C, C++, C#, Kotlin, Swift, Scala
- HTTP API + MCP parity (every endpoint has a tool equivalent)

**Quick start:**
```bash
# 1. trusty-search must be running first — it is a hard runtime dependency
cargo run -p trusty-search -- start

# 2. start the analyze sidecar
cargo run -p trusty-analyze -- serve --search-url http://127.0.0.1:7878

# 3. analyze a named index
cargo run -p trusty-analyze -- analyze <index-id> --top-k 20
```

**MCP tools:** `analyzer_health`, `complexity_hotspots`, `find_smells`,
`analyze_quality`, `list_facts`, `upsert_fact`, `delete_fact`, `ingest_scip`,
`cluster_concepts`

See [crates/trusty-analyze/README.md](crates/trusty-analyze/README.md) for full
documentation.

---

## Full Crate Index (All 25 Crates)

### Core Daemons / MCP Servers

| Crate | Description | License |
|---|---|---|
| `trusty-search` | Hybrid code search (BM25 + vector + KG) + MCP server | Elastic-2.0 |
| `trusty-memory` | Memory palace UI + MCP frontend (storage engine lives in `trusty-common`'s `memory-core` feature) | MIT |
| `trusty-analyze` | Code-analysis sidecar daemon (complexity, smells, facts) + MCP server | MIT |

### Shared Libraries

| Crate | Description |
|---|---|
| `trusty-common` | Tracing, daemon helpers, OpenRouter chat client, shared utilities |
| `trusty-embedder` | fastembed wrapper — all-MiniLM-L6-v2 INT8 quantised, 384-dim output |
| `trusty-mcp-core` | MCP primitives, JSON-RPC 2.0 types, stdio/HTTP transport |
| `trusty-symgraph` | Symbol graph engine — tree-sitter AST parser + knowledge graph |
| `trusty-rpc` | RPC helpers and service descriptors |
| `trusty-tickets` | GitHub Issues integration |
| `trusty-gworkspace` | Google Workspace client (Calendar, Tasks, Drive) |
| `trusty-cto-db` | SQLite schema + rusqlite bindings for ops data |
| `tc-services` | Service adapters (CTO DB, Granola, GWorkspace) |

### MPM Platform (Multi-agent Platform Manager)

| Crate | Description |
|---|---|
| `trusty-mpm-core` | Core domain types and traits |
| `trusty-mpm-mcp` | MCP server for MPM (OpenAPI / Swagger) |
| `trusty-mpm-daemon` | Background daemon service |
| `trusty-mpm-client` | API client library |
| `trusty-mpm-cli` | CLI binary — `trusty-mpm` / `tm` |
| `trusty-mpm-tui` | Terminal UI (ratatui + crossterm) |
| `trusty-mpm-telegram` | Telegram bot integration |
| `trusty-mpm-gui` | Desktop GUI (Tauri) |

### Analytics & Orchestration

| Crate | Description |
|---|---|
| `trusty-git-analytics` | Developer productivity analytics from git history |
| `open-mpm` | MPM orchestration platform |
| `cto-assistant` | CTO domain assistant |

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Flagship Services (User-Facing)                             │
│  trusty-search  |  trusty-memory                             │
│  (search)       |  (storage)                                 │
└──────────────────────────────────────────────────────────────┘
         │                  │
┌────────▼──────────────────▼──────────────────────────────────┐
│  Storage / Engine Libraries                                 │
│  trusty-symgraph  ·  trusty-embedder  ·  trusty-cto-db      │
└────────┬──────────────────────────────────────────────────────┘
         │
┌────────▼──────────────────────────────────────────────────────┐
│  Shared Infrastructure                                       │
│  trusty-common  ·  trusty-mcp-core  ·  trusty-rpc           │
│  trusty-gworkspace  ·  trusty-tickets  ·  tc-services       │
└────────┬──────────────────────────────────────────────────────┘
         │
┌────────▼──────────────────────────────────────────────────────┐
│  Orchestrator / Platform                                    │
│  open-mpm  ·  trusty-mpm-* family  ·  trusty-git-analytics  │
└───────────────────────────────────────────────────────────────┘
```

## Quick Start — All Crates

```bash
git clone https://github.com/bobmatnyc/trusty-tools
cd trusty-tools

# Build all crates
cargo build --release

# Run all tests
cargo test

# Install the CLI tools
cargo install --path crates/trusty-search --locked
# More tools available via: cargo install --path crates/<crate>
```

## Build & Test Commands

```bash
cargo build                                          # all crates, debug
cargo build --release                               # all crates, release/optimized
cargo test                                          # all tests
cargo test -p <crate-name>                          # single crate tests
cargo check                                         # fast compile check
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo fmt
```

## Workspace Info

**Single source of truth:** This monorepo consolidates seven formerly separate repos. All 24 crates are co-located under `crates/` with one workspace root and one `Cargo.lock` — no more `[patch.crates-io]` dances during active development.

**MSRV:** Rust 1.88+ (required for `let-chains` used by `trusty-mpm-*` and `open-mpm` in edition 2024)

**License:** Elastic License 2.0 (most crates), MIT (trusty-memory, trusty-analyze). See each crate's `Cargo.toml` for the authoritative license field.

**Where to start:**
- **I want to search code:** Read [crates/trusty-search/README.md](crates/trusty-search/README.md)
- **I want persistent memory:** Read [crates/trusty-memory/README.md](crates/trusty-memory/README.md)
- **I want the full platform:** Read [crates/open-mpm/README.md](crates/open-mpm/README.md)

