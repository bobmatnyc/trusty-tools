# Tantivy + SurrealDB as Memory/Retrieval System — Evaluation

**Date**: 2026-04-25
**Status**: Research complete — recommendation: partial replacement, phased

---

## Current State: What open-mpm Already Has

Before evaluating the proposed stack, it is critical to understand what is already in place.

### Existing embedded memory stack (in-process Rust)

open-mpm already has a substantial embedded memory system in `src/memory/`:

| Crate | Role | Status |
|---|---|---|
| `redb 2.6` | Transactional k/v storage for payloads and id/label maps | In production |
| `usearch 2.25` | HNSW vector index (mem.usearch, code.usearch) | In production |
| `fastembed 5.13` | Local ONNX embedding (AllMiniLML6V2, 384-dim) | In production |
| `tree-sitter-*` | AST-based code chunking for the vector index | In production |

The key modules:

- `src/memory/redb_usearch.rs` — `RedbUsearchStore`: two HNSW indexes (agent memory segment + code index segment) backed by redb for payloads and label maps.
- `src/memory/graph.rs` — `MemoryGraph`: agent session graph encoded *inside* the existing redb/usearch store. Edges are zero-vector rows, filtered out of search results.
- `src/memory/code_store.rs` — `CodeStore`: code-specific index opened from `.open-mpm/state/code/`.
- `src/memory/embed.rs` — `FastEmbedder`: wraps `fastembed` crate.
- `src/tools/memory.rs` — `VectorSearchTool` (native) and `KuzuRecallTool` (shells out to Python/kuzu).

### External MCPs (not in-process)

Two MCP servers are configured in `.mcp.json`:

1. **kuzu-memory** (`kuzu-memory mcp`): External Python-based MCP server. open-mpm calls it via two paths:
   - Claude Code session hooks (Claude Desktop reads memories from it at conversation start)
   - `KuzuRecallTool` (`memory_recall` tool): shells out via `python3 -c` to query `.kuzu-memory/memories.db` with a kuzu Cypher query. This is a subprocess shim, NOT a native Rust integration.

2. **mcp-vector-search** (`uv run mcp-vector-search mcp`): External Python-based MCP server for Claude Code's code analysis tools. This is a Claude Code development tool, not used by the open-mpm runtime agents directly.

### What kuzu actually provides to open-mpm

The kuzu integration is deliberately lightweight and optional:
- `read_kuzu_memories()` in `src/init/mod.rs` reads Markdown files from `kuzu-memories/`, `.kuzu-memory/`, and `~/.kuzu-memory/exports/` — it reads the **exported artifact files**, not the DB directly.
- `KuzuRecallTool` shells out to Python to query the DB. If Python or kuzu isn't installed, it returns a graceful JSON error and the agent proceeds without memory.
- The kuzu DB is the **kuzu-memory CLI's** database, populated by the external kuzu-memory tool, not by open-mpm itself.

**Critical finding**: open-mpm does not depend on kuzu structurally. The `memory_recall` tool is an optional, gracefully-degrading bridge to an external tool. The core memory system is already the redb + usearch + fastembed stack.

---

## Critical Context: Kuzu is Effectively Dead

The comparison is further simplified by a significant external event: **KùzuDB was acquired by Apple and its GitHub repository was archived in October 2025.** Active development has stopped. The kuzu-memory MCP tool depends on the kuzu Python package which is now unmaintained.

This changes the question from "should we replace kuzu?" to "kuzu is already a liability — what fills the gap cleanly?"

---

## Proposed Stack Evaluation

### Tantivy

**What it is**: Pure-Rust full-text BM25 search engine (Lucene-inspired). The most mature Rust search library. Powers Quickwit, Meilisearch internals, and many others.

