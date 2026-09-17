# Improving memory prompt relevance

This follow-up separates query selection, graph relation filtering, claim indexing and cleanup after the [previous experiment](../trusty-memory-prompt-enrichment-2026-09-17/report.md) found high required-fact coverage but poor precision and no abstention on unsupported tasks.

The [protocol](protocol.md) specifies independent interventions and a fresh heldout evaluation. The [source review](research.md) identifies reuse points and failure mechanisms. Original experiments and production code remain unchanged. This experiment uses local BM25 and deterministic graph/claim rules without embeddings or generation.

Read the [completed comparison and recommendations](report.md), [verification](verification.md), and [ablation interpretation guide](interpretation.md). Production adoption remains tracked by [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246), including fallback without a mandatory reindex and incremental derived-index maintenance during dream cycles.
