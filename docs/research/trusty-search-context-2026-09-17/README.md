# Trusty Search: chunking, BM25, and KG research

Production completion: [issue #8245](https://github.com/bobmatnyc/trusty-tools/issues/8245).

Start with the [results and recommendation](report.md). The [completion contract](completion.md) separates the completed experiment from production integration and makes backward compatibility and optional reindexing mandatory.

- [Experiment tools, dataset, and compressed evidence](../../../experiments/trusty-search-context/README.md)
- [Initial research and external references](protocol/research.md)
- [Frozen evaluation protocol](protocol/evaluation.md) and [tuning amendments](protocol/amendment-01.md)
- [Dataset rationale](dataset-rationale.md) and [interface specification](interface.md)
- [KG cost diagnostic](kg-diagnostic.md), [report audit](report-audit.md), [code review](critic.md), [adapter review](adapter-critic.md), and [security review](security.md)

Paths such as `runs/`, `replays/`, and `results/` in historical reports refer to the experiment root, reconstructed by unpacking its compressed evidence files. Absolute host paths in preserved evidence are provenance from the original run. Build products and indexes are regenerated locally, not stored in Git.

The original protocol requires fresh indexes between treatments to keep comparisons fair. That experiment rule is not an upgrade requirement: the completion contract requires existing and mixed indexes to work without a full reindex.

Preservation checks: [compatibility regression](compatibility.md), [packaging validation](packaging-validation.md), [packaging review](packaging-review.md), and [packaging security review](packaging-security.md).
