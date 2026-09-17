# Relevance experiment protocol

## Question and scope

Separate query selection, claim indexing, graph relation selection and consolidation effects on useful memory prompt content. Build on the completed graph-enrichment experiment without changing its source, fixtures or results. Use deterministic local methods and the existing resident Rust BM25/formatter helper. Embeddings are excluded from this comparison so they cannot confound attribution. No answer model, live store, daemon installation or production API changes.

## Interventions

Compare the previous BM25-plus-bounded-graph policy on fresh data against four single interventions: post-retrieval claim selection with abstention; relation-aware graph traversal; claim-level BM25 replacing source-level BM25; and exact duplicate consolidation before packing. Add a combined treatment. All share the same eligible source assertions, standing prelude, complete-claim formatting and token budgets. Each single intervention must leave the other stages unchanged. Do not call a combined result the effect of one intervention.

Consolidation must preserve source evidence and temporal semantics. Only exact equivalent eligible assertions may collapse, with deterministic representative and complete provenance retained. Different values, scopes or validity states must never merge. Duplicate reduction can improve token efficiency; it does not create missing task relevance. Separately test bounded derived-record backfill, revision replacement, deletion, no-op and resume behavior. Native scope/time projections may still require construction outside query timing; report that limitation and do not claim production incremental publication.

## Frozen fixture

The independently authored fixture contains 86 initial sources, 180 exact-span assertions, eight revision/deletion events and 96 queries. Each split has 48 queries: 28 positive and 20 task-empty. Tuning has 40 required groups; heldout has 44, including branched dependency comparisons absent from tuning. See [fixture notes](fixture-notes.md) for composition, temporal cases and limits.

## Evaluation discipline

An independent fixture author creates new sources, task structures, queries and gold before any ranking. Tuning and heldout sets must differ in both wording and some task compositions. Include supported and unsupported requests, unfamiliar phrasing, unknown intents, overlapping entity names, ambiguous aliases, multiple relations, multi-hop support, temporal invalidity, updates/deletions, duplicates and mixed-fact long sources. No query may carry an expected predicate, target entity hint or expected answer into the retriever. Schema/category labels and gold are evaluation-only inputs.

Hash inputs, interface, protocol and query policy before ranking. Tune only on tuning gold. Write selected policy before parsing heldout gold. Do not alter labels, mappings or thresholds after inspecting heldout outputs. Preserve any failed or superseded run. Existing experiment results are development evidence, not validation data for this experiment.

Use 128 and 256 cl100k token ceilings and one warmup plus three measured repetitions. Record query latency separately from startup, index construction, maintenance and scoring. Alternate forward/reverse treatment order by budget to reduce fixed-order drift. Frozen finite eligibility projections remain outside timed queries and must be disclosed.

## Metrics and choice

Report positive-query precision, required-group coverage, F1 and all-required success; negative-query task-empty rate and unsupported task tokens; all-query standing coverage; stale/forbidden/scope violations; tokens; redundant assertions; unique useful facts per 100 prompt tokens; p50/p95 latency and stage costs. Keep unique query counts distinct from repeated query-budget cases and timing samples.

Equivalent duplicates must not earn repeated usefulness credit. For precision, count unique acceptable semantic assertions in the numerator against all emitted task assertions in the denominator; required alternatives count once. Standalone duplicate-token and redundancy metrics make cleanup effects visible. Standing prelude remains outside task relevance metrics but inside token costs. Independently validate actual packet text and source revision/spans before scoring.

Choose selector settings on tuning only: minimize safety violations first, then maximize the mean of positive macro F1 and negative abstention, then required coverage, then fewer tokens, with fixed grid-order ties. Report recall loss even if the combined objective improves. If no tested policy meets both 90% positive required coverage and 90% negative abstention, explicitly report that none meets the provisional adoption target. These are experiment targets, not evidence of general production readiness.

## Compatibility and delivery

Missing derived records must retain a source fallback; partial backfill cannot make stored sources inaccessible. Backfill must preserve observation/verification/validity timestamps and original source bytes. Batch limits bound source count, not necessarily total CPU or native rebuild work. Leave production integration and installed verification on #8246. Save all code, frozen inputs, emitted packets, source/model-free provenance, reviews and results in the experiment branch; update the existing ticket without closing it.
