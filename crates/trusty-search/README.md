# trusty-search

[![CI](https://github.com/bobmatnyc/trusty-search/actions/workflows/ci.yml/badge.svg)](https://github.com/bobmatnyc/trusty-search/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/trusty-search.svg)](https://crates.io/crates/trusty-search)
[![License: ELv2](https://img.shields.io/badge/License-Elastic%20License%202.0-blue.svg)](./LICENSE)

Machine-wide, blazingly fast hybrid code search service. One install per machine,
one always-on daemon, unlimited named indexes.

## System requirements

- **Rust 1.75+** (for source builds)
- **16 GB RAM minimum (default)** — hard-checked at daemon startup. The daemon exits with an actionable error message on under-spec hosts. Set `TRUSTY_SKIP_RAM_CHECK=1` in the daemon environment to bypass this check for small workloads where peak RAM is known to stay well under the memory limit. Bypass at your own risk on large corpora — the default exists because realistic indexing OOMs without it.
- **macOS 12+ or Linux** (Windows: not yet supported)
- **~2 GB disk** for model cache (downloaded on first run to `~/Library/Caches/trusty-search/` on macOS or `$XDG_DATA_HOME/trusty-search/` on Linux)

## Install

### From crates.io (recommended)

```bash
cargo install trusty-search
```

### From source

```bash
git clone https://github.com/bobmatnyc/trusty-search
cd trusty-search
cargo install --path . --locked
```

### Apple Silicon

CoreML GPU acceleration is enabled automatically on M1/M2/M3/M4. No flags or extra installs are needed. The startup log confirms the active provider:

```
embedder initialized: model=AllMiniLML6V2(Q) dim=384 provider=CoreML (Metal GPU / ANE)
```

### NVIDIA GPU (CUDA)

```bash
cargo install trusty-search --features cuda
```

Requires CUDA toolkit installed on the host. See [CLAUDE.md](./CLAUDE.md) for `ORT_DYLIB_PATH` setup on Amazon Linux 2023 and other glibc 2.34 hosts.

## Quick start

The following five steps take you from zero to a running search in under five minutes.

**Step 1 — Start the daemon**

```bash
trusty-search start
```

Expected output:
```
trusty-search daemon starting on http://127.0.0.1:<port>
embedder initialized: model=AllMiniLML6V2(Q) dim=384 provider=CoreML (Metal GPU / ANE)
daemon ready
```

The daemon auto-selects a free port and writes it to `~/Library/Application Support/trusty-search/port.lock`.

**Step 2 — Index a project**

```bash
trusty-search index ~/Projects/myproj --name myproj
```

Expected output:
```
Registered index "myproj" at /Users/me/Projects/myproj
⟳ Indexing myproj [████████░░] 1204/1520 files — 12s remaining
✓ Indexed 14 823 chunks in 142s
```

Re-running is safe — unchanged files are skipped via content fingerprints. Use `--force` to rebuild from scratch.

**Step 3 — Run a search**

```bash
trusty-search query "fn authenticate" --index myproj
```

Expected output:
```
1. src/auth.rs:42 — authenticate (hybrid+kg, score=0.018)
   fn authenticate(ctx: &Context) -> Result<Token> {
2. src/middleware.rs:17 — verify_token (hybrid, score=0.011)
   ...
```

Add `--json` for machine-readable output.

**Step 4 — Open the admin UI**

```bash
trusty-search ui
```

Opens `http://127.0.0.1:<port>/ui` in your browser. The UI provides search, index management, and an OpenRouter-backed chat panel (requires `OPENROUTER_API_KEY`).

**Step 5 — Check status at any time**

```bash
trusty-search status   # daemon version, port, per-index chunk counts
trusty-search doctor   # 6-check diagnostic; add --fix to auto-repair
```

## Using with Claude Code

Add trusty-search as an MCP server in your Claude Code config (`~/.claude/claude_desktop_config.json` or via `claude mcp add`):

### stdio (recommended)

```json
{
  "mcpServers": {
    "trusty-search": {
      "command": "trusty-search",
      "args": ["serve"]
    }
  }
}
```

### HTTP/SSE

```bash
trusty-search serve --http 127.0.0.1:7879
```

Then add `http://127.0.0.1:7879/sse` as an SSE MCP endpoint in your Claude Code config.

Once connected, Claude Code can call `search_code`, `index_file`, `list_indexes`, and 9 other tools directly. The daemon must be running independently (`trusty-search start`) before Claude Code connects.

## Features

- **Machine-wide daemon** — single install (`cargo install trusty-search`),
  one process, unlimited registered indexes via `DashMap<IndexId, IndexHandle>`
- **Hybrid search** — BM25 (lexical, zero-dep port with camelCase / snake_case
  splitting) + HNSW vector (usearch 2.25, all-MiniLM-L6-v2 INT8) + Knowledge
  Graph 1–2 hop expansion, fused via Reciprocal Rank Fusion (k = 60, always-on)
- **Query intent routing** — sub-ms regex classifier routes every query to one
  of 5 intents and adjusts α / β weights and KG gating per query
- **Branch-aware search** — pass `branch_files` (or just `branch: "feature/foo"`) to
  `POST /indexes/:id/search`; chunks from your branch get a configurable score boost
  (default 1.5×) and every result carries `on_branch: bool`
- **KG symbol graph** — petgraph-backed `SymbolGraph` derived from tree-sitter
  parses, with `EdgeKind` (CALLS / IMPORTS / INHERITS / CONTAINS) score
  multipliers; KG expansion is intent-gated (Usage only)
- **Auto-tuned memory tiers** — 5 tiers (Tiny / Small / Medium / Large / XLarge)
  from < 8 GB up to 64+ GB; chunk caps, batch sizes, cache sizes, and BM25 /
  KG limits computed at daemon startup from detected RAM
- **macOS CoreML auto-detection** — on Apple Silicon the ONNX session
  registers the CoreML execution provider automatically (no `--features`
  flag needed since v0.3.13)
- **Multi-index repo support** — drop a `trusty-search.yaml` at the repo root
  to define per-directory named indexes; `trusty-search index` reads it
  automatically (see [`docs/examples/trusty-search.yaml`](docs/examples/trusty-search.yaml))
- **Incremental reindex** — sha2 content fingerprints skip unchanged files
  across daemon restarts; `--force` triggers a full rebuild
- **Zero cold-start queries** — HNSW kept hot (`Duration::MAX` cool-after),
  LRU embedding cache (256+ entries) skips re-embedding on repeat queries
- **Native multi-request** — `Arc<SearchAppState>`, reader-priority `RwLock`,
  axum HTTP/2 — many concurrent searches against the same index never block
- **MCP server** — stdio + HTTP/SSE transports, 11 tools, drop-in for Claude Code
- **Embedded Svelte 5 admin UI** — Collections, Search, Chat, Admin panels
  compiled into the binary via `include_dir!`; open with `trusty-search ui`
- **Migration path** — `trusty-search convert` reads `mcp-vector-search`
  configs and re-registers each project as a named index

> **Code quality analysis:** Complexity hotspots, smell detection, and quality grades
> have moved to [trusty-analyze](../trusty-analyze).
> The `complexity_hotspots`, `smells`, and `quality` HTTP endpoints are not served
> from this binary as of v0.2.0.

## Memory tiers (auto-tuned at startup)

`MEMORY_LIMIT_MB` is computed dynamically as **25% of detected system RAM, clamped to 1–64 GB**. It is not a fixed tier value. The env var `TRUSTY_MEMORY_LIMIT_MB` overrides it. All other limits below are tier-based.

| Tier   | Total RAM  | `MEMORY_LIMIT_MB`     | `MAX_CHUNKS` | `EMBEDDING_CACHE` | `MAX_BATCH_SIZE` | `BM25_CORPUS_CAP` | `MAX_KG_NODES` |
|--------|------------|-----------------------|--------------|-------------------|------------------|-------------------|----------------|
| Tiny   | < 8 GB     | 25% of RAM (≥ 1 GB)   | 50 000       | 500               | 64               | 20 000            | 30 000         |
| Small  | 8–15 GB    | 25% of RAM            | 100 000      | 1 000             | 128              | 50 000            | 75 000         |
| Medium | 16–31 GB   | 25% of RAM            | 200 000      | 5 000             | 256              | 100 000           | 150 000        |
| Large  | 32–63 GB   | 25% of RAM            | 400 000      | 10 000            | 512              | 200 000           | 300 000        |
| XLarge | ≥ 64 GB    | 25% of RAM (≤ 64 GB)  | 800 000      | 20 000            | 512              | 400 000           | 500 000        |

Env vars (`TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`, `TRUSTY_MAX_BATCH_SIZE`,
`TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`, `TRUSTY_MEMORY_LIMIT_MB`,
`TRUSTY_COREML_BATCH_SIZE`, `TRUSTY_COREML_TRIPWIRE_MB`)
always override the tier default. Precedence: shell env > `daemon.env` >
tier default. The resolved tier and all limits are logged at daemon startup.

### Apple Silicon CoreML batch sizing

On Apple Silicon (M1–M4), the ONNX Runtime CoreML execution provider batches
are optimised separately from CPU and GPU tiers:

- **`DEFAULT_COREML_BATCH_SIZE = 32`** — optimal for Apple Neural Engine (ANE).
  Benchmark results on a 19k-chunk corpus show that larger batches (64, 128)
  consume 7–10% more time and 1.2–9.7 GB additional peak RSS with zero
  throughput gain. The ANE has a fixed dispatch budget; batch size scales
  unified-memory allocation but not per-call throughput.
- **`TRUSTY_COREML_TRIPWIRE_MB = 4096`** — safety net for RSS spikes. If a single
  CoreML embedding batch increases RSS by >4 GB, the batch size is automatically
  halved (floor: 1) and a warning is logged. Fires once per reindex.
  Override with `TRUSTY_COREML_TRIPWIRE_MB` env var if your host has different
  memory pressure characteristics.
- Non-fatal RSS probes: failure to read `/proc/self/status` returns 0, disabling
  the tripwire gracefully rather than crashing.

## Query intent → routing weights

| Intent     | α (vector) | β (BM25) | KG-first |
|------------|------------|----------|----------|
| Definition | 0.3        | 0.7      | false    |
| Usage      | 0.5        | 0.5      | **true** |
| Conceptual | 0.8        | 0.2      | false    |
| BugDebt    | 0.1        | 0.9      | false    |
| Unknown    | 0.6        | 0.4      | false    |

The classifier is a sub-ms regex over the query text. KG expansion is gated
to `Usage` intent only — caller/callee chains are scored at 70% of the
trigger chunk's RRF score.

## CLI

```bash
trusty-search start                                  # start HTTP daemon (background)
trusty-search stop                                   # stop daemon (SIGTERM via PID lockfile)
trusty-search index [path] [--name <id>] [--force]   # register + index (primary command)
                                                     # auto-detects ./trusty-search.yaml
trusty-search query <text> [--index <id>] [--top-k N] [--json]
trusty-search status                                 # daemon + index overview (alias: health)
trusty-search doctor [--fix]                         # 6-check diagnostic + auto-repair
trusty-search ui [--port N]                          # open web management UI in browser
trusty-search convert project|all [--dry-run]        # migrate from mcp-vector-search
trusty-search serve [--http <addr>]                  # MCP stdio (default) or HTTP/SSE
# Aliases preserved for backward compatibility:
trusty-search init [path]                            # alias for index
trusty-search reindex [path]                         # alias for index --force
```

## MCP tools

| Tool            | Description                                          |
|-----------------|------------------------------------------------------|
| `search_code`   | Hybrid search (BM25 + HNSW + KG, RRF-fused)          |
| `search_similar`| Code-to-code similarity from a seed file/function    |
| `index_file`    | Add or replace a single file in the index            |
| `remove_file`   | Remove a file and all its chunks                     |
| `list_indexes`  | Enumerate all registered indexes                     |
| `create_index`  | Register a new (empty) index                         |
| `delete_index`  | Drop an index from the registry                      |
| `reindex`       | Fire-and-forget full reindex (SSE progress)          |
| `index_status`  | Per-index chunk count and root path                  |
| `list_chunks`   | Paginated enumeration of chunks `(file, start_line)` |
| `search_health` | Daemon liveness probe                                |
| `chat`          | OpenRouter Q&A with auto-injected search context     |

## Stack

| Component       | Choice                                              |
|-----------------|-----------------------------------------------------|
| Language        | Rust 2021                                           |
| Async runtime   | tokio (full features)                               |
| HTTP            | axum 0.7 + tower-http (CORS, trace, gzip), HTTP/2   |
| Vector store    | usearch 2.25 (HNSW, in-memory, `Arc<RwLock<>>`)     |
| Embeddings      | fastembed 5.x (ONNX, all-MiniLM-L6-v2 INT8, 384-dim)|
| Lexical         | Custom BM25 (zero-dep port, camelCase splitting)    |
| KV store        | redb 2.6                                            |
| Knowledge graph | petgraph 0.6 (`SymbolGraph`)                        |
| File watching   | notify 6 + notify-debouncer-mini 0.4 (500 ms)       |
| Code parsing    | tree-sitter 0.26 (14 grammars)                      |
| Concurrency     | dashmap 5, lru 0.12, rayon 1                        |
| HTTP client     | reqwest 0.12 (rustls-tls)                           |
| CLI             | clap 4 (derive)                                     |
| UI              | Svelte 5, embedded via `include_dir!`               |
| Hashing         | sha2 (incremental reindex fingerprints)             |

## Troubleshooting

**Daemon won't start**

Run `trusty-search doctor` for a 6-check diagnostic. Common causes:
- Another daemon already running: `trusty-search stop` then `trusty-search start`
- Stale PID lockfile: `trusty-search doctor --fix` removes it automatically
- Less than 16 GB RAM: the daemon performs a hard RAM check and exits with an actionable error. Set `TRUSTY_SKIP_RAM_CHECK=1` in the daemon environment to bypass for small workloads; not recommended on large corpora (risk of OOM during indexing)

**Embedder stuck on "initializing"**

The ONNX Runtime initializes the model on first start and may take 30–60 seconds on slower machines. If it hangs indefinitely, increase the timeout:

```bash
TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS=120 trusty-search start
```

**High memory usage during reindex**

The daemon has a soft RSS ceiling (`TRUSTY_MEMORY_LIMIT_MB`). When hit, it skips remaining batches and logs a warning. Already-committed chunks stay searchable. To lower pressure:

```bash
TRUSTY_MEMORY_LIMIT_MB=2048 trusty-search start
```

Or wait for the soft cap to trip — the partial index is usable immediately.

**Port conflict**

The daemon auto-selects a free port on each start. The live port is written to:
- macOS: `~/Library/Application Support/trusty-search/port.lock`
- Linux: `$XDG_DATA_HOME/trusty-search/port.lock`

If `trusty-search status` reports the wrong port, stop and restart the daemon.

**Device flag not persisting across restarts**

Use `trusty-search start --device cpu` to force CPU mode. The flag is persisted to `daemon.env` so it survives daemon restarts.

## Architecture and HTTP API

See [CLAUDE.md](./CLAUDE.md) for the full HTTP endpoint catalogue, query
pipeline, multi-request design, memory tuning reference, and release process.

## Documentation

- [CLAUDE.md](./CLAUDE.md) — full architecture + HTTP API reference
- [CHANGELOG.md](./CHANGELOG.md) — release history
- [docs/examples/trusty-search.yaml](./docs/examples/trusty-search.yaml) — multi-index repo config
- [docs/research/](./docs/research/) — design + comparison documents

## License

[Elastic License 2.0 (ELv2)](./LICENSE) — free for internal use; you may not
provide trusty-search as a hosted or managed service to third parties without
a commercial agreement. See [LICENSE](./LICENSE) for the full terms.
