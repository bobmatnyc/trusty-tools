# mcp-vector-search Integration Analysis

*Researched: 2026-05-09 | Status: Canonical Reference*

## What mcp-vector-search Is

A Python-based (v2.5.56), CLI-first semantic code search service with 17 MCP tools. Per-project tool (not machine-wide). Independently converged on the same hybrid architecture as trusty-search: RRF k=60, all-MiniLM-L6-v2, tree-sitter, BM25+HNSW fusion.

Located at: `/Users/masa/Projects/mcp-vector-search`

## Architecture

```
CLI (typer)
  └── MCPVectorSearchServer (MCP stdio)
        ├── SemanticSearchEngine
        │     ├── LanceDB (IVF-PQ vector ANN)
        │     ├── BM25Backend (rank_bm25, three-pass tokenizer)
        │     ├── KnowledgeGraph (KuzuDB — structural + temporal graph)
        │     ├── CrossEncoderReranker (sentence-transformers)
        │     ├── MMR reranker (diversity pass)
        │     └── QueryExpander (synonym/code-synonym expansion)
        ├── SemanticIndexer
        │     ├── Language parsers (tree-sitter, 13 languages)
        │     ├── NLPExtractor (YAKE keywords from docstrings)
        │     └── EmbeddingEngine (sentence-transformers, MPS/CUDA/ONNX)
        ├── KGBuilder (Kuzu graph from chunks, ~5000 lines)
        └── FileWatcher (watchdog, auto-reindex on save)
```

## What trusty-search Is Missing (Gap Analysis)

### Search Layer Gaps

| Feature | mcp-vector-search | trusty-search | Gap Ticket |
|---|---|---|---|
| BM25 tokenizer | Three-pass: compound + camelCase + snake_case split | Simple `\w+` | #27 |
| Diversity | MMR (λ=0.5, post-RRF) | None | #28 |
| Reranking | Cross-encoder (top-50 → top-k) | None | Future |
| Query expansion | Synonym + code-synonym dict | Intent routing only | Future |

### Entity Layer Gaps

| Feature | mcp-vector-search | trusty-search | Gap Ticket |
|---|---|---|---|
| CodeChunk richness | chunk_type, calls, inherits_from, complexity, depth, nlp_keywords | file, start_line, end_line, content, function_name | #29 |
| KG edge types | CALLS, IMPORTS, INHERITS, CONTAINS + 11 more | callers_of, callees_of only | #33 |
| self.method() resolution | ✅ impl block scope tracking | Not implemented | #33 |

### Facts Layer Gaps

| Feature | mcp-vector-search | trusty-search | Gap Ticket |
|---|---|---|---|
| Git blame per chunk | last_author, last_modified, commit_hash | None | #30 |
| Temporal decay | exp(-λ * days_since_modified) | None | #30 |
| Code quality metrics | Cyclomatic, cognitive, smells, A-F grade | None | #32 |

### MCP Surface Gaps

| Tool | mcp-vector-search | trusty-search |
|---|---|---|
| search_similar | ✅ code-to-code similarity | ❌ missing |
| complexity_hotspots | ✅ | ❌ missing |
| find_smells | ✅ | ❌ missing |
| trace_execution_flow | ✅ via KG | ❌ missing |
| kg_history | ✅ temporal at commit | ❌ missing |

## Key Learnings

### Three-Pass BM25 Tokenizer
The most impactful quick win. mcp-vector-search's tokenizer:
1. Preserves compound identifiers (dotted, hyphenated, namespaced) as single tokens
2. Splits camelCase: `CodeIndexer` → `["CodeIndexer", "Code", "Indexer"]`
3. Splits snake_case: `fn_search_code` → `["fn_search_code", "fn", "search", "code"]`
All forms indexed; IDF computed over union vocabulary.

### KGBuilder self.method() Resolution
The hardest AST entity extraction case. During tree-sitter traversal, track the current `impl` block's type name. When a method call `self.foo()` is encountered, resolve it to `<ImplType>::foo`. This is what enables accurate `CALLS` edges for OOP/Rust code.

### MMR Diversity
Greedy selection: `MMR(d) = λ·relevance(d,q) - (1-λ)·max_sim(d, selected)`. No re-embedding needed — chunk embeddings already in memory from HNSW search.

### KuzuDB Node/Edge Schema (reference for trusty-search petgraph)

**Nodes:** CodeEntity (file/class/function/module), DocSection, Document, Tag, Person (git author), Project, Repository, Branch, Commit, ProgrammingLanguage, ProgrammingFramework, TestCase

**Edges (code structure):** CALLS, IMPORTS, INHERITS, CONTAINS
**Edges (git provenance):** AUTHORED, MODIFIED, MODIFIES, PART_OF
**Edges (doc↔code):** REFERENCES, DOCUMENTS, DESCRIBES, DEMONSTRATES, LINKS_TO
**Edges (tech stack):** WRITTEN_IN, USES_FRAMEWORK, FRAMEWORK_FOR
**Edges (testing):** TESTS, USES_FIXTURE

### Temporal Decay Scoring
`temporal_score = exp(-0.01 * days_since_modified)` — half-life ~70 days. Used as tie-breaker in re-ranking, not primary signal.

### LCA Scorer
Least Common Ancestor scorer for structural proximity in the KG. Provides a contrastive baseline for KG expansion scoring, more principled than the flat "70% of trigger score" heuristic currently in CLAUDE.md.

## Integration Roadmap

| Phase | Tickets | Layer | Priority |
|---|---|---|---|
| Immediate | #27 (BM25 tokenizer), #29 (CodeChunk fields) | Search + Entity | High |
| Short-term | #28 (MMR), #30 (git blame), #33 (KG edges) | All | Medium |
| Medium-term | #31 (search_similar), #32 (complexity metrics) | MCP + Facts | Medium |
| Long-term | Cross-encoder reranking, query expansion, KG temporal | Search | Low |

## Architecture Principle

trusty-search and mcp-vector-search should be complementary, not redundant:
- **trusty-search**: machine-wide daemon, multi-project, Rust-native, sub-10ms queries, canonical facts store
- **mcp-vector-search**: per-project, Python, richer analysis (cross-encoder, MMR, complexity), existing MCP surface

The code analysis capabilities (complexity, smells, git blame, rich AST metadata) flow from mcp-vector-search's design into trusty-search as the **Analysis layer** — a fourth layer alongside Search / Entity / Facts.

---

*This document is the canonical reference for mcp-vector-search integration. Update when new capabilities are identified.*
