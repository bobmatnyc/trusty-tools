# trusty-search

Machine-wide, blazingly fast hybrid code search service. Single install per machine,
serves multiple named indexes (one per project) via HTTP daemon and MCP server.

> **Coordination:** Shared library patterns, consistent conventions, and CI/CD configuration for this project are managed by [trusty-common](../trusty-common). See that repo's CLAUDE.md for cross-project guidelines.

## Project Goals

- **Machine-wide service**: one install (`cargo install trusty-search`), one daemon
  per machine, serves all projects on the box
- **Multiple named indexes**: each project registers an `IndexId`; one daemon manages
  all of them concurrently
- **Hybrid search**: BM25 (lexical) + HNSW vector (semantic) + Knowledge Graph
  expansion, fused via Reciprocal Rank Fusion (RRF, k=60, parameter-free)
- **Query-type routing**: classify intent (Definition / Usage / Conceptual / BugDebt /
  Unknown) and route to optimal weighting before searching
- **MCP server**: stdio + HTTP/SSE for Claude Code integration
- **Zero cold-start**: HNSW stays hot (Duration::MAX cool-after), LRU embedding cache
  (256 entries) skips re-embedding on repeated queries
- **Native multi-request**: `Arc<SearchAppState>`, concurrent reads via `RwLock`,
  axum HTTP/2 — many concurrent readers never block each other
- **Zero dependency on trusty-memory**: bundles its own storage layer
  (redb + usearch + fastembed) — `cargo install trusty-search` works standalone

## Architecture

```
Machine-wide service (single install, one daemon per machine)
  └── IndexRegistry: DashMap<IndexId, Arc<IndexHandle>>
        └── IndexHandle
              ├── CodeIndexer: Arc<RwLock<HnswIndex>> (usearch) — concurrent reads
              │     ├── parse_and_embed_files()  — runs outside write lock (parse + embed)
              │     └── commit_parsed_batch()    — holds write lock only for redb+HNSW commit
              ├── BM25Builder: per-query, built from chunk corpus
              ├── KnowledgeGraph: Arc<SymbolGraph> (petgraph, tree-sitter derived)
              ├── FileWatcher: notify-debouncer-mini, 500ms debounce
              └── QueryCache: Arc<Mutex<LruCache<QueryHash, Vec<f32>>>> — skip embedding on repeat
```

### Query Pipeline

1. **Classify intent**: `QueryClassifier` (sub-ms regex) →
   `Definition / Usage / Conceptual / BugDebt / Unknown`
2. **Route weights**: `alpha` (vector), `beta` (BM25), `use_kg_first`
3. **Search**: 4×top_k HNSW candidates + per-query BM25 index over chunk corpus
4. **Fuse**: Reciprocal Rank Fusion (k=60, parameter-free)
5. **KG expand**: 1–2 hop `callers_of` / `callees_of` via `SymbolGraph`,
   scored at 70% of trigger chunk's RRF score
6. **Return**: compact (7-line snippet) or full chunk

### Query Intent → Routing Weights

| Intent      | alpha (vector) | beta (BM25) | use_kg_first |
|-------------|----------------|-------------|--------------|
| Definition  | 0.3            | 0.7         | false        |
| Usage       | 0.5            | 0.5         | true         |
| Conceptual  | 0.8            | 0.2         | false        |
| BugDebt     | 0.1            | 0.9         | false        |
| Unknown     | 0.6            | 0.4         | false        |

### CodeChunk

```rust
pub struct CodeChunk {
    pub id: String,                       // "{path}:{start}:{end}" — collision-safe
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub function_name: Option<String>,
    pub score: f32,
    pub compact_snippet: Option<String>,  // 7-line snippet for token-efficient output
    pub match_reason: String,             // "hybrid", "hybrid+kg", "bm25", "vector", "fallback:ripgrep"
}
```

### HTTP API (axum, single daemon, multi-index)

**Audience**: integrators (e.g. open-mpm) calling the daemon's REST API.

**Transport conventions** (apply to every endpoint below):

- **Base URL**: `http://127.0.0.1:<port>` — daemon binds loopback only; resolve
  the live port from `~/Library/Application Support/trusty-search/port.lock` (macOS)
  or `$XDG_DATA_HOME/trusty-search/port.lock` (Linux).
- **Authentication**: none. The daemon is localhost-only and trusts every caller;
  do **not** bind it to a non-loopback interface.
- **Content-Type**: `application/json` for all request and response bodies (SSE
  endpoint excepted — it returns `text/event-stream`).
- **Error response**: any 4xx / 5xx returns a JSON body of the shape
  `{ "error": "<message>" }`. Status codes follow standard HTTP semantics
  (`404` = unknown `index_id`, `503` = subsystem disabled / not configured,
  `500` = internal error).
- **CORS**: permissive (`*`) for browser-based admin UIs.
- **Gzip**: responses are gzipped when `Accept-Encoding: gzip` is set.

#### Endpoint catalogue

##### `GET /health`

Liveness + readiness probe. Used by `trusty-search status`, `trusty-search doctor`,
and external process detectors (open-mpm) to decide whether to spawn their own
daemon.

