## Verdict: APPROVE

No unresolved issues found at greater than 80% confidence. APPROVED for next pipeline stage.

Reviewed `routing.py`, `replay.py`, `presentation.py`, their tests, `kg_adapter.py`, and `protocol/amendment-01.md`. This is source approval for the evaluation adapters. Parent owns the remaining real HTTP and zero-embedding verification.

## Resolved findings

- `replay.py:91`: directed retrieval now intercepts only graph-stage requests. The lexical comparison remains normalized lexical retrieval.
- `replay.py:32`: a corpus-scoped exclusive lock and conservative source-PID check reject concurrent reuse. Successful-source evidence, prior completeness validation, binary identity, and restored chunk/graph counts are checked before scoring.
- `kg_adapter.py:54`: degraded or stale seed searches raise before graph traversal, rather than becoming valid empty or partial outcomes.
- `kg_adapter.py:105`: results preserve the materialized chunk envelope, require `file`, and supply the existing seven-line compact snippet. This addresses the Rust card endpoint's required field and avoids comparing snippet-free compact payloads with normal compact results.
- `protocol/amendment-01.md`: directed query patterns, seed/neighbor limits, ordering, fallback, trace preservation, and payload-versus-intermediate-traffic reporting are now specified before held-out evaluation.

## Evidence and limits

The reviewer inspected the source and test assertions without starting a daemon, indexing, compiling, or reading held-out queries. No experiment source files were edited; only this review report was written.

Parent reports 42 passing Python tests and strict mypy success across seven files. At inspection, the existing `results/python-tests.log` still contained `14 passed in 0.09s`, and `results/python-types.log` contained `Success: no issues found in 2 source files`. Those older logs do not independently establish the reported expanded test run.

The adapter tests use synthetic transport responses. They exercise query-only direction selection, exact seeds, pagination, materialization, bounded neighbors, metadata preservation, compact snippets, and degraded-response rejection. They do not prove the real HTTP integration works. Parent must verify a nonempty directed response survives the Rust card endpoint and completes with zero embedding calls before accepting measurements.

No label leakage was found in routing, directed retrieval, or presentation construction. Routing uses query text; directed retrieval uses retrieved symbol metadata and graph edges; presentation uses saved source and retains original ranks and source locators. Token reductions describe output payloads with differing information density, not equivalent answer quality or total retrieval traffic.

Parent continues with runtime verification, then freezes or rejects the directed adapter on tuning data before held-out evaluation.
