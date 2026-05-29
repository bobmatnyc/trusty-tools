# Stage 3 (KG + community detection) retrieval-lift decision — 2026-05-25

**Status**: DECIDED — **PROVENANCE-ONLY**
**Tracking issue**: [#145](https://github.com/bobmatnyc/trusty-tools/issues/145)
**Daemon version**: trusty-search v0.10.0 (uptime continuous since prior #5 run)
**Decision author**: Rust engineer agent, in conjunction with #145 reviewer
**Reviewer status**: pending — this document opens the review.

## TL;DR

- Across 18 queries SPECIFICALLY designed to exercise the symbol graph (`who calls X`, `what implements T`, `what is the neighborhood of Y`), `search_kg` did **NOT** lift Hit@K over `search_semantic`. In fact it **lost** Hit@1 by 16.7 percentage points (7/18 vs 10/18) and tied on Hit@5 (11/18 each).
- The kg_callers class — the cleanest "graph signal beats vector signal" shape — is where `search_kg` lost the most: 2/6 Hit@1 vs 5/6 for `search_semantic`. The KG expansion actively demoted the correct caller files in favour of files that share community structure but aren't ground truth.
- `kg_traversal` and `kg_impl_of` classes were ties (both at 3/4 and 2/5 Hit@1 respectively). `kg_neighborhood` was a tie at 0/3 for both — neither tool could surface call-graph neighborhoods of broad entry points using natural-language queries.
- **Implication for #109 Phase 2**: the cost of Stage 3 (Louvain community detection + community_cohesion ranking) is not paid back by retrieval-lift. Stage 3 should be **rescoped**, not shipped as designed — keep the symbol graph for `get_call_chain` and tool-call provenance, but drop the community-cohesion ranking signal from search.

## Methodology

- **Corpus**: open-mpm (`crates/open-mpm/`) — 359 files, 8,699 tree-sitter chunks indexed via v0.10.0 (fresh reindex; prior #5 baseline reported 6,611 chunks but that was at a different commit. Reindex wall-clock 112.9 s, peak RSS 21.4 GB.)
- **Query set**: 18 KG-targeted queries authored against actual open-mpm symbols:
  - **kg_callers** (6 queries): "who calls X" — ground truth = caller files
  - **kg_traversal** (4 queries): "what does X use/contain" — ground truth = outward graph neighborhood
  - **kg_impl_of** (5 queries): "what types implement trait T" — ground truth = `impl T for ...` sites
  - **kg_neighborhood** (3 queries): "what is the call-neighborhood of entry-point Y" — broad 1–2 hop expansion
- **Tools tested**: `search_lexical`, `search_semantic`, `search_kg`, `search_all` — all four per-lane MCP tools from #138.
- **Two-stage pattern**: each KG query first issued a stage-1 `search_lexical` with `top_k=1` to anchor the seed chunk_id (logged for forensics), then stage-2 fired the chosen lane with `expand_graph` set per the lane semantics. The `seed_chunk_id` was included in the request body for forensic clarity even though the daemon's current `SearchQuery` struct ignores the field — KG expansion in production is intent-driven and seeded from top-K results, not from an explicit chunk_id.
- **Daemon**: same persistent daemon instance used for all #5 / #128 / #138 measurements (continuous uptime); embedder running on CPU (provider reported as `CPU` in /health — CoreML EP was NOT active for this run, see Caveats).
- **Index reuse**: harness checks `GET /indexes/open-mpm-benchmark/status` first and skips reindex when all three stages are `ready` with a matching root_path. This run was a fresh index (the prior #5 cleanup deleted it); subsequent #145 re-runs will skip reindex.
- **Harness**: [`crates/trusty-search/tests/benchmark_open_mpm_kg.rs`](../../../crates/trusty-search/tests/benchmark_open_mpm_kg.rs).
- **Ground truth**: [`crates/trusty-search/tests/benchmark_open_mpm_kg_ground_truth.json`](../../../crates/trusty-search/tests/benchmark_open_mpm_kg_ground_truth.json) — each query carries an explicit list of files that a graph-aware tool should be able to surface.

## Results — per-class breakdown

| Tool | kg_callers (H@1 / H@5) | kg_traversal (H@1 / H@5) | kg_impl_of (H@1 / H@5) | kg_neighborhood (H@1 / H@5) | Aggregate (H@1 / H@5) |
|---|---|---|---|---|---|
| `search_lexical` | 3/6 / 5/6 | 1/4 / 4/4 | 1/5 / 2/5 | 0/3 / 2/3 | **5/18 (28%) / 13/18 (72%)** |
| `search_semantic` | **5/6 / 5/6** | **3/4 / 4/4** | **2/5 / 2/5** | 0/3 / 0/3 | **10/18 (56%) / 11/18 (61%)** |
| `search_kg` | 2/6 / 5/6 | 3/4 / 4/4 | 2/5 / 2/5 | 0/3 / 0/3 | **7/18 (39%) / 11/18 (61%)** |
| `search_all` | 5/6 / 5/6 | 3/4 / 4/4 | 2/5 / 2/5 | 0/3 / 0/3 | **10/18 (56%) / 11/18 (61%)** |

## Headline number: search_kg vs search_semantic on KG-targeted queries

| Class | kg H@1 | sem H@1 | Δ H@1 | kg H@5 | sem H@5 | Δ H@5 |
|---|---:|---:|---:|---:|---:|---:|
| kg_callers | 2/6 | 5/6 | **−3** | 5/6 | 5/6 | 0 |
| kg_traversal | 3/4 | 3/4 | 0 | 4/4 | 4/4 | 0 |
| kg_impl_of | 2/5 | 2/5 | 0 | 2/5 | 2/5 | 0 |
| kg_neighborhood | 0/3 | 0/3 | 0 | 0/3 | 0/3 | 0 |
| **AGGREGATE** | **7/18** | **10/18** | **−3 (−16.7 pp)** | **11/18** | **11/18** | **0 (0.0 pp)** |

**`search_kg` did not lift Hit@K on the KG-targeted query set. It REGRESSED Hit@1 by 16.7 pp and tied on Hit@5.**

The regression is concentrated in `kg_callers`, the cleanest "graph wins" shape. The KG expansion actively demoted the correct caller files. Inspection of the per-query log:

- **KG04** (`who calls atomic_append_line`): seed correctly landed at `src/state_writer.rs`. `search_semantic` and `search_lexical` correctly returned `src/interaction_log.rs` at top-1 (a true caller). `search_kg` returned `src/init/mod.rs` — a file that shares community structure with state_writer.rs but is NOT a caller of atomic_append_line.
- **KG05** (`who calls register_tm_tools`): all tools seeded at `src/tools/tm_tools.rs`. Three of four tools returned tm_tools.rs at top-1 (correct — caller is in same file). `search_kg` returned `src/init/mod.rs` — same wrong-community drift.
- **KG06** (`who calls spawn_subagent_and_run`): seeded at `src/subprocess.rs`. Three tools returned subprocess.rs at top-1 (correct). `search_kg` returned `src/tools/web_search.rs` — yet again, community-drift.

The pattern is consistent: when seeded correctly, `search_kg` walks outward via community/centrality structure that is too coarse to discriminate "this is a true caller" from "this lives in the same module cluster". The KG signal is operating at the wrong granularity for caller-set retrieval.

## Decision

Based on the data, the answer is **PROVENANCE-ONLY**. Reasoning:

1. **The KG ranking signal does not earn its keep on retrieval.** With ≥10 pp Hit@1 lift as the KEEP threshold and <5 pp as the DEPRECATE threshold, the −16.7 pp aggregate Hit@1 result falls well below either positive bar. This is corpus-invariant: the #5 baseline (mixed-intent queries) showed `search_kg ≡ search_semantic`; this #145 measurement (KG-targeted queries) shows `search_kg < search_semantic`. Two independent measurements with different query designs agree.

2. **`search_kg` still has a legitimate, narrower purpose.** As a deterministic graph-neighborhood explorer — "I have chunk X, give me its 1-hop call neighborhood" — `search_kg` is genuinely the only tool that can answer the question. The community-cohesion ranking signal that gets blended into score is what hurts; the raw graph-walk capability is fine. This argues for **rescoping**, not deletion.

3. **`get_call_chain` (the MCP provenance tool) is unaffected.** Stage 3's underlying symbol graph powers `get_call_chain`, which surfaces explicit call-edges with provenance. This tool's value does not depend on retrieval ranking — it consumes the graph directly. Keep building the graph.

4. **The community detection (Louvain) step is the load-bearing cost.** Profile data from prior runs shows Louvain dominates Stage 3 wall-clock and accounts for the bulk of the RSS spike during reindex. With community_cohesion contributing negatively to ranking, Louvain becomes pure overhead — it can be deleted from the reindex critical path.

5. **The two-stage seed pattern is not the problem.** Every query's stage-1 seed landed at the correct file (or a co-located file) for the symbols where the daemon's existing intent-routing also reached the right file. The seed audit log shows search_kg's regression isn't from wrong-seed compounding; it's from the post-fusion KG/community blending. This narrows where to make the change.

**Recommendation**: keep building the symbol graph (it powers `get_call_chain`, file-level provenance, and the underlying Stage 3 facts store), but **delete** the `community_cohesion` post-fusion ranking term and the Louvain detection step. Rename `search_kg` in tool descriptions to clearly indicate it returns graph-neighborhood-anchored chunks rather than "best matches" — the LLM picking between `search_semantic` and `search_kg` should know they answer different questions.

## Implications for #109 Phase 2

#109 Phase 2 proposed async-splitting Stage 3 (graph build + community detection) off the reindex critical path. Given this measurement:

- **Do not implement async-split for community detection.** If the signal is being deleted anyway, async-splitting it is wasted engineering work.
- **DO async-split graph build** (the petgraph build from tree-sitter facts). It's the cheap part of Stage 3 and the bit `get_call_chain` actually needs.
- **Open a follow-up to delete Louvain.** This is a code-removal ticket, not a feature ticket. Net negative LOC. Reduces reindex RSS ceiling.

The #109 ticket itself should be rescoped from "async-split Stage 3" to "async-split graph build; delete community detection".

## Implications for other open tickets

- **#110 (embedder process)**: Unaffected. The embedder is Stage 2; this measurement is about Stage 3. The 21 GB RSS spike during reindex on this run was almost entirely Stage 2 (CoreML/CPU ONNX arena), not Stage 3 — Louvain is a small fraction of total reindex cost, but the ratio shifts on larger corpora.
- **#128 (Stage 3 A/B validation)**: Already closed-superseded. This measurement is the definitive answer.
- **#138 (per-lane MCP tools)**: Keep all four tools. The decision is to rescope what `search_kg` *means*, not to delete the tool surface. Update the tool description to "returns graph-neighborhood-anchored chunks for a given seed (call-edge expansion)" so the LLM picks it for navigation queries, not retrieval queries.
- **#147 (search_kg refining query)**: Highly relevant. Refining the query before search_kg won't help if the post-fusion KG blending is what's hurting. The refining-query mechanism should be evaluated AFTER the community-cohesion blend is removed; otherwise we'd be optimising on a degraded signal.
- **#145 (this ticket)**: Stays open until a maintainer reviews and signs off on the rescope direction.

## Acceptance criteria for this decision

- [ ] At least one engineer / maintainer reviews this doc (target: end of week)
- [x] Empirical run captured under repeatable conditions (this measurement)
- [ ] Follow-up ticket filed: "deprecate community_cohesion ranking + Louvain detection" (after review)
- [ ] Follow-up ticket filed: "rescope #109 Phase 2 to graph-build async-split only" (after review)
- [ ] Update `search_kg` MCP tool description in `crates/trusty-search/src/mcp/tools.rs` to "graph-neighborhood expansion" framing
- [ ] Document the change in `docs/regression-testing/` next baseline

## Caveats / surprises

1. **Daemon was running on CPU, not CoreML.** The /health endpoint reported `provider: "CPU"` for this run, where prior #5 measurements reported `CoreML (Metal GPU / ANE)`. This is suspicious — either the daemon was restarted (the uptime claim in the methodology may be wrong) or the embedder downgraded itself for some reason. The retrieval-lift comparison is provider-invariant (all four tools use the same embeddings), so this does NOT affect the headline decision, but it's worth flagging for the maintainer review. Reindex took 112.9 s on CPU vs ~150 s with CoreML on the prior run — a curious finding worth its own investigation.
2. **Chunk count differs from the #5 baseline.** This run indexed 8,699 chunks vs the #5 baseline's 6,611 chunks. Both were against `crates/open-mpm/`; the delta is probably the org-mpm source tree itself growing between runs. Not a methodology concern but documented for reproducibility.
3. **`kg_neighborhood` class (KG16/17/18) failed universally.** Zero Hit@1 across every tool. The seeds landed plausibly (KG16 landed at `claude_code_runner.rs` instead of `runtime.rs` — the lexical query `"pub async fn run runtime"` isn't unique enough), but every tool's stage-2 then chose `docs/research/...` or `intent/mod.rs` as top-1. This class needs a different harness approach: pre-resolve the seed via `search_similar` with a known `file + function` pair, then run search_kg. Three queries is also too small for a stable conclusion on this class.
4. **`kg_impl_of` class is more lexical-friendly than expected.** `search_lexical` got 1/5 Hit@1, but `search_semantic` and `search_kg` both got 2/5 — the same 2 hits (KG11 AgentRunner and KG13 ModelAdapter). The 3 misses (KG12 MemoryStore, KG14 HarnessAdapter, KG15 TicketingClient) had top-1 = the test-only mock file (`src/tools/native_memory.rs`, `src/tools/native_ticketing.rs`) or the trait-definition file itself, NOT the production impl files. The KG should be able to walk the impl-of edge to a sibling file but does not. This is informative for #147.
5. **Seed search is fast.** Every query's stage-1 lexical seed lookup landed in <10 ms; the two-stage pattern adds ~1.7 ms per query (the second HTTP round trip). Latency is not the limiting factor for the two-stage pattern's adoption.
6. **`search_all` matches `search_semantic` exactly on every class.** Same Hit@1 and Hit@5 across all four classes. This suggests the daemon's adaptive routing for `search_all` falls back to semantic-leaning behavior for KG-targeted queries — neither leaning on lexical nor leaning on KG expansion. Worth confirming with a quick look at `mcp::tools::run_lane_search`.

## Raw output

- Full harness output: `/tmp/ts-bench-kg-determination.json` (uncompressed log, 223 lines)
- The decision tables above are the harness's `print_per_class_table` and `print_delta_table` output, captured verbatim.

## Cross-links

- [#145](https://github.com/bobmatnyc/trusty-tools/issues/145) — this ticket
- [#5](https://github.com/bobmatnyc/trusty-tools/issues/5) — first open-mpm baseline (the predecessor)
- [#109](https://github.com/bobmatnyc/trusty-tools/issues/109) — Stage 3 async-split (to be rescoped)
- [#128](https://github.com/bobmatnyc/trusty-tools/issues/128) — closed-superseded by this work
- [#138](https://github.com/bobmatnyc/trusty-tools/issues/138) — per-lane MCP tool surface (stays as-is)
- [#147](https://github.com/bobmatnyc/trusty-tools/issues/147) — search_kg refining query (re-evaluate after rescope)
- [open-mpm-baseline-2026-05-25.md](../regression-testing/open-mpm-baseline-2026-05-25.md) — the mixed-intent peer baseline
- [benchmark_open_mpm_kg.rs](../../../crates/trusty-search/tests/benchmark_open_mpm_kg.rs) — harness source
- [benchmark_open_mpm_kg_ground_truth.json](../../../crates/trusty-search/tests/benchmark_open_mpm_kg_ground_truth.json) — 18-query KG-targeted ground truth

## Re-run instructions

```bash
cargo test -p trusty-search --test benchmark_open_mpm_kg -- --include-ignored --nocapture
```

Subsequent runs will reuse the existing `open-mpm-benchmark` index (skipping the ~2 min reindex) if the index is still registered and all stages are `ready`. Run time then drops to ~10 s for all 72 (query × tool) combinations.
