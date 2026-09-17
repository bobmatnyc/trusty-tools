# Frozen prompt-enrichment evaluation

Question: does quick graph lookup add useful evidence to a bounded prompt, and is its cost justified relative to contextual BM25 and local embeddings? This extends the earlier source-retrieval experiment; the measurement unit is now complete, cited factual claims in the actual prompt packet.

The [interface](interface.md) defines schemas, selection, output packing, and measurement. The [source review](research.md) identifies actual APIs and distinguishes standing-cache access from the prompt hook's newest-200 lexical KG selection. No production configuration, live memory, or installed service changes. This is an isolated experiment on explicit synthetic facts, not an evaluation of automatic fact extraction or generated answers.

## Inputs and split

There are 1,422 initial sources containing 1,434 exact-span assertions, 12 revision/deletion events, and 102 queries. Three new fictional families form each split: Alder/Birch/Cobalt for tuning, Nimbus/Orchid/Sequoia for heldout. Each split has 51 queries. Heldout prompts use different wording but share scenario structure; this is stronger than renaming alone, still not a real-query distribution or fully independent task-family test.

Each family has task facts, one always-on preference, 220 recent structural inventory assertions, and a same-name source in another scope. The inventory tests whether newest-first filtering hides older useful facts. Every graph assertion also appears verbatim in source prose. Edges are explicit fixture structure, not inferred or extracted using gold. Claims that share a source remain individually labelled.

The 17 categories per family are standing preference, exact entity, alias, one hop, two hops, inverse relation, semantic paraphrase, lexical release rule, current owner, historical owner, ambiguous alias, hub/noise, expired access, knowledge cutoff, unsupported motivation, revised release rule, and deleted source. Initial current time is September 1, 2026; historical time is June 1. Queries supply scope and time; entity hints are null. Entity resolution costs therefore count, and no expected answer entity is handed to the graph.

Gold is in its own file. Required groups allow equivalent evidence; acceptable facts cover relevant supporting context. Shared standing preferences are separately evaluated and excluded from task F1/coverage. Unsupported task-empty rate excludes that legitimate common prelude, while total packet tokens still include it. Cross-scope, superseded, expired, future-known, and old/deleted revisions are explicitly forbidden where applicable; eligibility violations are independently checked even when not enumerated in gold.

Sources, queries, gold, this protocol, and the interface are hashed before any retrieval tuning. Do not change labels after examining outputs. Freeze tuning selection before opening heldout gold. The older memory experiment's fixtures and results do not participate.

## Predeclared comparisons

Four main treatments share eligibility and extractive packing: contextual BM25, BM25+graph, BM25+dense, and BM25+graph+dense. Controls measure standing cache, the current lexical graph policy, graph-only lookup, and whole-source BM25 packing. Prompt budgets are 128, 256, and 512 cl100k tokens; facts remain whole. No fluent rewriting or answer generation occurs.

Graph tuning varies max seeds/hops over (1,1), (3,1), (1,2), (3,2), with 128 examined edges and 32 emitted facts. Dense cosine floors are 0.25, 0.40, 0.55. Tune each addition independently, then combine the chosen policies unchanged. Rank on safety, macro task fact F1, required coverage, and smaller packets, with fixed grid-order ties. Report all cells, including regressions and ties.

The improved graph projection excludes structural plumbing predicates `tags`, `contains`, `mentioned_in`, and `mentioned-in` at index-build time. This exclusion is predeclared, inspired by existing prompt-fact policy, and never derived from labels. Other explicit relation/fact predicates remain available. Record excluded counts and build cost. Source lexical/dense inputs remain unchanged. Cycles and substantive high-degree hubs must separately exercise edge caps so filtering the inventory does not substitute for a bounded-work test.

Standing cache and native graph API measurements are controls, not claimed replicas of the whole installed daemon. If private prompt-hook selection is replicated, name it `current_policy_replica` and report the implemented deviations. Public formatter output is used, but the common extractive adapter rendering claims as `is_fact` is explicitly experimental. Native controls retain their native predicates.

The resident helper may maintain distinct finite scope/time/scenario projections by an additive `projection` request key. All construction and hydration happen outside timed queries and are reported. Historical eligibility belongs to the adapter; it is not a capability inferred from the active-only native graph. These precomputed projections support this fixed workload, not arbitrary-clock production caching.

## Embeddings and timing

Use the cached fp32 Qdrant all-MiniLM-L6-v2 model, pinned by artifact hashes in the interface, with CPU ONNX Runtime, one thread, attention-mask mean pooling, and unit normalization. Query embeddings are recomputed for timed requests; corpus vectors remain resident. Report source truncation at 256 model tokens. Model startup, corpus encoding, and index construction are separate from warm per-prompt costs. No model download or hosted inference occurs during evaluation.

This follows the model's documented pooling and normalization procedure ([model card](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2)); the embedding model's token limit and prompt-budget tokenizer are different. One model on one authored workload cannot establish the value of all embedding models.

Measure stage and end-to-end p50/p95 with five repetitions after one warmup. Keep fixed query order and rotate treatment order. Timings include entity resolution, eligibility, fusion, formatting, token counting, and IPC where used. Record build profile; debug helper timings are experimental measurements, not release-daemon latency. Deterministic output checks exclude timing and allow documented floating-point tolerance for vectors.

Scaling probes use a fixed small seed graph plus 100, 1,000, and 10,000 unrelated noise edges, with actual node/edge counts reported. These are mechanism and cost probes, not new quality evidence. Report native exact lookup, native adjacency, bounded projected lookup, and lexical retrieval separately. A fast response that carries no useful evidence does not validate prompt enrichment.

## Completion evidence and limits

Report per-budget and per-category task precision/F1/required coverage, all-required success, standing-fact coverage, unsupported task-empty rate, stale/scope violations, tokens, useful facts per 100 tokens, and lookup/packet costs. Retain emitted packet text and source-span evidence, not only selected IDs. Verify source digests, full-claim inclusion, scope/time eligibility, and packet budgets independently of ranking. Retain policy, source, model, input and result hashes.

Focused tests cover maintenance revisions/deletion/resume/no-op, mixed missing derived records, source preservation, deterministic insertion-order behavior, alias ambiguity, direction/cycles/hubs, exact spans, oversized claim omission, missing model failure, and real finite normalized embeddings. Production adoption still requires an adapter, real extraction-quality evaluation, backward-compatible API/old-client checks, release-build workload measurements, and installed verification. Existing indexes must remain usable; dream maintenance incrementally upgrades derived data and an optional full backfill only accelerates coverage.
