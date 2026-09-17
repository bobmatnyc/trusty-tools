# Memory query-plan experiment protocol

Status: premeasurement protocol, 2026-09-17. Follow-up to the immutable memory relevance experiment; tracked by trusty-tools #8246.

## Question and controls

Can deterministic interpretation improve memory prompt relevance without losing supported evidence? Compare three arms: unchanged prior combined policy; a bundled defect repair; and structured directed relation plans. Embeddings remain off. All arms use the same eligible synthetic evidence, native claim BM25 helper, scopes, clocks, maintenance implementation, token budgets and exact-claim formatter. The repaired and structured arms share a BM25 plus plan-directed candidate pool, while the old combined control keeps its prior retrieval. Therefore control-to-repair is a bundled retrieval/selection comparison; repair-to-structured isolates flat versus bound selection on matched candidates. Graph execution differences must be reported as part of each treatment.

Keep the prior policy at overlap1 and four task facts. Repaired and structured arms count four exact semantic facts after deduplication. Budgets are 128 and 256 tokens including standing context. Conflicts, distinct validity intervals and scopes cannot be deduplicated together. Retain provenance and complete support paths.

## Freeze and independence

Old experimental directories and results remain unchanged. Old exposed failures are regression cases only. An independent author creates fresh sources and gold after the interface is fixed, without implementation access. Include materially new binding structures and unsupported positive language, not merely renamed old templates. Report structural overlap honestly. Parser and executor receive only prompt, scoped eligible catalog and clocks; no IDs, split/category labels, gold or expected plan annotations.

Freeze fixture, interface, protocol and policy hashes before official ranking. No parameter grid is planned. Development tests use separate fixtures. Any later policy repair requires preserving the old run and identifying exposed validation. Required evidence groups are an independent completeness oracle; parser-produced demand counts cannot establish recall.

## Measurement

Measure candidate, selected and final required-group coverage; precision; negative abstention; all-required success; semantic duplicates; tokens; stale, forbidden and cross-scope output. Break failures down into unsupported parsing, unresolved entities, incomplete bindings, missing candidates, selection caps and packet budget loss. Record plan and execution statuses outside the evidence packet. Use one warmup and three measured repetitions per case; verify identical outputs, report p50/p95 and build time separately. Timing uses the existing debug helper and is experimental, not a production latency claim.

The unchanged adoption target is at least 90% positive coverage and 90% negative abstention together, with zero stale or cross-scope claims. Report both budgets separately as well as aggregates. Reuse source revision/tombstone/TTL eligibility and deterministic maintenance checks. Never update source verification clocks while rebuilding derived data.

## Scope and release

This is a sibling Python experiment using an existing Rust helper, not a production daemon rollout. No API/schema migration or required full reindex. Existing production behavior is untouched. A production integration still needs backward-compatible defaults, incremental derived-index backfill, real-corpus evaluation and installed-runtime verification. Save evidence and update #8246; keep it open.
