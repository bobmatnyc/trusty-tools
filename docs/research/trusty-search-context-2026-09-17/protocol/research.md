# Trusty Search lexical context experiment — research handoff

Status: experiment specification, not a repository governing spec. 2026-09-17.
Scope: re-chunk a frozen copy of this repository; compare baseline with deterministic contextual enrichment and navigation cards. No embeddings or vector experiments. No production daemon changes.

## Confirmed integration points

All paths below are relative to the repository root.

- `crates/trusty-search/src/core/chunker/ast.rs:72`, `chunk_ast(file, content)`, returns source chunks and parsed entities. Preserve this parser and its KG entities in the first candidate.
- `crates/trusty-search/src/core/chunker/types.rs:86`, `RawChunk`, already carries exact source text, inclusive line ranges, function_name, chunk_type, calls, parent/child IDs, and persisted `virtual_terms` with a serde default.
- `crates/trusty-search/src/core/chunker/walk.rs:29` uses maximum 200 lines, subwindows 100 lines, stride 50. At lines 452–492, subchunks inherit symbol/type but clear calls; the umbrella parent remains. Changing windows affects duplicate hits and relationship IDs. Treat window size as a separate experimental axis after context enrichment.
- `crates/trusty-search/src/core/indexer/helpers.rs:389`, `populate_virtual_terms`, gathers entity text within each chunk range, deduplicates, then REPLACES virtual_terms. Add context after this routine, or extend it; adding before it loses the context.
- `crates/trusty-search/src/core/indexer/ingest/mod.rs:338` applies enrichment to incremental ingestion; line 625 applies it in the parallel bulk parse path. Both must use the same deterministic helper with the full file content available.
- `crates/trusty-search/src/core/indexer/persist_hnsw.rs:247`, `bm25_doc_text`, joins raw body and virtual terms. `ingest/commit.rs:292`, `commit_bm25_batch`, calls it. Reuse this persisted route rather than altering displayed source.
- `crates/trusty-search/src/core/indexer/helpers.rs:312`, `build_compact_snippet`, returns the first seven lines, regardless of query or symbol boundaries. `raw_to_code_chunk` starts at line 343 and is the result-materialization hook.
- `crates/trusty-search/src/mcp/tools/compact.rs:37`, `DROPPED_FIELDS`, removes symbol, type, language, ID, and source content. HTTP compact and MCP compact differ: HTTP retains full content; MCP transforms the response. Measure the actual selected output format rather than assuming HTTP compact saves tokens.

## No-embedding requirements

- Create each experiment index with `skip_vector: true` and `skip_kg: false`; creation resolves these flags at `service/server/indexes.rs:496–502`. Incremental indexing honors skip_vector at `core/indexer/ingest/mod.rs:354`; bulk parsing also supports no vectors.
- Lexical requests use `stage: "lexical"`. In `core/indexer/search/mod.rs:255–265`, lexical disables both KG and query embedding.
- KG requests use `stage: "graph"`, but that same code still calls `embed_query` for graph stage. `search/lanes.rs:235` returns None only if no embedder is attached. Thus skip_vector alone is not proof of zero query embeddings. For HTTP creation, use the rejecting-embedder startup below and gate all query/refinement embedding paths on skip_vector. Verify using a fail-on-call embedder test or observable call counts.
- Never query the semantic lane; the HTTP layer can route it to another facet (`service/server/search.rs:747`), which would invalidate the isolated comparison.

## Minimal candidate

Use an explicit opt-in experiment mode with the default behavior unchanged. Freeze mode for the lifetime of an index; reindex from scratch when changing it.

1. Retain AST boundaries initially. At indexing, enrich virtual_terms with bounded, deduplicated, repository-relative path components, symbol name, enclosing parent signature/name, and source-derived leading documentation. Prefer the chunk's own description and immediate parent context. Avoid copying an entire file TOC into every chunk; that creates false lexical matches.
2. Derive a card from persisted chunk metadata/source: relative path, inclusive range, symbol, kind, a short extracted description/signature, bounded child landmarks, and an expansion locator. Keep evidence source separate from descriptions. No generated claims and no LLM dependency.
3. Optional second candidate: smaller AST subwindows, evaluated independently. Preserve umbrella and relationship contracts unless explicitly designing a separate deduplication treatment.

Suggested tuning grid: context budget 0/128/256 words; own documentation versus own plus parent description. Card budget fixed across candidates. Choose a small grid before evaluation. These are experiment parameters, not asserted optimal settings.

## Contracts and edge cases

- Enrichment never changes source content, start/end lines, chunk IDs, or parsed entities. Inclusive lines must resolve against the frozen source copy.
- Order and truncation are deterministic and UTF-8-safe. Context terms have a hard limit; repeated headers do not grow index size without bound.
- Absolute workspace paths must not enter searchable context or reproducibility comparisons; use relative paths.
- Empty files, unsupported languages, parse fallbacks, multiline signatures, CRLF, non-ASCII identifiers, docs before the AST node, huge single-line files, missing parents, and oversized parent/child duplicate hits need coverage.
- Bulk index, incremental file replacement, removal, and restart/reload must preserve equivalent BM25 results for unchanged content.
- A card-only change must leave ranking unchanged. An index-context change must persist across restart. Old corpus rows default to baseline if optional metadata is added.
- A navigation locator includes index identity plus path/range or chunk ID; path alone is ambiguous across projects.
- Do not interpret absent results from failed/not-ready stages as misses.

