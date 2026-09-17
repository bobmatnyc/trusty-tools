## Verdict: WARN

Read-only review of the Python evaluation harness against the research, evaluation, and interface specifications. Rust implementation is excluded. Paths below are relative to `/Users/masa/trusty-search-experiment/worktree`.

## Findings

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| HIGH | experiments/trusty-memory-deterministic/metrics.py | 59 | `len(r['scope_errors']) + len(r['forbidden_hits'])` counts the same returned hit twice when it is both outside the query scope and explicitly forbidden. A single forbidden hit produced `scope_errors=2`. This distorts the first parameter-selection criterion and the reported safety count. | Count the union of violations per returned hit; report violating-query count separately. Add a test covering overlapping and disjoint error sets. | Fix here |
| HIGH | experiments/trusty-memory-deterministic/evaluate.py | 97 | `sources = ... for x in state['sources']` trusts the engine's returned source store as the evidence oracle. `drain` replaces the input state at line 59 without comparing sources to the frozen fixture and applied events. If maintenance changes or loses a source, subsequent locator checks validate against the same corrupted response. The required no-source-loss check is absent. | Replay fixture mutations into an independent expected source/revision/tombstone map. Compare every completed maintenance response to that map and validate hit provenance against it. Test an altered source and a dropped source that no query retrieves. | Fix here |
| MEDIUM | experiments/trusty-memory-deterministic/evaluate.py | 76 | `source = sources.get(...)` followed by body/range checks never verifies `revision`, `line_start`, or `line_end`. A one-line source with hit revision 999 and lines 99–100 passed `validate_hits`. Duplicate source identities also pass, although the interface prohibits them. | Retain expected revisions in the source oracle; derive line ranges from UTF-8 byte offsets; reject duplicate `(scope,id)` hits. Add negative tests for each. | Fix here |
| MEDIUM | experiments/trusty-memory-deterministic/evaluate.py | 114 | `excerpt=' '.join(h['excerpt'].split()[:24])` measures a different excerpt than the one checked for evidence and stored in responses. For `cedar-manual`, the measured excerpt ends at “counterclockwise exactly” and drops “one quarter turn”, while retaining the original full-excerpt line locator. The README discloses shortening, but the token metric still enters parameter selection. | Measure actual returned-hit tokens separately. If navigation-card tokens remain a selection objective, persist the exact cards, preserve exact excerpt bytes and matching ranges, and explicitly qualify that objective as navigation cost. Test the long-manual boundary. | Fix here |

## Required Changes

1. Correct the overlapping scope/forbidden count and add regression coverage.
2. Compare maintenance output and hit provenance to an independent fixture/event oracle.
3. Strengthen locator validation and keep token measurements tied to the exact measured artifact.

## Verification

- Supplied environment: `/Users/masa/trusty-search-experiment/venv/bin/python`.
- Full Python harness suite: `python -m pytest -q -p no:cacheprovider .../test_evaluate.py .../test_metrics.py` returned exit 0. Output is preserved at `/tmp/memory-eval-critic-tests-venv.log`.
- Strict mypy on evaluate.py and metrics.py returned exit 0. Output is preserved at `/tmp/memory-eval-critic-mypy.log`.
- Frozen fixture, queries, and protocol hashes each verified `OK` with `shasum -a 256 -c`.
- Direct adversarial observations: `single_forbidden_hit_scope_errors 2`; `false_line_and_revision_locator_accepted=True`; `duplicate_identity_accepted=True`; manual full excerpt contains the answer, while its measured card does not.
- The default system Python initially failed collection with `ModuleNotFoundError: No module named 'tiktoken'`; using the supplied environment resolved it. No dependency changes were made.

## Notes

The request allowlist excludes labels. Policy candidates run only against tuning queries, and selection is persisted before held-out treatment runs. Fixture counts and splits match the protocol: 102 sources, 18 events, and 66 queries with 33 per split. Relevance and temporal errors are separate; empty-label cases do not get perfect recall/MRR. The manifest currently matches.

The README correctly limits latency to fresh CLI requests, includes reconstruction/serialization overhead, and qualifies aggregate CPU and process-wide RSS. No Rust execution, retrieval outcome, or engine model-call claim was verified in this review.

The report currently retains per-query rows but does not generate the protocol's category/scenario summaries, forbidden-query rate, or pre-grouping duplicate share. Preserve these as explicit measurement gaps if they are not added. Runtime/Python/tokenizer versions are also absent from provenance; record them before publishing reproducibility claims.

Parent continues with corrections and the separate Rust review. No repository files or Git state were changed.

## Final disposition after corrections: APPROVE

All four findings above are addressed. No remaining HIGH or CRITICAL findings in the bounded Python review. APPROVED for the next pipeline stage; Rust execution and engine review remain separate.

| Original finding | Disposition evidence |
|------------------|----------------------|
| Double-counted scope errors | `metrics.py:59-60` uses the union and separately counts violating queries. `test_metrics.py:47` covers overlapping and disjoint violations. |
| Engine state used as source oracle | `oracle.py:29-53` independently replays frozen mutations and compares sources/revisions/tombstones, including duplicate identities. `evaluate.py:57-64,111,140` checks maintenance outputs and validates hits against that oracle. `test_oracle.py:14,24` covers altered bodies, loss of an unqueried source, and a late update after deletion. |
| Unchecked provenance locators and duplicates | `evaluate.py:78-97` checks unique source identities, source revision, digest, exact byte excerpt, and line ranges. `test_evaluate.py:36` rejects fabricated revisions/lines and duplicate hits. |
| Truncated excerpt token measurement | `evaluate.py:125-133` preserves the full exact hit excerpt in persisted cards and separately measures actual returned-hit tokens. The 24-word transformation is removed. |

The follow-up also adds category/scenario summaries (`evaluate.py:151-152`) and runtime/tokenizer provenance (`evaluate.py:194-195`). Frozen labels and protocol still match all three original hashes.

Verification rerun with `/Users/masa/trusty-search-experiment/venv/bin/python`:

- Entire Python harness suite, all three test modules: `EXIT=0`; output at `/tmp/memory-eval-critic-recheck-tests.log`.
- `mypy --strict` on evaluate.py, metrics.py, oracle.py: `EXIT=0`; output at `/tmp/memory-eval-critic-recheck-mypy.log`.
- `shasum -a 256 -c .../manifest.sha256`: fixture.json `OK`, queries.json `OK`, evaluation.md `OK`.

Pre-grouping duplicate share and checks outside this Python scope still require separate observation or an explicit report gap. No Rust source or engine result was reviewed. Parent proceeds with the separate Rust review and frozen evaluation.
