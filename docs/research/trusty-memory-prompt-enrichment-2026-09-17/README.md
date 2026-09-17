# Graph lookup for prompt enrichment

This experiment tests whether a resident graph can contribute useful task context quickly under a fixed prompt budget. It compares contextual BM25 plus temporal eligibility with graph expansion, local embeddings, and both. It separately measures standing prompt-cache access and the current graph selection policy.

The [source review](research.md) found that the prompt hook selects a recent page of graph triples and applies lexical filtering, while the repository already has a separate resident adjacency API. These mechanisms need separate measurement. A fast adjacency lookup does not prove that the resulting prompt contains the right facts.

The [interface](interface.md), [frozen protocol](protocol.md), and [pre-run clarifications](pre-run-clarifications.md) define the comparison. The [fixture audit](fixture-review.md) records label checks and the corrected inventory timestamps before tuning. The [experiment directory](../../../experiments/trusty-memory-prompt-enrichment/README.md) contains runnable code, wholly synthetic fixtures, and retained results.

All retrieval treatments receive the same explicit assertions and source prose. The graph is a curated capability test; this does not demonstrate automatic extraction quality on real memories. Complete evidence claims are scored inside the emitted prompt, with source-revision provenance checked separately. Standing preferences and task-specific facts have separate metrics.

The cached fp32 MiniLM encoder runs locally. No generative model, hosted service, or live memory store participates in evaluation. Production code and installed behavior remain unchanged. Integration, backward-compatible fallback, optional backfill, and installed verification remain tracked in [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).

Read the [completed comparison](report.md), [verification and limitations](verification.md), and [retained run](../../../experiments/trusty-memory-prompt-enrichment/results/run-01/summary.json).
