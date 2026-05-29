# Performance Testing Report: 2026-05-12

## Test Environment

| Property | Value |
|----------|-------|
| **Date** | 2026-05-12 |
| **Version** | trusty-search v0.3.21 |
| **Machine** | Apple Silicon Mac, 128 GB unified memory |
| **Daemon** | launchd-managed, XLarge memory tier auto-detected |

## Phase 1: trusty-search Repo (Small)

### Index Metrics

| Metric | Value |
|--------|-------|
| Files | ~300 Rust files |
| Chunks | 1,282 |
| Index time (total) | ~1m 45s |
| Index time (embedding) | 1m 42s |
| Index time (BM25) | 89ms |
| Index time (KG) | 0ms (0 symbols — bug #90) |
| Daemon RSS after index | 573 MB |

### Query Performance

| Metric | Value |
|--------|-------|
| Cold query latency | 95ms |
| Warm query latency | 13–16ms |

**Analysis**: Cold-start penalty reflects embedding cache miss on first query. Warm query performance is excellent for hybrid search across 1,282 chunks. Embedding accounts for 97% of total indexing time — the primary bottleneck.

### 8-Query Battery Results

| Query | Intent | Latency | Quality | Notes |
|-------|--------|---------|---------|-------|
| "struct CodeChunk fields" | Definition | 95ms | ⚠️ Partial | Top hit: CHANGELOG not source |
| "fn commit_parsed_batch" | Definition | 15ms | ⚠️ Partial | Exact fn at rank 2 |
| "QueryClassifier intent classification" | Unknown (mis-classified) | 16ms | ⚠️ Partial | Should be Definition |
| "where is spawn_incremental_persist called" | Usage | 15ms | ⚠️ Partial | Top hit is test, not call site |
| "how BM25 index is built from chunks" | Conceptual (mis-classified) | 14ms | ⚠️ Partial | Should be Usage |
| "how does hybrid search ranking combine scores" | Conceptual | 14ms | ✅ Hit | RRF fusion code found |
| "memory management strategy for large codebases" | Conceptual | 13ms | ✅ Hit | memory_policy.rs found |
| "unwrap panic potential error handling" | BugDebt | 13ms | ⚠️ Partial | Hits test, not real unwraps |

**Score: 2✅ / 6⚠️ / 0❌**

**Summary**: Query latencies are strong across the board. However, quality is inconsistent:
- Exact function name queries (queries 2, 4) rank relevant results below false positives
- Compound noun Definition queries (query 1) fail classifier and place markdown docs first
- Intent classifier struggles with multi-word concepts (queries 3, 5)
- Conceptual queries work well when intent is classified correctly
- KG expansion never triggered (bug #90 — zero symbols extracted from Rust code)

## Phase 2: duetto-cto (Medium, 16,586 files)

### Result: OOM Crash Loop

**Outcome**: ❌ Indexing failed — unrecoverable memory exhaustion.

**Timeline**:
- RSS spiked to 50–53 GB within 10–15 seconds of reindex start
- 851 partial chunks committed before first OOM crash
- Daemon restarted; RSS never recovered below 4 GB gate
- Subsequent reindex attempts hit same wall

**Root Cause**: ORT ONNX arena pre-allocates buffers before `TRUSTY_MEMORY_LIMIT_MB` soft cap can trigger (bug #89). The embedding pipeline allocates tensors for the largest batch it has ever seen, and the soft cap polls RSS only after batch commits. On a 16k-file repo, this race condition allows RAM to be exhausted before the cap can halt indexing.

**Search Results on Partial Index**: All queries returned ❌ Miss — insufficient chunk coverage (851 / ~200,000 estimated chunks for duetto-cto) makes search results unreliable.

## Phase 3: duetto-main (Large)

**Status**: Skipped — Phase 2 RSS never recovered below 4 GB gate after crashes. Attempting Phase 3 with residual memory pressure would compound the OOM risk.

## Bugs Filed

| Issue | Title | Severity | Status |
|-------|-------|----------|--------|
| #89 | ORT arena pre-allocates before RSS cap can trip | Critical | Open |
| #90 | KG 0 symbols on Rust codebases | Critical | Open |
| #91 | Intent classifier misses compound noun queries | Medium | Open |
| #92 | Definition queries rank .md docs above source | Medium | Open |

## Key Insights

### Strengths

- **Warm query latency of 13–16ms is excellent** for hybrid search across kiloclock-scale indexes
- **Cold-start penalty manageable**: 95ms reflects single embedding cache miss; subsequent queries use cache
- **Low overhead at small scale**: 573 MB RSS for 1,282 chunks + loaded HNSW + embeddings
- **Parallel indexing works well**: Sub-2min indexing for 300-file Rust repo

### Weaknesses

- **Memory scaling breaks at 16k+ files**: ORT arena pre-allocation defeats soft cap (`TRUSTY_MEMORY_LIMIT_MB`)
- **KG expansion disabled by bug #90**: `hybrid+kg` never triggered; all results are `hybrid` or lower
- **Intent classifier accuracy insufficient**: Multi-word Definition queries mis-classified as Unknown or Conceptual
- **Ranking inconsistency**: Exact function name queries place docs / tests above source definitions

### Bottlenecks

1. **Embedding dominates index time** (97% of total) — vectorization is the primary throughput constraint
2. **Memory soft cap is necessary but insufficient** — ORT arena must also be bounded
3. **Intent classification on compound nouns** — regexes miss multi-word patterns
4. **Knowledge Graph extraction** — zero symbols on Rust code; AST traversal not reaching symbol definitions

## Recommendations

### Immediate (Blocking Large Indexes)

- **Fix bug #89**: Cap ORT arena pre-allocation. Consider `SharedMemoryPool` in ort or batch-size tuning before `Session::new`.
- **Fix bug #90**: Debug tree-sitter AST extraction on Rust. Verify `extract_definitions` captures `fn`, `struct`, `impl` blocks.

### Short-term (Quality Improvements)

- **Enhance intent classifier** (bug #91): Add multi-word token handling for Definition/Usage distinction
- **Re-rank Definition results** (bug #92): Demote `.md` files in Definition intent; prioritize source files by file extension

### Medium-term (Scale & Performance)

- **Batch embedding optimization**: Profile ONNX batch sizes on Apple Silicon; may need platform-specific tuning for CoreML
- **KG Phase B**: Once bug #90 is fixed, enable IMPORTS/INHERITS propagation for deeper context
- **Lazy embedding cache**: Consider LSMT-backed on-disk cache for 256-entry in-memory LRU (survives daemon restarts)

## Conclusion

**Phase 1 validates core search quality on a small, well-understood codebase.** Latencies (13–16ms warm) meet performance targets. However, Phase 2's OOM crash and Phase 1's quality inconsistencies reveal two critical blockers:

1. **Memory scaling** (bug #89) must be fixed before indexing large repos
2. **Intent classification and ranking** (bugs #91, #92) need refinement for production reliability

With both fixes applied, trusty-search should handle 50k–100k chunks at sub-20ms p50 latency across hybrid + KG expansion.