## Reproducible evaluation

The existing `tests/benchmark_harness.rs:35–92` provides intent buckets, but its metrics at lines 103–134 judge arbitrary substrings (for example `cache`, `error`, `test`). These labels can be satisfied by irrelevant code and become particularly misleading when adding context. Do not use them as the primary quality verdict.

`tests/benchmark_open_mpm.rs:92` offers a better ground-truth model: query ID, query type, mode, expected relative files, and optional KG seed query. Extend the idea with expected symbols/range overlap and multiple relevant sources for this repository.

- Freeze repository SHA, source manifest checksums, toolchain, binary SHA, tokenizer, stop-word/ranking settings, ignore patterns, file count, chunk count, graph entity/edge count, and experiment mode.
- Label 30–40 queries before tuning, stratified across exact definitions, conceptual implementation, callers/relationships, configuration/docs, and hard negatives. Split tuning and held-out queries before running candidates. Keep expected paths/symbols out of enrichment logic.
- Four primary cells: baseline lexical; context lexical; baseline graph; context graph. Baseline and candidate use the same snapshot and fully re-chunk it independently.
- Keep retrieval and presentation ablations separate: full output, existing MCP compact, new card. Report real output bytes and tokens with a named tokenizer or explicitly label a character-based estimate.
- Report MRR@5, success@5/10, true recall@10 where multiple relevance labels exist, duplicate source overlap, and per-intent results. Existing boolean hit rate is success@k, not full recall.
- Track indexing wall time, CPU/RSS where practical, on-disk bytes, p50/p95 warm query latency, cold query separately, output tokens, and follow-up reads to acquire labelled evidence. Token savings alone do not prove faster task completion.
- Run equal warmups and repeated shuffled/interleaved query orders. Keep indexing outside query timing. Preserve all raw JSON responses and manifests, plus per-query paired deltas.
- Select on tuning data; run held-out once on the selected candidate. Report optimal only within the tested grid. A candidate must improve efficiency without material relevant-source loss; record tradeoffs instead of choosing solely on aggregate scores.

Isolation helpers already exist in `tests/support/isolated_benchmark.rs:1`: dedicated non-default loopback port, TRUSTY_DATA_DIR with `.trusty-search-test-daemon`, TRUSTY_SEARCH_TEST_CORPUS_ROOT without .git and with `.trusty-search-test-corpus`, and daemon `--no-auto-discover`. Helpers validate URL/discovery consistency, ownership, and readiness before mutation/measurement. Reuse them.

## Evidence and next step

Read-only source inspection only. `trusty-search search` returned `index trusty-tools not found on daemon`, so current files were inspected directly. No implementation or runtime benchmark was performed by this research agent.

Parent continues with isolated daemon setup and interface specification. Engineer implements opt-in enrichment/cards and zero-embedding proof; evaluator prepares labels before tuning. Production and unrelated sessions remain untouched.

## Startup integration addendum

Parent froze source at `90b6aeb94` in `/Users/masa/trusty-search-experiment/worktree`.

A custom runner can use `SearchAppState::new` (`service/server/state_impl.rs:49`), explicit `with_allowlist_paths` (line 192), and `build_router` (`service/server/mod.rs:288`). Avoid normal daemon boot, discovery, and production registry paths.

Confirmed HTTP creation constraint: `service/server/indexes.rs:451` requires `current_embedder()` before resolving skip_vector. A completely absent embedder blocks even lexical-only index registration. Smallest experimental fixture: attach a rejecting `Embedder`, with dimension set to a valid storage dimension, that increments a shared atomic counter and returns an error from both embed and embed_batch. The trait at `core/embed.rs:35` requires precisely those two async methods plus dimension; provider has a default. A rejecting embedder does no embedding and makes accidental calls fail visibly.

Shared correction needed for graph queries: at `core/indexer/search/mod.rs:262`, skip embedding when lexical_only OR self.skip_vector. At line 356, skip refinement embeddings when skip_kg OR self.skip_vector. Test graph requests both with and without refine_query, and indexing including incremental updates, against the rejecting embedder. Final counter must be zero. Keep a non-vector BM25+KG baseline using the same no-vector guard, so candidate benefit is attributable to context/cards.

## External research checked for this experiment

- [Anthropic: Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval) describes prepending chunk-specific context for BM25 as well as embeddings. Its widely quoted 49% retrieval-failure reduction combines contextual embeddings and BM25; it is not a BM25-only prediction for this experiment. Here the context is deterministic source metadata/documentation rather than LLM-generated summaries.
- [cAST (Zhang et al., 2025)](https://arxiv.org/abs/2506.15655) studies AST-aware structural chunking for code retrieval. Trusty already uses AST extraction; our grid changes only oversized subwindows, so this is not a reproduction of the complete cAST algorithm.
