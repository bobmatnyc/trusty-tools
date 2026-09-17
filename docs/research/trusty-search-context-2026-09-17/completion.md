# Completion contract: backward-compatible contextual search

Production completion: [issue #8245](https://github.com/bobmatnyc/trusty-tools/issues/8245).

The experiment is complete; production integration remains a separate completion task. Its prototype and evidence are preserved on `codex/search-context-experiment`, with the [report](report.md), [protocol](protocol/evaluation.md), and [runnable experiment](../../../experiments/trusty-search-context/README.md). The existing production search behavior remains the default on this branch.

## Required compatibility behavior

- Open and search existing persisted indexes without a schema/version bump, destructive migration, forced full scan, vector rebuild, or mandatory reindex. Existing callers, query parameters, result fields, default ranking path, and vector-enabled behavior must remain supported.
- Use the existing optional `RawChunk.virtual_terms` field for bounded source context. Missing or unenriched values remain valid. An old index and a mixed index containing enriched new/changed files must both serve successfully after restart.
- Introduce context incrementally during normal file indexing. Do not use a setting change as a reason to clear the corpus or schedule a full walk. An explicit optional reindex backfills context for unchanged files and provides full coverage; partial coverage is valid and must not prevent readiness.
- Make query routing and grouped TOC output additive/opt-in during integration. They operate on existing chunks without re-chunking. Preserve the old response shape by default, exact source locators, index identity, scoping, filtering, and useful baseline results when graph resolution is ambiguous or empty.

## Implementation remaining

1. Integrate the narrow definition and relationship routes into supported search interfaces with explicit intent/ambiguity handling, caller filters, and a useful lexical fallback. The Python adapters are evaluated prototypes, not production API integration.
2. Replace the experiment's numeric-ID cursor predecessor trick with a supported exact-ID/batched chunk lookup. Keep graph diagnostics out of normal agent responses and expose the lean TOC as an additional presentation mode.
3. Define durable context configuration/coverage behavior and incremental rollout. Default compatibility must be deliberate; do not infer that changing a global experiment environment variable migrates old data. Preserve 100-line oversized windows unless broader evidence supports a change.
4. Add upgrade, mixed-corpus, restart, incremental-update, old-client contract, filter, ambiguity, and no-vector regression coverage; validate on a larger set of real user queries before enabling by default.

## Acceptance evidence

An old-format, unenriched fixture must load and answer queries with no full-index operation. Indexing one new or changed file must enrich only that file, preserve unrelated old chunks/IDs, and survive restart. Both old and new output modes must work on that same mixed fixture; vector-enabled behavior must still work, and the no-vector path must invoke no embedding method.

A deliberate full reindex may improve coverage and retrieval, but its absence is never an error or a prerequisite to use the upgrade. The 16-question held-out experiment showed top-five source-location hits rising from 1 to 8 with the combined prototype, alongside higher overall latency and memory; this is not a general quality or latency guarantee.