- **Request body**: none.
- **Response 200**:
  ```json
  { "status": "ok", "version": "0.1.0", "indexes": 3 }
  ```
  - `status`: `"ok"` normally; `"degraded"` when at least one registered index
    has a failed search lane (issue #1870) OR at least one index's file
    watcher was refused because its root is network-mounted (issue #3408 —
    see `indexes_watcher_network_degraded` below).
  - `version`: `CARGO_PKG_VERSION` of the running binary.
  - `indexes`: number of indexes currently registered in the in-memory registry.
  - `indexes_watcher_network_degraded` (issue #3408): count of registered
    indexes whose file watcher was refused because `root_path` was detected as
    a network-mounted filesystem (NFS/EFS/SMB/CIFS). inotify/FSEvents cannot
    observe writes made by a *different* host onto a shared network mount —
    an OS-level limitation, not a bug — so starting the watcher there would
    leave the daemon reporting healthy while silently never reacting to
    cross-host changes. A non-zero count means those indexes must be kept in
    sync via `POST /indexes/:id/index-file` / `POST /indexes/:id/remove-file`
    instead — see "Supported network-mount pattern" under
    `POST /indexes/:id/remove-file` below. Per-index detail (which index, and
    the actionable message) is on `GET /indexes/:id/status`'s `watcher` field.

##### `GET /indexes`

List every registered index.

- **Request body**: none.
- **Response 200**:
  ```json
  { "indexes": ["my-project", "trusty-search", "open-mpm"] }
  ```

##### `POST /indexes`

Register a new (empty) index. Idempotent: re-registering an existing id returns
`created: false` rather than an error.

- **Request body**:
  ```json
  { "id": "my-project", "root_path": "/Users/me/code/my-project" }
  ```
- **Response 200** (created):
  ```json
  { "id": "my-project", "created": true }
  ```
- **Response 200** (already existed):
  ```json
  { "id": "my-project", "created": false, "reason": "already exists" }
  ```

##### `DELETE /indexes/:id`

Drop an index from the in-memory registry. On-disk redb data is preserved —
re-registering with the same id will reuse it.

- **Request body**: none.
- **Response 200**: `{ "id": "my-project", "removed": true }`

##### `GET /indexes/:id/status`

Per-index stats.

- **Request body**: none.
- **Response 200**:
  ```json
  {
    "index_id": "my-project",
    "root_path": "/Users/me/code/my-project",
    "chunk_count": 14823,
    "watcher": {
      "active": true,
      "network_mount_degraded": false,
      "degraded_reason": null
    }
  }
  ```
  - `watcher` (issue #3408): `active` is whether a live OS-level watcher is
    currently running for this index. `network_mount_degraded` is `true` when
    the watcher was refused because `root_path` was detected as
    network-mounted; `degraded_reason` then carries the full actionable
    message (names the `index-file`/`remove-file` endpoints). Both are
    `false`/`null` for a normal local-disk index.
- **Response 404**: unknown `index_id`.

##### `POST /indexes/:id/search`

Hybrid search (BM25 + vector + KG expansion + RRF fusion).

- **Request body**:
  ```json
  {
    "text": "fn authenticate",
    "top_k": 10,
    "expand_graph": true,
    "compact": true,
    "branch_files": ["src/auth.rs", "src/middleware.rs"],
    "branch_boost": 1.5,
    "branch": "feature/new-auth"
  }
  ```
  - `text` (required): the query string.
  - `top_k` (optional, default `10`): max results to return.
  - `expand_graph` (optional, default `true`): perform 1–2 hop KG expansion on top hits.
  - `compact` (optional, default `true`): include `compact_snippet` (7-line) in each chunk.
  - `branch_files` (optional): files modified on the current git branch
    (relative to the index `root_path`). Chunks whose `file` appears here have
    their RRF score multiplied by `branch_boost`. Leading `./` is stripped
    before comparison. Issue #122.
  - `branch_boost` (optional, default `1.5`, range `[1.0, 3.0]`): score
    multiplier applied to branch-modified chunks. `1.0` disables boosting.
    Values outside the range are clamped server-side. Issue #122.
  - `branch` (optional): branch name hint. When `branch_files` is absent, the
    daemon shells out to `git merge-base HEAD <branch>` followed by
    `git diff --name-only <base>..HEAD` inside the index `root_path` to
    derive the file list. Failure is non-fatal: a `tracing::warn!` is logged
    and search proceeds with no boost. Issue #122.
- **Response 200**:
  ```json
  {
    "results": [
      {
        "id": "src/auth.rs:42:78",
        "file": "src/auth.rs",
        "start_line": 42,
        "end_line": 78,
        "content": "fn authenticate(...) { ... }",
        "function_name": "authenticate",
        "score": 0.0184,
        "compact_snippet": "fn authenticate(...) {\n  ...\n}",
        "match_reason": "hybrid+kg",
        "on_branch": true
      }
    ],
    "intent": "Definition",
    "latency_ms": 7
  }
  ```
  - `intent`: one of `"Definition" | "Usage" | "Conceptual" | "BugDebt" | "Unknown"`.
  - `match_reason`: one of `"hybrid" | "hybrid+kg" | "bm25" | "vector" | "fallback:ripgrep"`.
  - `on_branch` (issue #122): `true` when the chunk's file appears in the
    branch-modified file set resolved for this query (either explicitly via
    `branch_files` or derived from `branch`). Always `false` when no branch
    context was provided. Lets clients highlight branch work in the UI
    without re-doing the lookup.

##### `POST /indexes/:id/search_similar`

Code-to-code similarity: find chunks similar to a known file/function.

- **Request body**:
  ```json
  { "file": "src/auth.rs", "function": "authenticate", "top_k": 10 }
  ```
  - `function` (optional): when omitted, uses the first chunk of the file as seed.
  - `top_k` (optional, default `10`).
- **Response 200**:
  ```json
  {
    "results": [/* CodeChunk[] */],
    "seed_chunk_id": "src/auth.rs:42:78",
    "latency_ms": 4
  }
  ```
- **Response 404**: unknown index, or seed chunk not found.

##### `POST /indexes/:id/index-file`

Add or replace one file in the index.

- **Request body**:
  ```json
  { "path": "src/auth.rs", "content": "fn authenticate() { ... }" }
  ```
- **Response 200**:
  ```json
  { "index_id": "my-project", "path": "src/auth.rs", "indexed": true }
  ```

##### `POST /indexes/:id/remove-file`

Remove a file (and all its chunks) from the index.

- **Request body**: `{ "path": "src/auth.rs" }`
- **Response 200**:
  ```json
  { "index_id": "my-project", "path": "src/auth.rs", "removed_chunks": 4 }
  ```

###### Supported network-mount pattern (EFS/NFS/SMB) — issue #3408

**`POST /indexes/:id/index-file` and `POST /indexes/:id/remove-file` are the
officially supported, stability-committed incremental-indexing path for
network-mounted (EFS/NFS/SMB/CIFS) and build/serve-split deployments.** This
is not a fallback or a workaround — it is the correct integration point
whenever the index root is not a local disk local to the daemon's host.

Why this is necessary (rather than just a suggestion): the native file watcher
(`service/watcher.rs`, backed by `notify`'s inotify/FSEvents) is a *local-host
kernel notification* mechanism. It cannot observe a write made by a
*different* host onto a shared network mount — this is a limitation of
inotify/FSEvents themselves, not something this crate can fix by retrying or
reconfiguring the debouncer. A build box that writes to a shared EFS/NFS mount
will never wake a serving box's inotify watch. As of issue #3408 the daemon
detects a network-mounted index root (via `statfs` — see
`service/network_fs.rs`) and refuses to start the watcher for that index
rather than silently reporting healthy while never reacting to changes; see
`indexes_watcher_network_degraded` (`GET /health`) and the `watcher` field
(`GET /indexes/:id/status`) above.

Equivalent MCP tools: `index_file` (`{ index_id, path, content }`) and
`remove_file` (`{ index_id, path }`) — see "MCP Tools" below.

**Worked example — a build box pushes to shared storage; each consumer (e.g. a
CI job, a git post-merge hook, or a small sidecar poller) detects its own
changes and drives the daemon directly:**

```bash
#!/usr/bin/env bash
# Run after `git pull` (or any local mutation of the network-mounted root),
# from a process that has its own view of what changed — e.g. `git diff`
# against the previous HEAD, an fswatch on the LOCAL side of a build step, or
# a CI job's own manifest of touched files. Do NOT rely on the daemon's
# watcher to discover these — see the "why" above.
set -euo pipefail
INDEX_ID="my-project"
DAEMON="http://127.0.0.1:$(trusty-search port)"

# IFS=$'\t': git's --name-status output is TAB-delimited (not generic
# whitespace), so this also handles paths containing spaces correctly. A
# rename line has a THIRD field ("R100<TAB>old_path<TAB>new_path"); reading
# into three variables leaves $new_path empty for D/A/M lines and populated
# only for renames.
git diff --name-status "$PREV_HEAD" HEAD | while IFS=$'\t' read -r status path new_path; do
  case "$status" in
    D)
      curl -sf -X POST "$DAEMON/indexes/$INDEX_ID/remove-file" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg path "$path" '{path: $path}')"
      ;;
    A|M)
      content=$(jq -Rs . < "$path")
      curl -sf -X POST "$DAEMON/indexes/$INDEX_ID/index-file" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg path "$path" --argjson content "$content" \
              '{path: $path, content: $content}')"
      ;;
    R*)
      # Rename (any similarity index, e.g. R100 or R087): $path is the OLD
      # path, $new_path is the new one. git diff --name-status gives no
      # chunk-level delta for a rename, so remove the old path's chunks and
      # index the new path fresh — simplest and correct.
      curl -sf -X POST "$DAEMON/indexes/$INDEX_ID/remove-file" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg path "$path" '{path: $path}')"
      content=$(jq -Rs . < "$new_path")
      curl -sf -X POST "$DAEMON/indexes/$INDEX_ID/index-file" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg path "$new_path" --argjson content "$content" \
              '{path: $path, content: $content}')"
      ;;
  esac
done
```

Note: `index_file` triggers a full `rebuild_symbol_graph` per call
(`core/indexer/ingest/mod.rs`) — fine for a handful of files per push. There is
currently no HTTP/MCP-exposed batch variant (the internal
`index_files_batch_no_rebuild` fast path is only used by the full-reindex
pipeline); a consumer that needs to apply a LARGE batch of per-file changes at
once (e.g. after pulling hundreds of commits) should prefer
`POST /indexes/:id/reindex` over looping many single-file `index-file` calls.

##### `POST /indexes/:id/reindex`

Fire-and-forget full reindex. Returns immediately with an SSE stream URL; poll
`GET /indexes/:id/reindex/stream` for progress.

- **Request body** (all fields optional):
  ```json
  { "root_path": "/Users/me/code/my-project", "force": false }
  ```
  - `root_path`: override the path stored on the handle (lets CLI register + reindex in one call).
  - `force`: when `true`, clear the per-index content-hash cache so every file is re-embedded.
- **Response 200**:
  ```json
  {
    "index_id": "my-project",
    "queued": true,
    "stream_url": "/indexes/my-project/reindex/stream"
  }
  ```

##### `GET /indexes/:id/reindex/stream`

SSE stream of reindex progress. **Content-Type**: `text/event-stream` (not JSON).

Event payloads are JSON strings, one per SSE `data:` line, with shapes:

```json
{ "event": "start",    "total_files": 14823 }
{ "event": "progress", "indexed": 1024, "total": 14823, "current_file": "src/auth.rs" }
{ "event": "complete", "indexed": 14823, "elapsed_ms": 142000 }
{ "event": "error",    "message": "<error>" }
```

The handler replays any buffered events to late subscribers before streaming
live updates, so a subscriber that connects after `start` still sees it.

- **Response 404**: no reindex has been queued for this index.

##### `GET /indexes/:id/chunks?offset=&limit=&after=`

Paginated enumeration of all chunks. Two modes:

- **Offset mode** (default, issue #54): stable `(file, start_line)` order; the
  slice `[offset .. offset+limit]`. Simple but O(N log N) per page — it
  materialises and sorts the whole corpus each call, so deep offsets on large
  indexes are slow (and previously 502'd, issue #1325). `next_cursor` is always
  `null` in this mode.
- **Cursor mode** (issue #1325, preferred for deep / bulk enumeration): opt in
  by sending the `after` param. Pages by chunk `id` via an indexed redb B-tree
  seek — O(page) regardless of depth. Send `after=` (empty) to start from the
  first chunk; pass the response's `next_cursor` back as `after` to walk
  forward. `next_cursor` is `null` once the corpus is exhausted. `offset` is
  ignored. Cursor order (by `id`) differs from offset order — pick one mode and
  page consistently; do not seed a cursor walk from an offset page.

- **Query params**:
  - `offset` (optional, default `0`) — offset mode only.
  - `limit` (optional, default `100`, clamped to `1000`).
  - `after` (optional) — cursor mode. Empty string = from the first chunk;
    otherwise the opaque cursor (a chunk `id`) from a prior `next_cursor`.
- **Response 200**:
  ```json
  {
    "index_id": "my-project",
    "total": 14823,
    "offset": 0,
    "limit": 100,
    "chunks": [/* CodeChunk[] */],
    "next_cursor": "src/zzz.rs:10:42"
  }
  ```
  `next_cursor` is the chunk `id` to pass as the next `after`, or `null` when
  the page was the last one (cursor mode) / always `null` (offset mode).

##### Complexity / smells / quality endpoints

> **Moved to trusty-analyze (issue #71).** As of v0.2.0, `CodeChunk` no longer
> carries `complexity_score`, `complexity`, or `blame` fields. Complexity
> hotspots, code-smell findings, and aggregate quality grades are now served by
> the standalone **trusty-analyze** service, which owns the canonical
> cyclomatic / Halstead / cognitive metrics and git-blame integration. The
> previous `GET /indexes/:id/complexity_hotspots`, `GET /indexes/:id/smells`,
> and `GET /indexes/:id/quality` endpoints are not implemented in
> trusty-search.

##### `GET /facts?subject=&predicate=&object=`

Query the optional facts store (used by the KG/IA pipeline). Any combination of
the three filters is allowed; omitted filters match anything.

- **Response 200**:
  ```json
  { "facts": [/* FactRecord[] */], "count": 17 }
  ```
- **Response 503**: facts store not configured.

##### `POST /facts`

Upsert a fact.

- **Request body**:
  ```json
  {
    "subject": "fn authenticate",
    "predicate": "calls",
    "object": "fn verify_token",
    "index_id": "my-project",
    "confidence": 1.0,
    "provenance": ["src/auth.rs:42"]
  }
  ```
  - `confidence` (optional, default `1.0`).
  - `provenance` (optional, default `[]`).
- **Response 200**: `{ "id": 1234567890, "upserted": true }`
- **Response 503**: facts store not configured.

##### `DELETE /facts/:id`

Delete a fact by its u64 hash id.

- **Response 200**: `{ "id": 1234567890, "removed": true }`

##### `POST /chat`

OpenRouter conversational Q&A with auto-injected search context. Requires
`OPENROUTER_API_KEY` in the daemon's environment.

- **Request body**:
  ```json
  {
    "index_id": "my-project",
    "message": "How does authentication work?",
    "history": [
      { "role": "user",      "content": "..." },
      { "role": "assistant", "content": "..." }
    ]
  }
  ```
- **Response 200**: forwarded OpenRouter chat-completion payload.
- **Response 503**: `{ "error": "OpenRouter not configured" }` (API key missing).

##### `GET /ui`, `GET /ui/`, `GET /ui/*path`

Serves the embedded Svelte admin UI. Not part of the integration contract.

### MCP Tools

The MCP server registers **18 tools** (authoritative source:
`src/mcp/tools.rs` `tool_definitions`):

- `search` — hybrid search (BM25 + vector + KG, RRF-fused)
- `search_lexical` — BM25-only lexical search
- `search_semantic` — vector-only semantic search
- `search_kg` — knowledge-graph expansion search
- `search_all` — fan-out across every registered index
- `search_similar` — code-to-code similarity from a seed file/function
- `index_file` — add/update one file
- `remove_file` — remove one file
- `list_indexes` — enumerate registered indexes
- `create_index` — register a new index
- `delete_index` — delete an index
- `reindex` — trigger full reindex
- `index_status` — per-index stats
- `search_health` — daemon liveness
- `list_chunks` — paginated enumeration of an index's chunks
- `get_call_chain` — KG caller/callee chain for a symbol
- `grep` — literal/regex grep fallback over the corpus
- `chat` — OpenRouter conversational Q&A

## Stack

- **Language**: Rust 2021
- **Async runtime**: tokio (full features)
- **HTTP**: axum 0.7 + tower-http (CORS, trace, gzip), HTTP/2
- **Vector store**: usearch 2.25 (HNSW), wrapped in `Arc<RwLock<>>` for concurrent reads
- **Embeddings**: fastembed 5.x (ONNX, all-MiniLM-L6-v2, 384-dim, SIMD/AVX2/NEON)
- **Lexical**: BM25 (zero-dep port from open-mpm `src/context/bm25.rs`)
- **KV store**: redb 2.6 (chunk metadata, file→chunks mapping, `_meta` schema version)
- **File watching**: notify 6 + notify-debouncer-mini 0.4 (500ms debounce, fsevent)
- **Code parsing**: tree-sitter 0.24 (rust, python, js, ts, go, java, c, cpp)
- **Graph**: petgraph 0.6 (`SymbolGraph` for callers_of / callees_of)
- **Concurrency**: dashmap 5 (`IndexRegistry`), lru 0.12 (embedding cache),
  rayon 1 (parallel chunk hashing)
- **Serde**: serde + serde_json
- **Errors**: anyhow (app), thiserror (lib)
- **Tracing**: tracing + tracing-subscriber (env-filter)
- **CLI**: clap 4 (derive)
- **HTTP client**: reqwest 0.12 (rustls-tls, no native-tls dependency)
- **Progress display**: indicatif (progress bars during reindex)
- **Embedded assets**: include_dir (Svelte admin UI compiled into binary)
- **Content hashing**: sha2 (stable file fingerprints for incremental reindex skip)

## Schema Versioning and Migrations

trusty-search uses a forward-only, idempotent migration framework to evolve the
redb corpus schema across releases without requiring manual reindexing.

### How it works

Each redb corpus database contains a `_meta` table (keyed `&str → &[u8]`) that
stores a 4-byte little-endian `u32` under the key `"schema_version"`. Legacy
databases without this table are treated as version 0.

On daemon startup (after `restore_indexes`), the framework:
1. Reads each index's stored `schema_version` from redb.
2. Computes the migration chain: all registered migrations whose
   `source_version >= current`.
3. Applies each migration in sequence, writing the new `schema_version` to redb
   **after** each successful `apply` (crash-safe: a crash before the write
   causes a retry on next startup; idempotent `apply` implementations make
   retries safe).
4. Runs per-index migrations in background `tokio::spawn` tasks — the daemon
   continues serving queries while migration runs.

Set `TRUSTY_DISABLE_MIGRATIONS=1` to skip auto-migrations.

### Adding a new migration

1. Create `src/core/migration/m00N.rs` implementing `Migration` (`source_version`,
   `target_version`, `description`, `apply`). The `apply` method must be
   idempotent.
2. Register it in `MigrationRegistry::new()` in `src/core/migration/mod.rs`.
3. Increment `CURRENT_SCHEMA_VERSION` in the same file.
4. Add unit tests in `m00N::tests` (especially idempotency) and update
   `CHANGELOG.md`.

### Key types

| Type | Location | Purpose |
|---|---|---|
| `CURRENT_SCHEMA_VERSION` | `core::migration` | Monotonic `u32`; bump per migration |
| `Migration` (trait) | `core::migration` | `source_version`, `target_version`, `description`, `apply` |
| `MigrationRegistry` | `core::migration` | Ordered list of all migrations; `chain_from(v)` |
| `run_migrations` | `core::migration` | Runner: reads version, applies chain, writes version |
| `META_TABLE` | `core::migration` | redb `_meta` table definition |
| `M001PerPubConstRust` | `core::migration::m001` | Re-chunk Rust `pub const`/`pub static` (issue #143) |

---

## Multi-Request Design

- `Arc<SearchAppState>` shared across all axum handlers
- `DashMap<IndexId, Arc<IndexHandle>>` is shard-locked — different indexes never
  contend for locks
- `IndexHandle.indexer: Arc<RwLock<CodeIndexer>>` — reader-priority RwLock; many
  concurrent searches against the same index never block each other
- Indexing operations use `tokio::sync::Semaphore` to prevent thread-pool starvation
  (carry-over fix from open-mpm BUG-2)
- HTTP/2 multiplexing: a single client connection can issue many concurrent searches

## Performance Targets

- **Sub-10ms p50 warm query** on a 100k-chunk index
- **10× faster than ripgrep** on whole-repo conceptual queries
- **HNSW pre-warmed**: index loaded at daemon start, never paged out
  (`Duration::MAX` cool-after)
- **LRU embedding cache** (256 entries): repeated queries skip the embedder entirely
- **~2–3 min for a 14k-file repo** (4 optimizations: INT8 quantized model
  `AllMiniLML6V2Q`, batch upsert into HNSW, split lock via
  `parse_and_embed_files` / `commit_parsed_batch`, batch size 512)

## Embedder Configuration (Environment Variables)

`trusty-search start` selects an embedding back-end based on the `TRUSTY_EMBEDDER`
environment variable (issue #110 Phase 2):

| `TRUSTY_EMBEDDER` value | Behaviour |
|-------------------------|-----------|
| unset / `auto`          | **Default, platform-aware (epic #3524 slice 6, PR 3/5).** Resolves via `resolve_default_embedder_mode()`: on Apple Silicon with `TRUSTY_PY_DEFAULT` enabled, serves on the ort stdio sidecar immediately while bootstrapping the python/MPS sidecar in the background and hot-swapping when ready (see "Graceful Apple-Silicon default" below); with `TRUSTY_EMBEDDER_PYTHON_EAGER` enabled, takes the eager blocking `python` path below (any platform); otherwise (the shipped default everywhere today, since `TRUSTY_PY_DEFAULT` defaults OFF) arms a `LazyEmbedderHandle` at boot (issue #315 — deferred spawn): `trusty-embedderd --stdio` is spawned on the **first embed request** (reindex, hybrid search, `context_inference`), not at daemon startup. |
| `stdio`                 | **Permanent opt-out.** Always the plain ort stdio sidecar (identical to the unset/`auto` fallback above), even after `TRUSTY_PY_DEFAULT` ships on for real — never routed through the platform-aware resolution. `trusty-embedderd` is a **required runtime dependency** for this and the default ort path — binary discovery runs at boot and fails fast with an install hint if the binary is missing. **`cargo install trusty-search` installs `trusty-embedderd` automatically.** |
| `python`                | **Opt-in Python/MPS sidecar (epic #3524).** Eagerly bootstraps a pinned `uv`-managed venv (torch + sentence-transformers) at `start`, then lazy-spawns `trusty-embedderd-py` speaking the exact same stdio JSON-RPC 2.0 protocol as the Rust sidecar. On Apple Silicon this embeds ~2.4x faster than the ort path with numerically identical (>=0.999 cosine) results. On ANY bootstrap or launcher-discovery failure, falls back to the Rust ort stdio sidecar so search never hard-fails — see "Python/MPS sidecar tuning" below. **Default-off**; requires `uv` installed (or `TRUSTY_UV_BIN` set) and ~3 GB free disk on first use. |
| `in-process` / `local`  | Explicit escape hatch — in-process ONNX embedding. Use for tests, debugging, or environments where the sidecar cannot be installed. **Never activated silently**: you must set this variable explicitly to use the in-process path. |
| `http://…`              | HTTP remote — `POST /embed` to a manually-managed `trusty-embedderd` HTTP listener. |
| `unix:/path/to/sock`    | UDS remote — JSON-RPC 2.0 to a manually-managed `trusty-embedderd --socket` listener. |
| `candle`                | Candle Metal backend (requires `--features candle`). |

### Graceful Apple-Silicon default (epic #3524 slice 6, PR 3/5)

On Apple Silicon, once ship-gated on, `trusty-search start` serves on the ort
stdio sidecar immediately (zero HTTP-listener delay) while a detached
background task bootstraps the python/MPS sidecar, proves it with one real
readiness-probe embed call, and hot-swaps the running embedder over to it.
On any bootstrap or probe failure the daemon stays permanently on ort for
that daemon's lifetime and `/health`'s `embedder_bootstrap` reports
`"failed"`. Ships **default OFF** in this PR — a later slice flips the
default on after a soak period.

| Variable | Default | Purpose |
|----------|---------|---------|
| `TRUSTY_PY_DEFAULT` | unset (off) | **Ship-gate.** Set to `1`/`true`/`yes`/`on` to enable the graceful background path on Apple Silicon for unset/`auto` `TRUSTY_EMBEDDER`. Unset/falsy leaves Apple-Silicon resolution completely unchanged (ort) — this is why the PR that introduces the mechanism ships with zero user-facing behavior change. Has no effect off Apple Silicon. |
| `TRUSTY_EMBEDDER_PYTHON_EAGER` | unset (off) | Reaches the existing eager, blocking `python` arm (identical to explicit `TRUSTY_EMBEDDER=python`) via unset/`auto` instead of an explicit value. Not platform-gated. `TRUSTY_PY_DEFAULT` wins outright when both are set. |
| `TRUSTY_PY_BOOTSTRAP_RETRIES` | `2` | Number of bootstrap→probe attempts the background orchestrator makes (linear backoff between attempts) before giving up and marking the bootstrap `Failed` while staying on ort. A malformed or `0` value falls back to the default. |

### Python/MPS sidecar tuning (`TRUSTY_EMBEDDER=python` path only)

| Variable | Default | Purpose |
|----------|---------|---------|
| `TRUSTY_UV_BIN` | — (PATH search) | Explicit path to the `uv` binary used to bootstrap the venv (`uv python install`, `uv venv`, `uv export`, `uv pip sync`). Must point to an existing file if set; otherwise `uv` is located on `PATH`. Missing `uv` is a bootstrap failure that triggers the ort fallback. |
| `TRUSTY_EMBEDDERD_PY_BIN` | — (sibling/PATH search) | Explicit path to the `trusty-embedderd-py` launcher binary. Overrides the sibling-binary/PATH discovery `locate_launcher_binary` performs. |
| `TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS` | `600` | Bounded timeout applied to each individual `uv` bootstrap step (python install, venv create, export, pip sync) and to the post-build import+embed smoke test. Each step gets one retry on a transient failure. |
| `TRUSTY_DEVICE` | `auto` | Device selection inside the sidecar: `auto` (MPS if available, else CUDA, else CPU), `gpu` (same as `auto` but logs a warning if it falls back to CPU), or `cpu` (force CPU). |
| `TRUSTY_PY_EMBED_FP16` | unset (fp32) | Set to `1`/`true`/`yes`/`on` to opt into fp16 on MPS/CUDA (~1.3x faster; cosine similarity still >= 0.9999 vs the fp32 reference per the spike). fp32 is the default everywhere, including MPS/CUDA. |
| `TRUSTY_PY_EMBED_BATCH_SIZE` | unset | Python-sidecar-specific batch-size override; wins over the forwarded `TRUSTY_EMBED_BATCH_SIZE`. Clamped to at most 512 on MPS (Apple Silicon unified memory) and to at least 1 everywhere; falls back to 256 when unset and nothing is forwarded. |
| `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS` | `1800` | **Python-arm idle-shutdown override (epic #3524 fast-follow).** The shared `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` default (300s) is tuned for the lightweight Rust ort sidecar; the Python/MPS sidecar's cold restart is cheap (~2.5–3s: torch import + model load + one MPS warmup) but still worth avoiding mid-session, so this arm defaults to **1800s (30 min)** instead — the sidecar survives a typical work session's think-time gaps while still reclaiming its ~500 MB after genuine extended idle (matters on the 16 GB minimum-spec tier). Resolution precedence: this var (if set, including `0`) always wins; else an explicitly-set shared `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` (any value, including `0`) is honoured; else the python-specific 1800s default applies. Set `0` for always-warm (idle-shutdown disabled) on higher-RAM machines. Has no effect on the default ort/`stdio` arm. |

### Supervisor tuning (stdio-sidecar path only)

| Variable | Default | Purpose |
|----------|---------|---------|
| `TRUSTY_EMBEDDERD_BIN` | — | Explicit path to the `trusty-embedderd` binary. Overrides PATH search. Must point to an existing file if set. |
| `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` | `30` | How long to wait for the sidecar's readiness probe. |
| `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` | `60` | Exponential back-off ceiling between crash restarts. |
| `TRUSTY_EMBEDDERD_MAX_RESTARTS` | `5` | Maximum restarts before the supervisor gives up. |
| `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` | `300` | **Idle shutdown (issue #315; default flipped to 300s in #2315).** After this many seconds with no embed request the sidecar is SIGTERM/SIGKILLed and the spawn gate is reset so the next request triggers a fresh spawn. Defaults to `300` (5 min): an idle `trusty-embedderd` was measured pinning ~2.9 GB RSS indefinitely, so it is now reclaimed at rest; the cold-respawn cost on the next request is ~2–15 s. The watchdog never kills a request mid-flight — an in-flight counter defers eviction until the request completes. Set `0` to explicitly disable idle-shutdown (old always-resident behaviour). |
| `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS` | `120` | **Per-call deadline (fix: reindex-stall).** Maximum seconds to wait for a single `embed_batch` response from the sidecar. Under memory pressure (Apple Silicon unified memory) the sidecar can stall mid-batch; this deadline converts an indefinite hang into a fast error so the reindex task can emit a terminal SSE event and the supervisor can notice a stalled child. Increase only on machines with very large batches or extremely slow model init. |
| `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` | `60` | Overall timeout for the initial embedder init (all modes). |

---

## Memory Tuning (Environment Variables)

> **System requirement: 16 GB RAM minimum.** `trusty-search start` performs
> a hard RAM check at startup (`src/commands/start.rs`) and exits with an
> error on hosts with less than 16 GB. Indexing large codebases on
> under-spec machines is not supported. The legacy `Tiny` (<8 GB) and
> `Small` (8–15 GB) memory tiers have been removed — `Medium` (16–31 GB)
> is now the baseline.

The daemon caps several in-memory structures to keep RAM bounded on
long-running deployments (issue #75). **As of the memory-tier autosizing
change, defaults are computed from detected system RAM at daemon startup**
(`src/core/memory_policy.rs`). Env vars always override the auto-tuned
values; precedence is **shell env > `daemon.env` > tier default**.

### Auto-tuned defaults (per tier)

> **Note:** `MEMORY_LIMIT_MB` is now computed dynamically as
> `clamp(system_RAM × 25%, 1 GiB, 64 GiB)`. The table shows representative
> values per tier. Set `TRUSTY_MEMORY_LIMIT_MB` to override.

`MemoryPolicy::detect()` reads `hw.memsize` (macOS) or `/proc/meminfo`
(Linux) at daemon startup and selects one of five tiers:

| Tier | Total RAM | `MEMORY_LIMIT_MB` | `MAX_CHUNKS` | `EMBEDDING_CACHE` | `MAX_BATCH_SIZE` | `BM25_CORPUS_CAP` | `MAX_KG_NODES` |
|------|-----------|-------------------|--------------|-------------------|------------------|-------------------|----------------|
| Tiny | < 8 GB | ~2 048 (25% of RAM, min 1 024) | 50 000 | 500 | 64 | 20 000 | 30 000 |
| Small | 8–15 GB | ~2 048–3 840 (25% of RAM) | 100 000 | 1 000 | 128 | 50 000 | 75 000 |
| Medium | 16–31 GB | ~4 096–8 192 (25% of RAM) | 200 000 | 5 000 | 256 | 100 000 | 150 000 |
| Large | 32–63 GB | ~8 192–16 384 (25% of RAM) | 400 000 | 10 000 | 512 | 200 000 | 300 000 |
| XLarge | ≥ 64 GB | 65 536 (capped at 64 GiB) | 800 000 | 20 000 | 512 | 400 000 | 500 000 |

The resolved policy is logged at daemon startup so operators can confirm
which tier was selected. If RAM detection fails on an unsupported OS, the
daemon falls back to the Tiny tier (8 GB assumption) with a `tracing::warn!`.

### Per-variable reference

| Variable | Description |
|----------|-------------|
| `TRUSTY_MAX_CHUNKS` | Hard cap on chunks per index. Also clamps HNSW reserve growth, so a single index never holds more than this many vectors. New chunks past the cap are dropped with a warning. |
| `TRUSTY_EMBEDDING_CACHE` | LRU capacity for the in-memory chunk-embedding cache (≈1.5 MB per 1 000 entries at 384-dim f32). Evicted entries are gracefully re-embedded or fall back to relevance-only MMR. |
| `TRUSTY_MAX_BATCH_SIZE` | Hard cap on the embedding batch size used inside `parse_and_embed_files` (chunks per ONNX `embed_batch` call). Auto-derived from `TRUSTY_MEMORY_LIMIT_MB` as `floor(limit_mb × 0.75 / 55)` and clamped to `[32, 512]`, where 55 MB is the empirical ORT transient arena cost per batch slot (issue #95). Tier defaults: Medium (4 GB)=55, Large (8 GB)=111, XLarge (16 GB)=223. Setting this env var explicitly always wins. ORT allocates working memory proportional to batch size *during* each call, so the between-batch RSS poller cannot catch intra-call spikes; this formula bounds the spike below the soft cap. |
| `TRUSTY_BM25_CORPUS_CAP` | Maximum number of live BM25 documents per index. Once reached, new chunks are dropped from the lexical index (the HNSW lane still indexes them). Updates to existing chunks are always honoured. A single warn is logged on first cap hit. |
| `TRUSTY_MAX_KG_NODES` | Maximum number of nodes in the symbol-graph per index. Set to `0` to disable the cap entirely. |
| `TRUSTY_MEMORY_LIMIT_MB` | Soft RSS ceiling. Enforced in two places: (1) the reindex pipeline skips remaining batches with a `tracing::warn!` when hit (partial index preserved; `memory_limit_hit: true` in the SSE `complete` event); (2) the steady-state memory-pressure enforcement ticker (issue #2846) sheds evictable caches across every resident index once RSS crosses `TRUSTY_MEMORY_HIGH_WATER_PCT`% of this ceiling. Auto-tuned to ~25% of host RAM when unset. |
| `TRUSTY_MEMORY_ENFORCE_SECS` | Cadence (seconds) at which the memory-pressure enforcement ticker samples process RSS (issue #2846). Default `30`. `0` disables the ticker entirely (it never spawns) — use when an external cgroup (`MemoryMax`) already caps the process. |
| `TRUSTY_MEMORY_HIGH_WATER_PCT` | High-water mark, as a percent of `TRUSTY_MEMORY_LIMIT_MB`, at which the enforcement ticker begins reclaiming evictable in-memory caches (raw chunk text, BM25 corpus, per-file entities, promoted HNSW heap copy — all durable-corpus-backed and lazily rehydrated, so reclaim is non-destructive). Default `90`, clamped to `[50, 100]`. **Hysteresis:** a sweep only reclaims again once RSS has risen past the RSS observed right after the previous sweep — a host whose steady-state RSS simply sits at/above the mark does not force a full-fleet cache clear on every tick forever; the baseline resets once RSS drops back under the mark. |
| `TRUSTY_MEMORY_RESTART_ON_LIMIT` | Opt-in last resort (issue #2846). When `1`/`true`/`yes`/`on` AND RSS is still at/over the hard `TRUSTY_MEMORY_LIMIT_MB` after a reclaim sweep (i.e. the growth is un-evictable — allocator fragmentation, native ONNX/usearch arenas, or a true leak), the daemon gracefully drains in-flight requests and exits so a supervisor (launchd/systemd) respawns it; warm-boot reloads every index from disk, so no data is lost. **Defaults OFF** — only safe under a supervisor; an unsupervised daemon would not restart. |
| `TRUSTY_CHUNKS_IDLE_EVICT_SECS` | Idle window (seconds) after which a durably-backed index's in-memory `chunks` map (raw `RawChunk` text), BM25 corpus (`doc_terms` — a full tokenized second copy of every chunk's text), and per-file entity map are all evicted to reclaim heap (issue #2162 extended BM25 + entities onto the same window). Default `60` (1 min, lowered from 300s in issue #2166 — see `DEFAULT_CHUNKS_IDLE_EVICT_SECS`). The query hot path materialises results from the mmap-backed redb corpus, so the in-memory copies are only a convenience cache once a durable corpus is wired; evicting them on idle shrinks a quiet daemon to its durable baseline and every in-memory reader (`grep` fallback, `list_chunks`, `get_call_chain`, BM25 search, entity-exact-match) lazily rehydrates from redb on next access. Set `0` to disable eviction entirely. The symbol graph stays hot regardless (rebuilding it is far more expensive than a redb read). A background ticker drives both evictions every 60 s. Indexes without a durable corpus (BM25-only / tests) are never evicted. |
| `TRUSTY_MAX_RESIDENT_INDEXES` | Usage-based resident-index cap (issue #2161). Unset (default) disables the feature entirely — no index is ever evicted at runtime, matching pre-#2161 behaviour. When set to `N`, a periodic sweep (`TRUSTY_RESIDENCY_SWEEP_SECS`) ranks every currently-**resident** index by the same recency key used at boot-time selective warm-boot (`max(last_queried_unix, last_indexed_unix)`, issue #993) and cold-parks everything beyond the top `N` — a **non-destructive** detach that only removes the in-memory `IndexHandle` from the registry; `indexes.toml`, `roots.toml`, and every on-disk artifact (redb corpus, HNSW snapshot) are left untouched. The next query against a parked index reloads it lazily via the same cold-load path a never-yet-warm-booted index uses (subject to `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS`). `0` parks every resident index on the next sweep. An index with an in-flight reindex is never parked. This is the runtime counterpart to `TRUSTY_WARMBOOT_MAX_INDEXES`, which only bounds how many indexes are loaded *eagerly at boot* — without this cap, an index that is queried even once stays resident for the daemon's entire lifetime. |
| `TRUSTY_RESIDENCY_SWEEP_SECS` | Interval (seconds) between residency-cap sweeps (issue #2161). Default `120`. `0` disables the sweep ticker outright (it never spawns), independent of `TRUSTY_MAX_RESIDENT_INDEXES`. Has no effect — the sweep is a cheap per-tick no-op — when `TRUSTY_MAX_RESIDENT_INDEXES` is unset. |
| `TRUSTY_HNSW_MMAP_SERVE` | **Out-of-core quick win #1 (#709).** Controls whether warm-booted HNSW snapshots are served directly from the memory-mapped `Index::view` (low RSS — the OS page cache holds the graph) or eagerly promoted to a heap-resident copy at load time. Default **enabled** (`1`/`true`/`yes`/`on`): search serves from the view and never duplicates the graph onto the heap; promotion to a mutable heap copy happens lazily only on the first *write* (index_file / reindex / add / remove). Set to `0`/`false`/`no`/`off` to opt out — `load_from` then promotes immediately so all serving is heap-resident (higher RSS, but no cold page-fault latency on the first queries). **Trade-off:** mmap serving lowers RSS but the first touch of a cold page faults it in from disk; on EFS/NFS-backed snapshot storage that fault is a network round-trip and can add noticeable tail latency to the first few queries after a restart — opt out on such hosts if cold-start latency matters more than RSS. |
| `TRUSTY_VECTOR_QUANT` | **Out-of-core quick win #2 (#709).** Scalar precision new HNSW indexes are built with: `none` (default, `f32`) \| `f16` (≈2× smaller vectors, small recall cost) \| `i8` (≈4× smaller vectors, larger recall cost). Applied only at index **creation** time and maps onto usearch's `ScalarKind`. The `search` API still takes `f32` queries regardless (usearch quantizes the query internally). **Changing this requires a reindex** — existing snapshots keep the precision they were built with; a forced reindex (`--force` / `reindex`) adopts the new setting. There is no in-place migration. Recall@10 stays within tolerance (≥0.9 in the `ooc_quick_wins` fixture: f16=1.00, i8≈0.96 vs f32=1.00). The whole-snapshot on-disk reduction is diluted by fixed HNSW graph + key-map overhead at small dimensionality but approaches the per-vector 2×/4× ideal at the production 384-dim embedding size. |

Additional internal caps (not env-tunable):

- Per-index file-hash cache: ~200 000 entries (excess shrunk by ~10% on overflow).
- Reindex SSE replay buffer: 500 events (oldest dropped on overflow).
- Reindex progress entries on `SearchAppState`: GC'd 60 s after completion.

## CLI

```bash
trusty-search start                                  # start HTTP daemon (background)
trusty-search stop                                   # stop daemon (SIGTERM via PID lockfile)
trusty-search index [path] [--name <id>] [--force]  # register + index (primary command)
                                                     # auto-detects ./trusty-search.yaml for multi-index repos
                                                     # (see docs/trusty-search/examples/trusty-search.yaml)
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

## Crate Layout

As of v0.3.0, `trusty-search` is a **single crate** with both `[lib]` and
`[[bin]]` targets — the previous `trusty-search-core` / `-service` / `-mcp`
sub-crates were consolidated into three sibling modules under `src/`. The
library target (`trusty_search`) re-publishes `core`, `service`, and `mcp` so
integration tests and downstream consumers can reach the internal APIs.

```
trusty-search/
├── Cargo.toml                       single-crate manifest (lib + bin)
├── build.rs                         Svelte UI build wrapper
├── ui/                              Svelte 5 admin UI sources
├── ui-dist/                         compiled UI bundle (embedded via include_dir!)
├── CLAUDE.md                        this file
├── CHANGELOG.md
├── README.md
├── .open-mpm/agents/                pm.toml, engineer.toml
├── src/
│   ├── lib.rs                       re-publishes `core`, `service`, `mcp`
│   ├── main.rs                      CLI binary entry point
│   ├── commands/                    per-subcommand handlers
│   ├── detect.rs                    project auto-detection
│   ├── doctor.rs                    diagnostic checks
│   ├── core/                        CodeIndexer, BM25, HNSW, chunking, classifier
│   ├── service/                     axum daemon, FileWatcher, client, Svelte UI
│   └── mcp/                         MCP server (stdio + HTTP/SSE)
└── tests/
    ├── integration_tests.rs         imports `trusty_search::core::*`
    └── benchmark_harness.rs         MRR@5 / Recall@10 quality bench
```

### Shared Library (`trusty-common`, in-workspace path dep)

`trusty-search` depends on the in-workspace `trusty-common` crate
(`trusty-common = { workspace = true }`). The formerly separate
`trusty-mcp-core` and `trusty-embedder` crates were consolidated into
`trusty-common` modules behind feature flags (`mcp` and `embedder`); enable
them via the crate's `Cargo.toml` features. Key surfaces consumed here:

| Feature / module | Contents |
|------------------|----------|
| `mcp` (`trusty_common::mcp`) | `McpRequest`/`McpResponse`/`JsonRpcError`, `run_stdio_loop`, CORS/Trace axum helpers (formerly `trusty-mcp-core`) |
| `embedder` (`trusty_common::embedder`) | `Embedder` trait, `FastEmbedder` (LRU + persistent model cache), `MockEmbedder` (formerly `trusty-embedder`) |
| core (always available) | `bind_with_auto_port`, `resolve_data_dir`/`cache_dir`, `ConcurrentRegistry`, `init_tracing`, `daemon_http_client` |

## Development

```bash
# Build
cargo build

# Test
cargo test

# Run daemon with debug logging
RUST_LOG=debug cargo run -- start

# Query a registered index
cargo run -- query "fn authenticate" --index myproject

# Lint (no warnings allowed)
cargo clippy --all-targets --all-features -- -D warnings
```

### Release Process

Before publishing to crates.io, the Svelte admin UI must be built and synced
into the crate-root `ui-dist/` so `include_dir!` embeds the latest bundle.
`cargo publish` cannot reach files outside the crate tarball, so the sync
step is mandatory.

```bash
make release-prep                              # build ui/ and copy dist → ui-dist/
cargo publish                                  # single crate (lib + bin)
```

`make release-prep` runs `pnpm install --frozen-lockfile && pnpm build` (or
the npm equivalent) and then mirrors `ui/dist/` into the crate-root
`ui-dist/`. CI fails if `ui-dist/` is stale relative to a fresh build (see
`.github/workflows/ci.yml` → `ui-dist-check` job).

When the Rust build runs after the JS step is already done (CI publish flow),
set `SKIP_UI_BUILD=1` to skip `build.rs`'s embedded UI build:

```bash
SKIP_UI_BUILD=1 cargo publish
```

### GPU-accelerated embedding (CUDA, optional)

Default builds and installs are CPU-only and require no GPU drivers. To enable
GPU-accelerated embedding via the ONNX Runtime CUDA execution provider, opt in
with the `cuda` Cargo feature at *build time*:

```bash
# Install with CUDA support (machine must have CUDA toolkit + NVIDIA GPU)
cargo install trusty-search --features cuda

# Dev build with GPU support
cargo build --features cuda
```

**Runtime behaviour (issue #113):** when the binary is built with `--features
cuda`, the CUDA EP is auto-registered at daemon startup and prepended to the
ORT execution-provider list, with CPU-no-arena as the fallback. There is no
runtime flag to "enable" it — once the binary has CUDA support compiled in,
GPU usage is the default. Operators can:

- Force CPU at runtime: `trusty-search start --device cpu` (or `TRUSTY_DEVICE=cpu`)
- Require GPU (fail-fast if absent): `trusty-search start --device gpu`
- Auto-detect (default): `trusty-search start --device auto`

When CUDA is the active EP, the daemon **auto-bumps `TRUSTY_MAX_BATCH_SIZE`
to 512** so ONNX dispatches use the GPU efficiently (CPU's ≈55 MB/slot ORT
arena formula starves the GPU). Set `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` to
preserve a manually configured batch size. The startup log reports the
resolved provider:

```
embedder initialized: model=AllMiniLML6V2(Q) dim=384 provider=CUDA (CUDA GPU)
gpu_batch_tuning: provider=CUDA → TRUSTY_MAX_BATCH_SIZE=512 (was 128)
```

**VRAM arena bounding (issue #600).** The CUDA EP is configured with
`arena_extend_strategy = kSameAsRequested` and an explicit `gpu_mem_limit`
so ORT's BFCArena no longer grows by `kNextPowerOfTwo` and grabs nearly all
device VRAM on the first large batch — which previously OOM'd a 16 GB Tesla
T4 and forced the `TRUSTY_MAX_BATCH_SIZE=32` workaround. The per-process
device-memory ceiling defaults to **12 GiB** (T4-safe headroom) and is
tunable:

| Variable | Default | Purpose |
|----------|---------|---------|
| `TRUSTY_GPU_MEM_LIMIT_BYTES` | `12884901888` (12 GiB) | Exact CUDA `gpu_mem_limit` in bytes. Takes precedence over the MB knob. A malformed or `0` value is ignored (falls through to MB, then the default). |
| `TRUSTY_GPU_MEM_LIMIT_MB` | — | CUDA `gpu_mem_limit` in megabytes (scaled by 1024²). Used only when `TRUSTY_GPU_MEM_LIMIT_BYTES` is unset/invalid. |

Lower the ceiling on smaller cards (e.g. `TRUSTY_GPU_MEM_LIMIT_MB=6144`
for an 8 GB GPU) or raise it on larger ones. With this bounding in place
the `TRUSTY_MAX_BATCH_SIZE=32` workaround is no longer required on a 16 GB
T4. The resolved limit is logged at EP registration.

If CUDA EP initialisation fails at runtime (no driver, wrong CUDA version,
GPU in use by another process), the daemon falls back to CPU with a warning —
unless `--device gpu` was specified, in which case it exits non-zero so the
operator is not silently running CPU-bound on a "GPU node".

Requirements when compiling with `--features cuda`:
- NVIDIA CUDA toolkit installed (`nvcc` on PATH, or `CUDA_PATH` env set)
- Compatible NVIDIA GPU available at runtime
- Builds on CPU-only machines (including macOS) **will fail** the `cudarc` /
  `ort` build scripts — this is expected, not a bug. Omit `--features cuda`
  for CPU-only environments.

**Dynamic ORT linking + `ORT_DYLIB_PATH` (required for glibc 2.34 hosts,
e.g. Amazon Linux 2023 — issue #114):** the CUDA build deliberately does
NOT enable the default `bundled-ort` feature, so `ort-sys` skips its
prebuilt static libraries (which require glibc ≥ 2.38) and `ort` loads
`libonnxruntime.so` dynamically at runtime via `libloading`. The operator
must install a host-compatible ONNX Runtime shared library and point to
it with `ORT_DYLIB_PATH`. Always pair `--features cuda` with
`--no-default-features`:

```bash
# Build trusty-search without bundled ORT (load-dynamic path)
cargo install trusty-search --no-default-features --features cuda

# Install ONNX Runtime GPU 1.20.x (built against glibc 2.31, runs on 2.34+)
curl -L https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-x64-gpu-1.20.1.tgz \
  | sudo tar xz -C /opt
sudo ln -s /opt/onnxruntime-linux-x64-gpu-1.20.1 /opt/onnxruntime

# Point ort at the dynamic library and start the daemon
export ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so
trusty-search start
```

On macOS / modern Linux (glibc ≥ 2.38) the default `bundled-ort` build
keeps working unchanged — no `ORT_DYLIB_PATH` needed and no separate ORT
install.

The feature flag chain is:
`trusty-search/cuda` → `trusty-embedder/cuda` →
`{fastembed/cuda, fastembed/ort-load-dynamic, ort/cuda, ort/load-dynamic}`.
The `ort/cuda` link is the load-bearing one for GPU dispatch — the
all-MiniLM-L6-v2 model uses the ONNX runtime path, so `fastembed/cuda`
(which only enables candle-cuda) alone does not move embedding to the
GPU. The `ort/load-dynamic` link is the load-bearing one for glibc
compatibility on AL2023 and any other host whose glibc is older than
2.38.

### Apple Silicon GPU acceleration (CoreML, auto-detected)

On M1/M2/M3/M4 Macs the same ONNX session (all-MiniLM-L6-v2) runs on the
GPU / Neural Engine via ONNX Runtime's CoreML execution provider **with no
opt-in required**. As of v0.3.13 the `coreml` Cargo feature is no longer
needed — `trusty-embedder` always pulls in the `ort` dep with the `coreml`
feature on macOS, and at runtime registers the CoreML EP whenever
`cfg(all(target_arch = "aarch64", target_os = "macos"))` is true. The
startup log reports which provider is active:

```
embedder initialized: model=AllMiniLML6V2(Q) dim=384 provider=CoreML (Metal GPU / ANE)
```

```bash
# Standard install — no feature flag needed
cargo install trusty-search
```

The legacy `coreml` feature flag is kept as a no-op alias for backward
compatibility, so `cargo install trusty-search --features coreml` still
works but is equivalent to a plain install.

Implementation notes:
- `trusty-embedder` injects `ort::ep::CoreML::default().build()` into
  fastembed's `TextInitOptions::with_execution_providers` (fastembed does
  not expose a `coreml` passthrough feature, but it does accept an external
  `Vec<ExecutionProviderDispatch>`).
- The `ort` dep is pinned to `=2.0.0-rc.12` to match fastembed-rs's
  exact-version requirement.
- On Intel Macs / Linux / Windows the runtime cfg gate is false and the
  default CPU provider is used.

## Project Status

**Phase**: Production-ready. Full hybrid search pipeline, web UI, MCP server, and
robust CLI are all functional. The project is installable as a machine-wide service
via `cargo install trusty-search`.

**Working**:
- `FastEmbedder` with fastembed-rs, LRU cache, persistent model cache (`~/Library/Caches/trusty-search/models/`)
- `UsearchStore` wired to real usearch HNSW index (add/search/remove)
- `CodeIndexer::search` end-to-end (HNSW + BM25 + RRF fusion)
- Tree-sitter AST-aware chunker (rust, python, js, ts, go, java, c, cpp)
- `EntityExtractor` Phase A structural entities (functions, classes, imports)
- `SymbolGraph` KG expansion (callers_of / callees_of, 1–2 hop, EdgeKind multipliers)
- `FileWatcher` with notify-debouncer-mini, 500ms debounce
- MCP server: full JSON-RPC 2.0 stdio + HTTP/SSE transport, 18 tools (per `src/mcp/tools.rs`)
- Daemon: auto-port, fs4 PID lockfile, graceful shutdown, persistent model cache
- Svelte 5 admin UI embedded in binary via `include_dir`
- OpenRouter chat proxy with search context injection
- SSE reindex progress streaming with replay buffer
- Incremental reindex skip via sha2 content fingerprinting
- Parallel batch indexing (rayon + 256-chunk ONNX batches)
- HNSW capacity hinting for large codebases (> 50k chunks)
- Minified JS / build-dir exclusion from indexing
- `trusty-search doctor` 6-check diagnostic with `--fix` auto-repair
- `trusty-search convert` migration from mcp-vector-search
- `indicatif` progress bars for reindex
- HTTP timeouts (2s connect / 5s request) on all daemon calls
- GitHub Actions CI + Dependabot
- 170+ tests passing; clippy clean

**Potential next steps**:
- KG Phase B: IMPORTS/INHERITS edge propagation across file boundaries
- ONNX NER: enable doc comment entity extraction when model file is present
- Benchmark regression CI gate (MRR@5 / Recall@10)
- `cargo install trusty-search` smoke test in CI
- Windows / Linux daemon path support in `trusty-common`
- Blue-green verify canary query tuning (currently uses a fixed probe string)