**Vector/HNSW support**: As of 2025, **Tantivy core does not natively include HNSW or ANN vector search.** The community GitHub issue (#815) requesting this dates to 2020 and has not been resolved upstream. Hybrid search in the Rust ecosystem is achieved by pairing Tantivy (BM25) with a separate vector library like usearch, qdrant-segment, or hnsw-rs — not by using Tantivy alone.

**Relevance to open-mpm**: open-mpm's code search and agent memory retrieval are already well-served by usearch HNSW. The missing capability is BM25/keyword search, not vector search. Tantivy would add BM25 keyword search alongside the existing usearch ANN index.

**Verdict on Tantivy**:
- Adding Tantivy for keyword search over agent memories and code is a genuine improvement (hybrid BM25 + vector = better recall than vector alone).
- However it does NOT replace kuzu or provide graph/structured storage.
- It does NOT replace the existing usearch layer; it complements it.
- Effort: Medium. Tantivy indexes are separate from redb/usearch; hybrid search requires a Reciprocal Rank Fusion merge step.

### SurrealDB

**What it is**: Multi-model database (document + graph + relational + vector + time-series) written in Rust. Embeddable via `surrealdb` crate with `kv-surrealkv` (native Rust, no C deps) or `kv-rocksdb` (C++ dep, slow to compile).

**SurrealDB 3.0 (2025)**: Launched alongside $23M Series A extension, $44M total funding. Positioned explicitly for agent memory with graph traversal, vector HNSW, and MCP server support. Acquires the positioning that kuzu was heading toward but with OLTP semantics rather than OLAP.

**Vector search**: Native HNSW index via `DEFINE INDEX ... HNSW DIMENSION N DIST COSINE`. Supports cosine, euclidean, and Manhattan distance. Comparable capability to usearch.

**Graph model**: Native graph via `RELATE` statements and `->edge->` traversal in SurrealQL. Designed for OLTP (live agent writes), unlike kuzu which was OLAP-only.

**Compilation concerns**:
- `kv-rocksdb`: Requires C++ RocksDB libs. Very slow to compile. Large binary.
- `kv-surrealkv` (recommended): Pure Rust, no C deps, compiles fast. LSM tree architecture with leveled compaction. Designed as RocksDB replacement.
- The `surrealdb` crate itself is large (~400k lines including engine). Significant binary size increase.

**Startup time**: SurrealKV uses constant-time offset lookups for reads. No startup scan required (unlike older VART architecture). In embedded mode, initialization is sub-second for typical agent memory sizes, but cold start with SurrealKV will be slower than redb (which is a simple B-tree with no WAL complexity).

**Key risk**: The `surrealdb` crate is very large and opinionated. It brings its own async runtime assumptions, connection pooling, SurrealQL parser, and schema system. Embedding it means accepting a significant dependency weight. The recommended Tokio configuration (multi-thread, 10MiB stack) conflicts with open-mpm's current single-process lightweight model.

**Verdict on SurrealDB**:
- Could replace BOTH kuzu (graph) AND usearch (vector) in a single dependency.
- The native graph model is a genuine improvement over the current "zero-vector edge row" hack in `MemoryGraph`.
- SurrealKV avoids C deps entirely.
- Cost: ~2-3x binary size increase. Non-trivial startup overhead increase (likely 200-500ms on cold open). SurrealQL adds query surface complexity.
- The MCP server integration is a bonus for Claude Code tooling.

### fastembed (already present)

fastembed 5.13 is already in Cargo.toml and working. It is the correct answer for local embeddings. No change needed here. v5.12+ supports BGE Small, AllMiniLML6V2, Nomic V2, sparse models, and Qwen3. For a local-first tool with no API dependency, fastembed is the right choice and should be retained regardless of other changes.

---

## Key Questions Answered

### 1. Can SurrealDB replace BOTH kuzu (graph) AND mcp-vector-search (semantic search)?

**Partially yes, with caveats.**

- SurrealDB can replace kuzu's graph DB for agent memory (better fit for OLTP agent writes).
- SurrealDB can replace usearch as the vector index. However, usearch 2.25 is already embedded and working.
- `mcp-vector-search` (Python MCP server for Claude Code) is a development tool used by Claude Code sessions, NOT by open-mpm runtime agents. It would not be replaced by SurrealDB — it serves a different purpose.
- The kuzu-memory MCP's job (storing project decisions for Claude Desktop context) could be replaced by a SurrealDB-backed MCP or by extending open-mpm's own memory system.

### 2. What embedding model to run in Rust?

fastembed is already the answer and already in place (`fastembed = "5.13"`). It runs AllMiniLML6V2 (384-dim) via ONNX locally, no API required. This is the most practical option:
- No GPU required
- No API latency
- 3-5x faster than Python equivalents
- First-run model download is cached to `~/.cache/fastembed/`
- Alternative: call OpenRouter's embedding API (adds latency + API cost for every index operation — not recommended for a local-first tool)

### 3. Tantivy vs SurrealDB vector search — do we need both?

No. SurrealDB's HNSW and usearch's HNSW are equivalent capabilities. If SurrealDB is adopted, usearch can be dropped. If staying with the current stack, Tantivy adds BM25 as a complement to usearch (hybrid search), which is a genuine improvement.

### 4. Migration complexity

**Current kuzu integration**: 
- Shell-out to Python at query time (not a compile-time dependency)
- File-based artifact reading (`read_kuzu_memories`) from Markdown files
- No Rust kuzu crate in Cargo.toml

Migration away from kuzu is therefore **low complexity on the open-mpm side**. The hard part is: if the kuzu-memory CLI was being used to accumulate project knowledge, that data needs to be exported and re-imported. Since kuzu-memory exports Markdown files, a migration tool reading those exports and inserting into a new store is straightforward.

**Migration from usearch to SurrealDB vector**: Higher complexity. The `RedbUsearchStore` is tightly integrated with the memory graph encoding scheme. A SurrealDB migration would require rewriting `MemoryStore` trait implementations and the edge-encoding pattern in `MemoryGraph`.

### 5. Startup time impact

- Current stack (redb + usearch): Sub-100ms store open on typical agent memory sizes.
- SurrealDB embedded (SurrealKV): Likely 200-600ms cold open based on LSM initialization. This is a regression against the current <1s total startup goal.
- Tantivy index open: ~10-50ms for small indexes. Acceptable.
- **Verdict**: SurrealDB startup cost is the primary risk to open-mpm's responsiveness. Mitigation: lazy-open the SurrealDB store after serving the first user interaction, rather than at process startup.

---

## Architecture Recommendation

### Recommended approach: Targeted incremental improvements, not a full replacement

The current redb + usearch + fastembed stack is solid, already embedded, and already working. The kuzu dependency is external and optional — its archival is a prompt to clean up the shell-out, not a reason to replatform the entire memory system.

**Phase 1 — Remove kuzu (S, 1-2 days)**

Remove the `KuzuRecallTool` Python shell-out. Replace with a query against the existing `RedbUsearchStore` agent memory segment. The kuzu-memory MCP server can remain as a Claude Code development tool if desired (it's not a runtime dependency), but the `memory_recall` tool should be repointed to the native store. The `read_kuzu_memories` file-reader can remain as a backward-compatibility loader for existing Markdown exports.

**Phase 2 — Add Tantivy for hybrid BM25 + vector search (M, 1 week)**

Add `tantivy = "0.22"` alongside the existing usearch. Build a Tantivy index over code chunks (already chunked by tree-sitter) and agent memory content. Implement Reciprocal Rank Fusion to merge Tantivy BM25 results with usearch ANN results in `VectorSearchTool`. This gives meaningfully better recall for the research agent and plan agent without replacing any existing infrastructure.

**Phase 3 — Evaluate SurrealDB for MemoryGraph replacement (L, 2-3 weeks)**

If the graph encoding hack (`zero-vector edge rows`) in `MemoryGraph` becomes a maintenance burden, evaluate replacing the redb/usearch combo with SurrealDB + SurrealKV. The native graph model in SurrealDB would eliminate the encoding workaround. Key gates before committing:
- Measure actual SurrealDB cold-start time in the open-mpm binary (target: <500ms)
- Measure binary size increase (expect +15-25MB)
- Confirm SurrealKV compiles cleanly on the CI matrix without C deps
- Confirm usearch can be dropped (SurrealDB HNSW covers the same capability)

### What to keep, what to replace

| Component | Action | Rationale |
|---|---|---|
| `redb` | Keep (Phase 1-2), evaluate removal (Phase 3) | Working, fast, minimal |
| `usearch` | Keep (Phase 1-2), evaluate removal (Phase 3) | Working HNSW, no C deps issue |
| `fastembed` | Keep permanently | Already best-in-class for local embedding |
| `KuzuRecallTool` (Python shell-out) | Remove in Phase 1 | Kuzu archived; fragile subprocess shim |
| `kuzu-memory` MCP (Claude Code tool) | Keep as dev tool but treat as deprecated | Archival means no security patches |
| `mcp-vector-search` MCP (Claude Code tool) | Keep — separate purpose from runtime | Not a runtime dependency |
| Tantivy | Add in Phase 2 | BM25 complement to existing vector search |
| SurrealDB | Conditional Phase 3 | Only if MemoryGraph complexity justifies it |

### Proposed crate versions (if all phases executed)

```toml
# Phase 1: no new deps, remove kuzu shell-out
# Phase 2: add Tantivy
tantivy = "0.22"  # BM25 full-text index

# Phase 3: SurrealDB (replaces redb + usearch)
surrealdb = { version = "2", features = ["kv-surrealkv"] }
# Drop: redb, usearch (SurrealDB subsumes both)
# Keep: fastembed (SurrealDB integrates with it directly)
```

### Effort estimates

| Phase | Size | Duration |
|---|---|---|
| Phase 1: Remove kuzu shell-out, repoint to native store | S | 1-2 days |
| Phase 2: Add Tantivy BM25 + RRF hybrid search | M | 1 week |
| Phase 3: SurrealDB MemoryGraph replacement | L | 2-3 weeks |

### Key risks

1. **SurrealDB binary size**: The `surrealdb` crate is very large. If binary size is a constraint (e.g., for the Tauri UI binary), this is a blocker for Phase 3.
2. **SurrealDB startup latency**: SurrealKV LSM initialization may exceed the <1s startup target. Must benchmark before committing.
3. **SurrealDB API churn**: SurrealDB 3.0 is recent; API stability between minor versions is not yet proven at the same level as redb.
4. **Tantivy index persistence**: Tantivy indexes require explicit commit and merge operations. Integrating write-path management alongside the existing redb write path adds coordination complexity.
5. **fastembed model download on first run**: The first open-mpm invocation on a new machine downloads the embedding model (~90MB). This is existing behavior but worth documenting for CI environments.

### Whether to do phased migration or clean replacement

**Phased migration is strongly preferred.** The current stack is functional. Phase 1 alone removes the liability (kuzu archival). Phase 2 provides genuine search quality improvement. Phase 3 is optional and should only proceed if the graph encoding complexity becomes a real maintenance problem. A clean cut-over to SurrealDB risks significant startup regression and binary bloat with unclear return if the current graph model is adequate.

---

## Summary Answer to the Core Question

**Can Tantivy + SurrealDB replace kuzu-memory + mcp-vector-search?**

- kuzu-memory: Yes, and it should be — kuzu is archived, and the open-mpm integration is already a thin shim over the native store. Phase 1 removes this with minimal effort.
- mcp-vector-search: No replacement needed — it is a Claude Code development tool, not an open-mpm runtime dependency.
- The native memory system (redb + usearch) is already doing the heavy lifting. The proposed stack is most valuable as an incremental enhancement (Tantivy for BM25) rather than a wholesale replacement.
- SurrealDB becomes worth evaluating in Phase 3 only if the MemoryGraph edge-encoding workaround becomes a pain point.

---

## Sources

- [Tantivy GitHub](https://github.com/quickwit-oss/tantivy)
- [Tantivy ANN/Vector Issue #815](https://github.com/quickwit-oss/tantivy/issues/815)
- [tANNtivy experimental fork](https://github.com/mccullocht/tANNtivy)
- [fastembed crates.io](https://crates.io/crates/fastembed)
- [fastembed GitHub (fastembed-rs)](https://github.com/anush008/fastembed-rs)
- [SurrealDB Embedding in Rust](https://surrealdb.com/docs/surrealdb/embedding/rust)
- [SurrealDB Vector Database Docs](https://surrealdb.com/docs/surrealdb/models/vector)
- [SurrealDB Agent Memory Use Case](https://surrealdb.com/use-cases/agent-memory)
- [SurrealDB vs Agent Databases](https://surrealdb.com/why/vs-agent-databases)
- [SurrealDB 3.0 VentureBeat Coverage](https://venturebeat.com/data/surrealdb-3-0-wants-to-replace-your-five-database-rag-stack-with-one)
- [SurrealDB Semantic Search Blog](https://surrealdb.com/blog/semantic-search-in-rust-with-surrealdb-and-mistral-ai)
- [SurrealKV GitHub](https://github.com/surrealdb/surrealkv)
- [FrankenSearch hybrid BM25+vector](https://github.com/Dicklesworthstone/frankensearch)
- [SurrealDB Performance Best Practices](https://surrealdb.com/docs/learn/querying/performance/performance-best-practices)
