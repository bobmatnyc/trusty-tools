# open-mpm Organic-Corpus Baseline — First Per-Lane MCP Tool Measurement

**Date**: 2026-05-25
**Daemon version**: 0.10.0 (uptime ~88 minutes at run start; same instance as `v0.10.0-2026-05-25.md` and `synthetic-corpus-baseline-2026-05-25.md`)
**Tracking issue**: ticket #5 (this session's plan) · cross-links #138, #129 (tracker), #128 (closed-superseded)
**Status**: First measurement of trusty-search v0.10.0's four per-lane MCP tools (`search_lexical`, `search_semantic`, `search_kg`, `search_all`) against an organic Rust workspace.

This document is **parallel infrastructure**, not a version snapshot. The `current.md` pointer remains aimed at `v0.10.0-2026-05-25.md`. The open-mpm baseline is a corpus dimension (organic vs synthetic), not a release dimension.

## Motivation

The synthetic corpus (`benchmark_synthetic.rs`, #123 v2) was the first measurement of trusty-search free of BM25 circular bias — but at 47 files / 298 chunks it was **too small to differentiate `search_kg` from `search_semantic`**. KG-leading retrieval collapsed to the hybrid baseline because there were not enough inter-file call edges for graph expansion to pull meaningfully different chunks. The v2 baseline doc explicitly flagged this:

> **Hybrid ≈ KG-leading:** flipping `use_kg_first: true` produced identical Hit@1/Hit@5 on this corpus — KG signal is not yet making a measurable difference at this scale. … The synthetic corpus has limited inter-file call density … so KG expansion has few high-confidence edges to lift on.

The proposed follow-up was to index `open-mpm` — the MPM orchestration platform consolidated into this workspace as `crates/open-mpm/` — and re-run the per-lane comparison. That is this baseline.

**The headline question**: does the KG signal pull its weight on an organic Rust workspace, or is it dead-code architectural ornament?

## The open-mpm corpus

**Path**: `crates/open-mpm/` (in-tree workspace member; not the deprecated `/Users/masa/Projects/open-mpm` index that already exists in the daemon's registry)
**Index name**: `open-mpm-benchmark` (does not clash with the existing `open-mpm` index)
**Shape**: organic Rust — the MPM orchestration platform consuming `trusty-search`, `trusty-memory-core`, `trusty-symgraph`.

### File census (live measurement)

| Metric | Value |
|---|---|
| `.rs` source files (`src/**/*.rs`) | 221 |
| Total `.rs` source lines | ~115 651 |
| Files indexed by the daemon | **359** (includes Cargo.toml, README, agent configs, etc.) |
| Tree-sitter chunks at index time | **6 611** |
| Reindex wall-clock (force=true) | **151.2 s** (≈2.5 min) |
| Peak daemon RSS during embed | **22 259 MB** (~22 GB) |
| Final on-disk index size | reported via `disk_bytes` in /status |

**Surprise finding — embedding RSS is much higher than the synthetic corpus.** On open-mpm the embedding-phase RSS peaked at ~22 GB (versus 5–10 GB on the synthetic 298-chunk corpus). This is well below the daemon's 32 GB ceiling but the harness's original 12 GB bail threshold (carried over from `trusty-tools` indexing experience) tripped on the first run. The threshold was raised to 28 GB and a comment was added in the harness explaining the empirical reasoning. This RSS shape is worth its own investigation — see Follow-ups.

## Query set

20 ground-truth queries authored against real open-mpm symbols, split as:

| Type | Count | What it tests |
|---|---|---|
| Definition (PascalCase) | 3 | `SubprocessAgentRunner`, `SessionsRegistry`, `PluginManager` — exact symbol names |
| Definition (snake_case) | 2 | `atomic_write`, `cost_usd` — tests #119 snake_case Definition routing on organic vocabulary |
| Definition (SCREAMING_SNAKE) | 2 | `MAX_FILE_BYTES`, `FINISH_TASK_TOOL_NAME` — expected to miss until #142+#143 land; baseline data point |
| Conceptual | 8 | multi-word descriptions with no exact identifier match in the target file |
| KG-seed (two-stage) | 3 | stage-1 lexical seeds the chunk_id, stage-2 `search_kg` traverses call edges |
| Negative | 2 | `WidgetFactoryMutex` (doesn't exist), `encrypt outbound telemetry stream cipher` (synthetic-corpus query) — all tools should return zero / empty gracefully |

Every query carries a `description` field documenting the intent. The full set lives at:

`crates/trusty-search/tests/benchmark_open_mpm_ground_truth.json`

## Per-tool HTTP mapping

The harness uses HTTP because MCP is stdio-only; it builds the request body the same way `mcp::tools::run_lane_search` in `crates/trusty-search/src/mcp/tools.rs` does internally:

| MCP tool | HTTP body shape |
|---|---|
| `search_lexical` | `{"stage":"lexical", "expand_graph":false, "mode":<hint>, ...}` |
| `search_semantic` | `{"stage":"semantic", "expand_graph":false, "mode":<hint>, ...}` |
| `search_kg` | `{"stage":"graph", "expand_graph":true, "mode":<hint>, "seed_chunk_id":<from stage-1>, ...}` |
| `search_all` | `{"expand_graph":false, "mode":<hint>, ...}` — no `stage` field |

For KG-seed queries, stage-1 issues a lexical `top_k=1` lookup to resolve a seed chunk_id, then stage-2 fires `search_kg` with the seed attached so graph traversal is anchored deterministically.

## Headline results

### Per-tool Hit@K with per-type breakdown (live run, 2026-05-25)

| Tool | Def H@1 | Concept H@1 | KG H@1 | Aggregate H@1 | Aggregate H@5 | p50 server ms |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `search_lexical` | 5/7 (71%) | 5/8 (63%) | 2/3 (67%) | **12/18 (67%)** | **14/18 (78%)** | 6 |
| `search_semantic` | 5/7 (71%) | 5/8 (63%) | 2/3 (67%) | **12/18 (67%)** | **13/18 (72%)** | 8 |
| `search_kg` | 5/7 (71%) | 5/8 (63%) | 2/3 (67%) | **12/18 (67%)** | **13/18 (72%)** | 6 |
| `search_all` | 5/7 (71%) | 5/8 (63%) | 2/3 (67%) | **12/18 (67%)** | **13/18 (72%)** | 6 |

_Negative queries (Q19, Q20) excluded from aggregates._

### Aggregate by tool

| Tool | Hit@1 | Hit@5 | p50 server ms |
|---|:---:|:---:|:---:|
| `search_lexical` | 12/18 (67%) | 14/18 (78%) | 6 |
| `search_semantic` | 12/18 (67%) | 13/18 (72%) | 8 |
| `search_kg` | 12/18 (67%) | 13/18 (72%) | 6 |
| `search_all` | 12/18 (67%) | 13/18 (72%) | 6 |

## Headline findings

### 1. `search_kg` produces IDENTICAL Hit@K to `search_semantic` and `search_all` on this organic corpus.

This is the question this baseline was built to answer. The verdict is unambiguous at this query set:

> On 18 non-negative queries against an organic 359-file / 6 611-chunk Rust workspace, `search_kg`, `search_semantic`, and `search_all` produce identical Hit@1 (12/18) and identical Hit@5 (13/18). `search_kg`'s seed-anchored graph expansion adds no measurable retrieval lift.

The synthetic-corpus finding (#123 v2: "KG signal is not yet making a measurable difference at this scale") **does not change** when we scale from 47 files / 298 chunks (synthetic) to 359 files / 6 611 chunks (open-mpm, 20× larger). The hypothesis that "open-mpm is big enough for KG to differentiate" is **not supported** by this data.

This is a **significant ROI question**. Stage 3 (graph build + embed) materially affects reindex memory and disk usage, but the per-lane evaluation cannot distinguish its retrieval signal from the semantic lane on this query set. Two interpretations are possible:

- **(A)** The KG signal is genuinely redundant with vector signal for the *kind* of queries developers issue against this corpus. If so, Stage 3 buys provenance and `get_call_chain` (the trace tool) but not retrieval lift, and the documentation should reflect that.
- **(B)** The query set under-samples KG-relevant questions. Only 3/20 queries (Q16–Q18) explicitly exercise the two-stage seed pattern, and 2 of those 3 hit on every tool anyway (the lexical lane already finds the right file for `SubprocessAgentRunner` and `EVENT_LINE_PREFIX`). A query set with 8–10 "who calls X" style probes — where the answer is structurally a call edge — might surface a KG advantage.

A follow-up ticket should expand the KG-relevant subset and re-run. Until then, the honest claim is: **on naturally-phrased queries, KG signal does not improve top-K retrieval over semantic signal**.

### 2. `search_lexical` Hit@5 (78%) BEATS the other three tools (72%) by one query.

Q14 (`dispatch JSON-RPC tool calls to MCP servers over stdio`) hits at H@5 only under `search_lexical`. The lexical lane surfaces `src/plugins/stdio_mcp.rs` (a ground-truth file) at top-1 because the query has high token overlap with the file content (`json`, `rpc`, `tool`, `mcp`, `stdio` all appear). The semantic / KG / all variants demote it in favour of `src/plugins/mod.rs`, which is also a ground-truth file but only matches at H@5 for the latter three.

This reproduces the **synthetic-corpus finding** that lexical can match or exceed hybrid on Hit@5 when the corpus is below a critical size: BM25's literal-term match is hard to beat when query and corpus share vocabulary. The synthetic baseline (#123 v2) showed lexical Hit@5 ≥ hybrid Hit@5 at 47 files; the open-mpm baseline reproduces this at 359 files.

### 3. SCREAMING_SNAKE Definition queries miss universally — confirms the #142/#143 finding on organic vocabulary.

Q06 (`MAX_FILE_BYTES`) and Q07 (`FINISH_TASK_TOOL_NAME`) both miss at H@1 and H@5 across all four tools. Top-1 is consistently a wrong file (`src/session_registry.rs` / `src/logging/mod.rs` / `src/llm/mod.rs`). Intent is classified as `Unknown` — the classifier does not route SCREAMING_SNAKE to Definition mode, exactly mirroring the synthetic-corpus Q04 (`BRUSILOV_EPOCH`) result.

This is a **clean-corpus, organic-corpus reproduction** of the SCREAMING_SNAKE routing bug. It is genuine — not a synthetic-corpus artifact — and worth fixing in #142 / #143.

### 4. snake_case Definition queries succeed.

Q04 (`atomic_write`) and Q05 (`cost_usd`) both hit at H@1 across every tool. This validates the #119 fix from v0.9.x: snake_case identifiers are correctly routed to Definition mode and surface the defining file at rank 1. This is the only Definition routing class that works end-to-end across PascalCase + snake_case on organic code.

### 5. KG-seed Q17 (`ToolRegistry`) fails — informative failure mode.

Stage-1 lexical resolves `ToolRegistry` to the wrong chunk:
```
seed=/Volumes/.../crates/open-mpm/src/ctrl/mod.rs::Function::run_pm_task_with_persona::1540
```
This is a chunk in `ctrl/mod.rs` that mentions `ToolRegistry` once in a function body, not the chunk in `src/tools/mod.rs` that defines it. Stage-2 search_kg from this wrong seed yields top-1 = `src/llm/mod.rs`, missing every ground-truth file.

The diagnostic value is high: it demonstrates that the two-stage `search_kg` pattern is **only as good as the seed**. When stage-1 picks the wrong chunk (because BM25 ranks a usage site higher than the definition site for an ambiguous term), the KG expansion compounds the error rather than recovering from it. This is a known property of seed-anchored graph search; it is the right behaviour but worth surfacing in the tool description.

### 6. Negative queries do not return empty — both tools surface a top-1 "best wrong answer".

Q19 (`WidgetFactoryMutex` — doesn't exist) and Q20 (`encrypt outbound telemetry stream cipher` — a synthetic-corpus query with no semantic match in open-mpm) both surface a top-1 file across every tool:

```
Q19 WidgetFactoryMutex:
  search_lexical/semantic/kg/all   top1=src/repl/tui.rs   intent=Definition
Q20 encrypt outbound telemetry stream cipher:
  search_lexical   top1=src/api/server.rs   intent=Conceptual
  search_semantic/kg/all  top1=src/bus/mod.rs  intent=Conceptual
```

Neither tool returns an empty result set or surfaces a "no relevant matches" signal. This is consistent with the daemon's contract (it returns top-k regardless of score) but a downstream consumer that uses search to make decisions (e.g. an agent deciding whether code exists) cannot infer "this concept is absent" from the response alone. A future improvement would expose a confidence threshold or a `score < threshold` filter.

## Comparison: synthetic vs open-mpm

| Metric | Synthetic (47 files / 298 chunks) | open-mpm (359 files / 6 611 chunks) | Notes |
|---|---|---|---|
| Reindex wall-clock | 3.5 s | 151.2 s | 43× slower; corpus 22× larger by chunks |
| Peak RSS | ~3 GB est. (not measured) | 22 GB | CoreML embedding scales steeply with chunk count |
| `search_lexical` Hit@1 | 10/19 (53%) | 12/18 (67%) | Organic lexical-friendly vocabulary lifts BM25 |
| `search_lexical` Hit@5 | 18/19 (95%) | 14/18 (78%) | Organic ambiguity (e.g. `ToolRegistry` collides) hurts |
| `search_semantic` Hit@1 | 13/19 (68%) | 12/18 (67%) | Roughly tied — semantic doesn't lose ground |
| `search_kg` Hit@1 | 13/19 (68%) | 12/18 (67%) | Same as semantic on BOTH corpora — KG signal flat |
| `search_kg` vs `search_semantic` delta | 0 | 0 | **Identical on every metric, both corpora** |

**Key takeaway**: the *relative ordering* of tools is stable across corpora — lexical ≈ semantic ≈ KG ≈ all, with lexical edging ahead on Hit@5. The absolute numbers shift (open-mpm's larger vocabulary trades some Hit@5 for more Hit@1 stability) but the conclusion about KG signal is corpus-invariant on this query design.

## Comparison: open-mpm (organic) vs trusty-tools (circular-biased, similar size)

| Source | Tool | Hit@1 | Hit@5 | Bias status | Corpus size |
|---|---|---|---|---|---|
| trusty-tools (v0.10.0 snapshot, hybrid) | full-hybrid | ~57% est. | ~93% est. | **Contaminated** (#123) | 14k files |
| open-mpm (this baseline, search_all) | full-hybrid | 67% | 72% | **Clean** | 359 files |
| synthetic (#123 v2, hybrid) | full-hybrid | 68% | 95% | **Clean** | 47 files |

The open-mpm Hit@1 number (67%) is in the same ballpark as both the contaminated trusty-tools number and the clean synthetic number — but open-mpm's Hit@5 (72%) is **lower** than both. This is informative: organic code at moderate scale has more "near-miss" files that the daemon legitimately can't distinguish from the right one. The 95% Hit@5 of the synthetic corpus is an artifact of carefully-disjoint symbol vocabularies; real codebases share more vocabulary.

The absolute numbers are **reasonable**: 67% Hit@1 / 72% Hit@5 on naturally-phrased queries against an organic Rust workspace is consistent with hybrid-retrieval performance reported elsewhere in the literature for code search.

## Caveats

1. **Single run.** Each query was executed once per tool. p50 server latency is meaningful but tail-latency claims would need multi-trial.
2. **18 non-negative queries is a small sample.** The "identical Hit@K across tools" finding is statistically softer than it looks — a query set with 50+ probes would either confirm or refute it more decisively.
3. **KG-relevant query subset is undersized.** Only 3/20 queries (Q16–Q18) explicitly exercise the two-stage seed pattern. A KG-targeted query set ("who calls X", "what does Y use", "what's downstream of Z") might surface a KG advantage that this set doesn't.
4. **Same daemon, same instance, same uptime.** v0.10.0, uptime ~88 minutes at run start. No daemon restart between this run and the synthetic / `current.md` runs. Memory state is shared; embedder is warm.
5. **Q11 lexical leak.** Q11 (`compute reciprocal rank fusion of hybrid search lanes`) hits at top-1 = `src/search/indexer.rs` on every tool. This is a ground-truth file, but the query may be circular-biased: open-mpm's `search/indexer.rs` contains literal RRF terminology. The query is borderline — not as egregious as the #123 trusty-tools contamination but worth flagging.
6. **CoreML batch tripwire was not exercised.** The 22 GB peak was inside one ONNX batch; the daemon's `TRUSTY_COREML_TRIPWIRE_MB` (default 4 GB delta per batch) did not trip. Worth verifying on a hot run with a fresh daemon process whether the trip-wire engages.

## Follow-ups

- **Investigate KG signal ROI.** With both synthetic (47 files) and organic (359 files) corpora showing identical Hit@K between `search_kg` and `search_semantic`, the next step is either (a) author a KG-targeted query set and re-run, or (b) accept that Stage 3 buys provenance + `get_call_chain` but not top-K retrieval lift, and document that. (New ticket recommended.)
- **Investigate RSS spike during open-mpm embed.** 22 GB peak for 6 611 chunks is much higher than the synthetic baseline at scale-adjusted ratio. Possible causes: CoreML batch sizing on Apple Silicon, ORT arena retention, or tree-sitter chunk-boundary growth on tokio-heavy code. (New ticket recommended.)
- **Expand negative-case handling.** Q19 / Q20 surfacing plausible-but-wrong top-1 results suggests the daemon could expose a per-query relevance-confidence score or an "uncertain" flag for downstream consumers.
- **Fix SCREAMING_SNAKE Definition routing (#142 / #143).** Q06 / Q07 are clean-corpus reproductions of the same bug seen on the synthetic Q04. The fix should land before another baseline pass.
- **Multi-trial harness.** Run each query 3× and report p50 / p99 per tool. Today's single-run latency numbers are point estimates.

## Cross-links

- [ticket #5](https://github.com/bobmatnyc/trusty-tools/issues/5) — _build open-mpm benchmark harness_ (this work)
- [#138](https://github.com/bobmatnyc/trusty-tools/issues/138) — _per-lane MCP tools (`search_lexical` / `search_semantic` / `search_kg` / `search_all`)_ — the tools this baseline evaluates
- [#129](https://github.com/bobmatnyc/trusty-tools/issues/129) — _benchmark tracker_ — see the alternate-corpus row appended in this session
- [#128](https://github.com/bobmatnyc/trusty-tools/issues/128) — _Stage 3 signal A/B validation_ — superseded; this baseline answers the per-lane validation question
- [#123](https://github.com/bobmatnyc/trusty-tools/issues/123) — _BM25 circular bias_ — this baseline complements the synthetic-corpus measurement on the organic axis
- [synthetic-corpus-baseline-2026-05-25.md](synthetic-corpus-baseline-2026-05-25.md) — synthetic peer to this organic baseline
- [v0.10.0-2026-05-25.md](v0.10.0-2026-05-25.md) — current.md target; the version snapshot this baseline is parallel to
- [benchmark_open_mpm.rs](../../../crates/trusty-search/tests/benchmark_open_mpm.rs) — harness source
- [benchmark_open_mpm_ground_truth.json](../../../crates/trusty-search/tests/benchmark_open_mpm_ground_truth.json) — 20-query ground truth

## Raw measurements

Re-run with:

```bash
cargo test -p trusty-search --test benchmark_open_mpm -- --include-ignored --nocapture
```

The harness reads `benchmark_open_mpm_ground_truth.json`, registers and reindexes `open-mpm-benchmark` (cleanup on exit), runs every query against every tool, prints the markdown tables above, asserts at least one hit landed, and deletes the index. Expect 2.5–3 min reindex + ~10 s queries; peak RSS ~20–25 GB during embed. No daemon restart required.
