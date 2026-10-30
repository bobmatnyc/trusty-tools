# G02: improving trusty-search code search

Research only, 2026-10-30. Nothing built, changed, merged or committed. Figures marked "unverified" come from memory or from arithmetic, not from a cited source. Cargo.lock pins fastembed 5.13.4 and ort 2.0.0-rc.12.

## Decision asked

Which changes, in which order, raise code-search quality without a forced re-embed, and which embedder arms to add to the #10482 A/B before #10483 starts.

Short answer: fix fusion and exact-match protection first (no re-embed, no new model), land #10481 with a path and symbol header, add an opt-in cross-encoder rerank behind a flag, add a symbol field to BM25, and add two code-specific embedders to the #10482 pool.

## Baseline (from code)

| Part | Today | Cite |
|---|---|---|
| Embedder | MiniLM-L6-v2, 384-d, window pinned to 256 tokens (model allows 512). Candle fallback caps at 128. Live daemon uses the Python sidecar. | `crates/trusty-common/src/embedder/fast_embedder.rs:338,350`; `candle_embedder.rs:54`; D83 brief section A |
| Chunker | AST chunks; one over `MAX_CHUNK_LINES = 200` splits into 100-line windows, stride 50. Counts lines, not tokens. 29.9% of chunks exceed 256 tokens; 52.1% of chunk tokens never reach the embedder. | `core/chunker/walk.rs:29-34`; D83 brief "Data" |
| Vector store | usearch HNSW, dimension fixed at index creation. Tuning: connectivity 32, expansion_add 128, expansion_search 256. | `core/store/usearch_store.rs:446`; `core/store/hnsw_tuning.rs:23-37` |
| Lexical lane | In-memory BM25, one field, k1 = 1.5, b = 0.75. Tokenizer already splits camelCase, PascalCase and alpha/digit, and keeps the compound token. No path, symbol or comment field. | `core/bm25.rs:34`; `trusty-common/src/bm25.rs:86-105,235-243` |
| Fusion | `rrf_fuse`, rank-only, `RRF_K = 60`, score = alpha/(60+rank_vec) + beta/(60+rank_bm25). Then MMR (lambda 0.5). | `core/search/rrf.rs:14`; `core/indexer/search/mod.rs:409,424,811` |
| Intent weights (alpha vector, beta BM25) | Definition 0.3/0.7, Usage 0.5/0.5 (+KG first), Conceptual 0.8/0.2, BugDebt 0.1/0.9, Keyword/Unknown 0.6/0.4 | `core/classifier/intent.rs:22-50` |
| Extra lanes | Grep lane for Definition intent only; ripgrep fallback on empty result; exact-match floor for a single literal (identifier, quoted string, filename, issue ref). | `core/indexer/search/mod.rs:397-421`; `search` tool description (#7675) |
| KG boost | `expand_with_kg` adds call-graph neighbours when intent is Usage (`use_kg_first`); hits tagged `hybrid+kg`. | `core/indexer/search/kg.rs:36,96` |
| Reranker | None. MMR is a diversity pass, not a relevance model. A repo grep for rerank found no cross-encoder. | `core/mmr.rs`; grep of `crates/trusty-search/src` |
| Throughput | About 83 chunks/s with MiniLM on M4 Max (batch 64). | `crates/trusty-search/CHANGELOG.md:3506-3516` (#753) |

Scale: index `trusty-tools-4e2cf878`, 144,293 chunks, 2.32 GB on disk. Vectors alone: 144,293 x 384 x 4 B = 222 MB.

Hosts for budgets: Mac = 16 cores, 128 GiB (`sysctl -n hw.ncpu hw.memsize` gave 16 and 137438953472), Apple Silicon, CoreML/ONNX. EVO = 28 cores, 47 GiB (about 40 free), x86_64 Linux, CPU ONNX, 8-thread cap for analytics jobs (instance-evo skill and the #10482 plan).

## Reused work (not redone)

| Input | What this brief takes from it |
|---|---|
| `docs/briefs/2026-10-29-9284-chunk-window.md` | Chunk-token data, query mix (section G: 67% of `search_lexical` calls are bare names), hybrid buried the BM25 top hit in 2 of 13 queries, #6581 amendment wording (section E). |
| Architect briefs `2026-10-30-search-chunk-size.md`, `2026-10-30-per-index-embeddings.md` | Window 256 is our setting; #10481 is the 256-token change; #10483 is the swap. |
| #10482 plan (`d83/10482-ab-plan.md`) | 190-query set, pooled labels graded 0-2, two-judge protocol with kappa >= 0.6, arms C0-C8, nDCG@10 with paired bootstrap. A separate agent is running it; its files are untouched. |
| #10481, #10482, #10483 | Dependencies below. |

New in this brief: everything in sections "Comparison tables" and "Test set" except the items above. In particular new are the code-embedder candidates, reranker options, fusion evidence, lexical-field proposals, the cross-system survey and the 60-query code slice.

## Comparison table 1: embedders (area 1)

Quality columns are not comparable across rows: CSN = CodeSearchNet MRR and CoIR = CoIR average NDCG@10 as published on the CodeRankEmbed card; the gte-modernbert 79.31 is on a different CoIR variant (20 tasks) and must not be set against the 60.1 column. MiniLM has no code score found.

| Model | Params | Dim | Window | CSN / CoIR | fastembed 5.13.4 | RAM (fp32 weights) | CPU latency vs MiniLM | Vector bytes at 144,293 chunks |
|---|---|---|---|---|---|---|---|---|
| MiniLM-L6-v2 (today) | 22M | 384 | 256 set | not found | yes | about 0.1 GB (unverified) | 1x | 222 MB |
| jina-embeddings-v2-base-code | 161M | 768 (fastembed `dim: 768`) | 8192 (trained at 512) | 67.2 / 58.4 | yes, `JinaEmbeddingsV2BaseCode` | about 0.65 GB (unverified) | about 6x (unverified) | 443 MB |
| CodeRankEmbed | 137M | not on card (768 unverified) | 8192 | 77.9 / 60.1 | no; needs user-defined ONNX (`UserDefinedEmbeddingModel` exists); ONNX export unverified | about 0.55 GB (unverified) | about 5x (unverified) | about 443 MB (unverified) |
| CodeSage-small-v2 | 130M | 1024 | not on card | NL2Code avg 64.41; v1 small CSN 64.9 / CoIR 54.4 | no; Transformers only per card | about 0.5 GB (unverified) | about 5x (unverified) | 591 MB |
| bge-m3 | 568M (per CoIR paper) | 1024 | 8192 | CoIR 39.31 | yes, `BGEM3` | about 2.3 GB (unverified) | about 25x (unverified) | 591 MB |
| gte-modernbert-base | 149M | 768 | 8192 | CoIR 79.31 (20-task variant); MTEB retrieval 55.33 | no (only `ModernBertEmbedLarge`) | about 0.6 GB (unverified) | about 5x (unverified) | 443 MB |
| arctic-embed-m-v2.0 | not shown | not shown (768 unverified) | not shown (8192 unverified) | not shown | no | unverified | unverified | unverified |
| bge-small-en-v1.5, arctic-embed-s (already in #10482) | 33M | 384 | 512 | CoIR not found | yes | about 0.15 GB (unverified) | about 2x (unverified) | 222 MB |

Findings:

- Code-specific models beat general ones on CoIR in the published tables: CodeRankEmbed 60.1 and Jina-Code-v2 58.4, against BGE-Base 42.77, BGE-M3 39.31 and GTE-Base 36.75 (CoIR paper table 3). BM25 scores 29.79 there.
- Only Jina-code is runnable through fastembed 5.13.4 without custom ONNX work. Cargo.lock also resolves 5.17.3 in the registry; its model list adds only EmbeddingGemma variants.
- All code models above except the small ones change the dimension (768 or 1024). usearch fixes the dimension per index, so each needs a new index: #10483.
- CPU latency per chunk relative to MiniLM: no primary source found for any row. The multipliers are parameter-ratio estimates.
- Index cost at the token-aware split (+82% chunks, lower bound, D83 brief): 262,000 chunks. Vectors: 384-d 403 MB; 768-d 805 MB; 1024-d 1.07 GB (arithmetic).
- Indexing time at 144,293 chunks, MiniLM 83 chunks/s = 29 min (arithmetic on #753). A 5-6x slower base model: 2.5-3 h on the Mac, as in the D83 brief (unverified). EVO, 8 threads, CPU ONNX: MiniLM 10-30 min per the #10482 plan (unverified); a base model 1.5-3 h (unverified).

## Comparison table 2: chunking (area 2)

| Approach | Evidence | Cost | Fit with #10481 |
|---|---|---|---|
| AST-aware split and merge (cAST) | Recall@5 +4.3 on RepoEval, Pass@1 +2.67 on SWE-bench, versus line-based chunking (arXiv 2506.15655 abstract). Embedders not named on the page. | Our chunker is already AST-based; the delta is recursive split-and-merge to a token budget. 3-5 files, 150-300 SLOC (D83 estimate for the splitter). | Same work as #10481: token budget, sibling merge. Adopt the merge step inside #10481. |
| Function-level, tight chunks | Greptile: similarity 0.768 for the function alone, 0.739 buried in a file, 0.718 for noise (one query, one author). | None beyond existing chunks. | Supports #10481 splitting at declaration boundaries over blind line cuts. |
| Header on the embedded text (path, symbol signature, parent type) | Anthropic contextual retrieval, prose: failed retrievals -35% with embeddings, -49% with BM25 added, -67% with rerank. LLM-written context. No code-specific number found. | A deterministic header (path + signature) costs about 20-40 tokens per chunk and no LLM call. An LLM-written context costs one call per chunk (144,293 calls). | Fits #10481: the header counts against the 256-token budget. New or changed files only, so inside the zero re-embed budget. |
| Multi-vector chunks, max-sim over pieces | The #10482 plan already simulates max over pieces. No external code number found. | Vectors per chunk rise; +82% chunks is the lower bound. | This is #10481's design (several vectors per chunk). |
| Late chunking | Method: embed the whole text, then pool per chunk (arXiv 2409.04701). Abstract gives no numbers. Needs a long-context embedder. | Needs an 8192-window model (Jina-code, CodeRankEmbed) and a new index. | Needs #10483 first. Defer. |

## Comparison table 3: fusion and routing (area 3)

| Option | Evidence | Cost | Risk |
|---|---|---|---|
| Keep RRF k = 60, tune weights | Bruch et al.: RRF is sensitive to its parameters; convex combination (CC) beats RRF in and out of domain; CC tunes with few labels (arXiv 2210.11934). | Constants only. | Low. |
| Convex combination on normalised scores, one tuned alpha per intent | Same paper. Needs score normalisation (min-max or z) per lane; the HNSW distance and BM25 score have no common scale. | New fusion function beside `rrf_fuse`; tune on the #10482 set plus the code slice. | Normal; rank-only RRF today ignores score gaps that a clear lexical winner shows. |
| Lexical-head guarantee: the top 1-3 BM25 hits keep a slot in the final top 10 when BM25 score margin over rank 2 is large | Own data: hybrid dropped the BM25-first target in 2 of 13 queries (D83 section G). Cursor reports grep plus semantic is best (cursor.com/blog/semsearch). | A few lines after `rrf_fuse`; margin threshold from the A/B. | Low. Could displace a good vector hit. |
| Query-type routing beyond one literal | 67% of `search_lexical` calls are bare names (D83 G). The exact-match floor already covers one literal; keyword bags of 4-12 words get none. | Extend classifier to weight BM25 on identifier-dense bags. | Low-normal. |
| Graph signals (definition, callers) as boosts | No controlled code-retrieval result found. Zoekt calls symbol matches "a key signal" in ranking; GitHub indexes symbols beside content. | KG expansion exists for Usage only; extend to boost definition chunks for Definition intent. | Normal; needs measurement on the code slice. |

## Comparison table 4: rerankers (area 4)

fastembed 5.13.4 ships `TextRerank` with four variants: `BGERerankerBase`, `BGERerankerV2M3`, `JINARerankerV1TurboEn`, `JINARerankerV2BaseMultiligual` (`src/models/reranking.rs:9-15`). ms-marco-MiniLM-L-6-v2 is not a variant; it would load through the user-defined path (`RerankInitOptionsUserDefined`, exported in `lib.rs:105`; ONNX export unverified).

| Reranker | Params | Code evidence | License | CPU latency, top-20 / top-50 (256-token pairs) |
|---|---|---|---|---|
| ms-marco-MiniLM-L-6-v2 | 22.7M | none for code; MS MARCO NDCG@10 74.30 on TREC DL19; 1,800 docs/s on a V100 (model card) | Apache-2.0 | about 0.1-0.3 s / 0.3-0.6 s (unverified) |
| bge-reranker-base | about 278M (unverified) | none found | not checked | about 1-2 s / 2-5 s (unverified) |
| bge-reranker-v2-m3 | 568M (Jina's table), 0.6B (HF) | CodeSearchNet MRR@10 62.86 in Jina's vendor table | Apache-2.0 | about 3-6 s / 8-15 s (unverified) |
| jina-reranker-v2-base-multilingual | 278M | CodeSearchNet MRR@10 71.36, BEIR 53.17 (vendor card); 1024-token window | CC-BY-NC-4.0: research and evaluation only | about 1-2 s / 2-5 s (unverified) |

Findings:

- No published reranker result on CoIR or CodeSearchNet for bge rerankers was found. The only code numbers are Jina's own table.
- A 2026 repository-search benchmark found a cross-encoder on top of BM25+embedding RRF gave the best result on five of six metrics, "only modestly" above embedding-only (arXiv 2607.05443, via search snippet; not read in full).
- Jina v2's non-commercial licence conflicts with an MIT workspace that ships binaries. Do not make it a default or bundle it.
- Reranking is a query-time cost only. It needs no re-embed and no index change.

## Comparison table 5: lexical and cross-system practice (area 5)

| System | Public statement | Source |
|---|---|---|
| GitHub code search (Blackbird) | Lexical only; ngram index, "sparse grams" instead of trigrams; symbols indexed beside content and paths; score-ordered posting lists; shard p99 about 100 ms. No embeddings described. | github.blog engineering post |
| Zoekt | Trigram index plus ctags symbols; symbol match is "a key signal"; optional BM25 scoring. | github.com/sourcegraph/zoekt |
| Sourcegraph Cody | Adapted BM25 plus other signals; dropped ada-002 embeddings for privacy, admin overhead and scale. | sourcegraph.com blog |
| Cursor | Embedding model trained on agent-session traces ranked by an LLM; semantic search lifts answer accuracy 12.5% on average (6.5-23.5%); best with grep. | cursor.com/blog/semsearch |
| Greptile | Translate code to prose before embedding: 0.8152 vs 0.7280 similarity (one query). Chunk per function. | greptile.com blog |
| Continue | Docs page returned 404; not established. | docs.continue.dev (404) |
| Identifier splitting | No controlled study found. Our tokenizer already splits and keeps the compound. | `trusty-common/src/bm25.rs:86` |

Implications for us: a BM25F-style symbol field and a path field are the common practice (Zoekt, GitHub). Our index has one field. A trigram or sparse-gram substring index is the GitHub/Zoekt answer for exact and regex queries; our grep lane and ripgrep fallback cover that for Definition intent only.

## Ranked recommendation

Budgets: Mac = 16 cores, 128 GiB, ~83 chunks/s MiniLM (#753). EVO = 28 cores, ~40 GiB free, 8-thread cap, CPU ONNX. Every RAM, latency and indexing figure below is unverified (arithmetic or parameter-ratio estimate) unless a source is named. "Re-embed" means an amendment to the #6581 zero re-embed ruling is needed.

| # | Item | Expected gain (source) | Cost, rung | Daemon RAM added | Query latency added | Indexing 144,293 chunks | Needs first | #6581 re-embed |
|---|---|---|---|---|---|---|---|---|
| 1 | Fusion fix: lexical-head guarantee, then CC fusion per intent, then identifier-bag routing | Recovers the 2 of 13 buried hits (D83 G); CC > RRF (arXiv 2210.11934, paper, not code) | Small: constants and a post-fusion step, 1-3 files. Rung 3 | about 0 | under 1 ms (unverified) | none | neither; tune with #10482 set + code slice | no |
| 2 | #10481 token-aware split plus deterministic path+signature header on embedded text | cAST +4.3 Recall@5 (2506.15655); header idea -35% failures in prose (Anthropic); window loss on 29.9% of chunks (D83) | 150-300 SLOC plus header builder. Rung 3-4 | about 0 resident; vectors +82% chunks: +181 MB at 384-d (arithmetic) | about 0 (more HNSW candidates; unverified) | new and changed files only; full re-chunk of this index is 1.8-2.1x embed time: Mac about 55 min, EVO about 20-60 min | is #10481 | no for new/changed files; yes for existing indexes |
| 3 | Opt-in cross-encoder rerank of the top 20, flag off by default; ms-marco-MiniLM first, bge-reranker-base second | No code number found; Jina table only for code; hybrid+rerank best in 2607.05443 (snippet) | New stage plus MCP param. Rung 6 (schema), behind a flag | MiniLM about 0.1 GB; bge-base about 1.1 GB (unverified) | MiniLM 0.1-0.3 s, bge-base 1-2 s on top-20; Mac and EVO within 2x of each other (unverified) | none | neither; measure on #10482 set + code slice | no |
| 4 | BM25F-style fields: symbol name, path, doc comment with weights, beside the body | Practice at Zoekt and GitHub; no controlled number found | BM25 index rebuilds from corpus; the field extractor exists in the entity/symbol code. Rung 3-4 | +5-15% BM25 memory (unverified); current corpus cap 400,000 | under 5 ms (unverified) | BM25 rebuild only: minutes, no embedding | neither | no |
| 5 | Add jina-v2-base-code and CodeRankEmbed arms to #10482 Phase A (pooled chunks only) | Published CoIR 58.4 and 60.1 against 42.77 for BGE-Base (CoIR paper, model card) | Eval only: pooled chunks 15-20k plus 10k distractors. Rung 1 (docs) | 0.55-0.65 GB on the EVO job host only | measure p50/p95 per query in the A/B | Phase A under 1 h on EVO per model (unverified); Phase B full corpus 1.5-3 h per model EVO, 2.5-3 h Mac | informs #10483; swap itself needs #10483 | yes, for any swap; 768-d forces a new index |
| 6 | Trigram or sparse-gram substring index for exact and regex queries | GitHub and Zoekt design; no measurement for our data | New index type, persistence. Rung 5 | tens to hundreds of MB (unverified) | grep-class, ms (unverified) | one corpus pass, minutes | neither; only after 1 and 4 show a lexical gap | no |
| 7 | Late chunking, 8192-window code embedder | Qualitative only (2409.04701) | New embedder path | 0.6 GB+ | per-document encode (unverified) | full re-embed | #10483 | yes |
| 8 | Graph boost for Definition intent | no source found | extend `kg.rs` | about 0 | KG lookup ms (unverified) | none | neither | no |

Order rationale: items 1, 3 and 4 touch no vector. Item 2 is already ticketed. Item 5 costs only eval time and decides whether #10483 targets a code model (768-d) or bge-small/arctic-s (384-d).

New versus reused: items 1 (CC fusion, lexical-head guarantee), 3, 4, 5 (the two code arms), 6, 7 and 8 are new. Item 2 reuses #10481 and adds the header and cAST merge step. #10482's arms C1-C8 are unchanged.

## Test set proposal (area 6)

Extend the 190 queries of #10482 with a code slice of 60, 12 per type. Same judge setup and label scale (0-2), pooling depth 30 per system, labels keyed by path + line range + content hash.

| Type (12 each) | Source of query | Label method |
|---|---|---|
| Definition lookup | symbols from the symbol graph, written as "where is X defined" and as a bare name | Programmatic: the defining chunk = 2, its tests = 1 |
| Callers or usage | functions with 2-10 call edges in the symbol graph (`scip_ingest`, `symbol_graph`) | Programmatic: caller chunks = 2, sibling callers' modules = 1; judge audits 20% |
| How does X work | fleet queries re-tagged concept (the #10482 class) plus 4 new | Two-judge LLM pass, kappa >= 0.6, 20% human spot check, per #10482 |
| Error text to source | string literals from `thiserror` messages and `error!`/`warn!` calls, with one word changed in half | Programmatic: the chunk holding the literal = 2 |
| Config or flag lookup | clap flags, env vars, TOML keys | Programmatic: defining chunk = 2, doc mention = 1 |

- Rewrite half of the programmatic queries in natural language without the identifier, so vector lanes are tested and not only BM25.
- Report per type: nDCG@10, Recall@10, MRR, plus how often the top BM25 hit stays in the top 10 (the fusion defect).
- External sanity check: run the same arms on CodeSearchNet and CoSQA subsets of CoIR (about 500-1,000 queries each, sampled with a fixed seed) on EVO. Pass condition: MiniLM-256 lands within a few points of published numbers for comparable models; a large gap means a pipeline error (prefix, pooling, truncation), not a model result. NanoCoIR exists as a small public subset (hakari-bench-leaderboard page seen in search; not read).
- Reuse: the pooled-label tooling, judge prompts and bootstrap from `docs/eval/10482/` once it merges.

## Open questions

- Owner: is a 768-d code model in scope, or only 384-d candidates? This decides whether item 5 matters for #10483.
- Is jina-reranker-v2 (CC-BY-NC) ruled out for evaluation use only?
- Does the lexical-head guarantee need a ruling because it overrides fusion order?
- Who labels the 20% human spot check of the code slice?
- Does the Python sidecar (live path) support the rerank and the user-defined ONNX models, or only fastembed-rs models?

## Not established

- CPU latency and RAM for every model and reranker: no primary source found; all figures are estimates.
- A CoIR or CodeSearchNet score for MiniLM-L6-v2, bge-small and arctic-embed-s.
- arctic-embed-m-v2.0 dimension, window and code score (model card page truncated); CodeRankEmbed dimension.
- Any published code-retrieval result for bge-reranker-base or ms-marco-MiniLM, and any controlled study of identifier splitting.
- Whether CodeRankEmbed and CodeSage export to ONNX that fastembed-rs can load.
- Sweep chunking post (host unreachable) and Continue docs (404).
- arXiv 2607.05443 was seen only as a search snippet.

## Sources

- CoIR benchmark: https://arxiv.org/abs/2407.02883 and https://arxiv.org/html/2407.02883
- CodeRankEmbed card (CSN, CoIR table): https://huggingface.co/nomic-ai/CodeRankEmbed
- jina-embeddings-v2-base-code: https://huggingface.co/jinaai/jina-embeddings-v2-base-code
- gte-modernbert-base: https://huggingface.co/Alibaba-NLP/gte-modernbert-base
- bge-m3: https://huggingface.co/BAAI/bge-m3
- codesage-small-v2: https://huggingface.co/codesage/codesage-small-v2
- Arctic-Embed 2.0: https://arxiv.org/abs/2412.04506 and https://huggingface.co/Snowflake/snowflake-arctic-embed-m-v2.0
- cAST: https://arxiv.org/abs/2506.15655
- Late chunking: https://arxiv.org/abs/2409.04701
- Fusion analysis: https://arxiv.org/abs/2210.11934
- jina-reranker-v2: https://huggingface.co/jinaai/jina-reranker-v2-base-multilingual
- bge-reranker-v2-m3: https://huggingface.co/BAAI/bge-reranker-v2-m3
- ms-marco-MiniLM-L6-v2: https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2
- Scientific code search benchmark: https://arxiv.org/pdf/2607.05443 (snippet only)
- GitHub code search: https://github.blog/engineering/the-technology-behind-githubs-new-code-search/
- Zoekt: https://github.com/sourcegraph/zoekt
- Sourcegraph Cody: https://sourcegraph.com/blog/how-cody-understands-your-codebase
- Cursor: https://cursor.com/blog/semsearch
- Greptile: https://www.greptile.com/blog/semantic-codebase-search
- Anthropic contextual retrieval: https://www.anthropic.com/news/contextual-retrieval
- NanoCoIR page: https://hakari-bench-leaderboard.hf.space/docs/benchmark-tasks/NanoCoIR
- Local: fastembed-5.13.4 `src/models/{text_embedding,reranking}.rs`; Cargo.lock; D83 brief; #10482 plan; #10481/#10483 bodies.
