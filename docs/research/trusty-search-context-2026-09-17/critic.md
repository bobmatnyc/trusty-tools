## Verdict: APPROVE

Reviewed the current experiment changes against `protocol/research.md`, `interface.md`, and `protocol/evaluation.md`, including the follow-up guards in `warm_embedder` and `embed_text`, the new reindex-stage regression, and evaluator completeness validation. No unresolved CRITICAL or HIGH findings remain in the reviewed source. Approval permits the next verification stage; it does not validate the failed baseline or replace a fresh zero-call measurement.

No unresolved issues found at greater than 80% confidence. APPROVED for next pipeline stage.

## Resolved during review

1. The first baseline violated the zero-call contract through reindex warmup and metadata-context refresh. `core/indexer/ingest/mod.rs:654` now exits warmup for `skip_vector`; `core/indexer/search/lanes.rs:214` returns `None` before embedding arbitrary text. The new `service/reindex/stages_experiment_tests.rs` exercises both actual helpers with a README, a rejecting embedder, and a vector-enabled control. The updated test log records this test passing. The failed baseline remains invalid.
2. `benchmark.py:36` now rejects incomplete or failed indexing even when stage flags say ready. It checks the top-level lifecycle, nonempty corpus, cap drops, truncated walks, walk/corpus/migration failures, deferred promotion, and stored vectors. Negative test evidence is recorded below.
3. `benchmark.py:184–190` now rejects degraded repeat responses and preserves every successful repetition in `repeat_responses`, retaining evidence beyond the initial result and latency sample.

## Verification Results

This was a read-only source review; no compilation, daemon launch, or benchmark requests were performed. Only this report was written.

Observed supplied logs:

- `results/tests.log`: `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 2198 filtered out; finished in 0.17s`
- `results/example-tests.log`: `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- `results/python-tests.log`: `14 passed in 0.09s`
- `results/python-types.log`: `Success: no issues found in 2 source files`
- `results/baseline-run.log`: `ValueError: Embedding method was called`, raised by `verify_fixture` before evaluation. No retrieval results from this run qualify as measurements.

The focused tests verify config parsing, source preservation, window mappings, cards, direct indexing/query guards, warmup/context-refresh guards, and a persisted single-chunk BM25 reload. They do not constitute a full crate test run. The direct no-vector test calls `index_files_batch` and `index_file`; the new regression invokes the real reindex helpers individually, rather than executing the entire reindex runner.

Status: source review approved; fresh runtime verification required before accepting experimental results.

## Notes

- `protocol/evaluation.md` specifies natural-text lexical and graph requests. The evaluator follows it. This differs from the earlier seed-lookup sequence in `interface.md`; the graph implementation does support natural text followed by expansion of fused BM25 candidates. State this protocol precedence in the final experiment report.
- Enrichment appends bounded source-derived words to persisted `virtual_terms` without modifying chunk source or ranges. Bulk and incremental parse sites invoke the same helper. The supplied persistence test demonstrates terms and ranking survive a corpus reopen.
- The runner uses a dedicated registry, local allowlist, marked roots, a loopback port, and a rejecting embedder. Its middleware constrains index creation and mutation to the experiment corpus. No isolation escape was found at greater than 80% confidence in the reviewed paths.
- Restart/reload coverage is a corpus reopen test; the example daemon intentionally refuses an existing registry. Do not describe this as a demonstrated daemon restart experiment. Nonzero-context bulk/incremental equivalence and removal are also not established by the supplied focused tests.
- Parent continues with security review and a fresh baseline. Require zero embedding calls after complete indexing and retrieval before accepting measurement results.
