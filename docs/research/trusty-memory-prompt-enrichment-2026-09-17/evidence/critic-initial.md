## Verdict: WARN

Three HIGH findings need correction before claiming the experiment verifies packet integrity and gradual maintenance. No CRITICAL security, live-data-loss, or production-crash finding was established. Review scope is the isolated experiment, not production deployment.

## Findings

All paths below are relative to `/Users/masa/trusty-search-experiment/worktree`.

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| HIGH | experiments/trusty-memory-prompt-enrichment/projection.py | 109 | `np.stack([index.vectors[k] for k in ids])` requires a vector for every eligible source. A valid source-only snapshot fails with `KeyError`, so even BM25 cannot open the mixed state promised by interface.md:269–271. `semantic_hash` at line 22 has the same assumption. | Keep lexical document IDs independent of available dense IDs; build the dense matrix only from present, validated derived records. Hash absent derived state explicitly. Add a mixed present/missing-vector test that retrieves both sources lexically, skips missing dense records, and completes bounded backfill without rewriting source timestamps. | Fix here |
| HIGH | experiments/trusty-memory-prompt-enrichment/projection.py | 67 | `current.sources[source.key] = source` occurs before fallible `encoder.encode(...)`. An encoding error leaves revision 2 paired with the revision-1 vector while sequence remains 0. Retrying the same event then fails as a regressed revision. | Compute and validate the new vector before publishing the source/vector pair and advancing the sequence. Preserve the old pair on failure; add an injected encoder-failure test followed by a successful retry of the same event. | Fix here |
| HIGH | experiments/trusty-memory-prompt-enrichment/scoring.py | 21 | `packet.renderings[e.id] not in packet.text` trusts a rendering supplied by the packer instead of deriving the expected complete claim/native bullet from source evidence. The denominator at line 23 also counts only sidecar IDs. A packet with no claimed fact can score F1=1 when its rendering is changed with it; an additional unsupported assertion is ignored. This does not provide the independent actual-packet check required by interface.md:137–141 and :244–246. | Derive allowed representations from validated source facts and an explicit treatment mode. Verify the complete emitted packet against those representations, including all assertion-bearing source blocks; reject unaccounted text or include its assertions in the denominator. Recount tokens against the supplied budget. Add negative tests for absent/replaced claims, unlisted extra claims, and incorrect token counts. | Fix here |
| MEDIUM | experiments/trusty-memory-prompt-enrichment/evaluate.py | 62 | The standing-cache construction passes `prelude=False`, leaving `prelude_tokens=0` even though every emitted fact is standing context. `scoring.py:42` consequently reports the entire cache packet as unsupported enrichment on negative queries, while simultaneously reporting zero task facts. | Mark the cached standing content's tokens as prelude tokens, or derive unsupported tokens from the task-only content. Add a negative-query standing-cache metric test. | Fix here |

## Required Changes

1. Preserve source-only retrieval when optional derived vectors are missing, and test gradual completion of that state.
2. Make each maintenance event publish atomically after successful encoding, and prove retry after failure.
3. Make evaluation independently validate the actual packet and its token count instead of trusting the packer's evidence/rendering inventory.
4. Correct standing-cache unsupported-token accounting before reporting that control.

## Verification Results

No repository edits, build, full benchmark, Git mutations, or ticket mutations were performed. Only review/probe artifacts were written under `/tmp`.

Independent focused command:

```text
/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-prompt-enrichment/test_experiment.py
........                                                                 [100%]
8 passed in 1.12s
```

Manifest verification:

```text
manifest entries verified: 8
```

Independent probes, using the real pinned local model and resident Rust helper; the encoder exception is injected only for the negative maintenance case:

```text
/Users/masa/trusty-search-experiment/venv/bin/python /tmp/graph-prompt-critic-probes.py
mixed snapshot: KeyError: 'test|a'
failed update: injected unavailable model; source_revision=2; old_vector_retained=True; sequence=0
retry after failure: IntegrityError: maintenance revision regressed
extra emitted assertion: task_facts=1; precision=1.0; fact_f1=1.0
absent full claim with arbitrary rendering: coverage=1.0; fact_f1=1.0
```

Additional real helper/model standing-cache probe, using the runner's `prelude=False` construction and a negative task gold row:

```text
{'task_facts': 0, 'standing_facts': 1, 'empty_task': True, 'tokens': 16, 'prelude_tokens': 0, 'unsupported_tokens': 16}
```

The expected unsupported-token count is zero because the packet contains only accepted standing context. This affects control reporting, not policy selection on the main treatments.

Provided artifacts were read; these were not rerun:

```text
/tmp/graph-prompt-mypy.log: Success: no issues found in 8 source files
/tmp/graph-prompt-rust-tests.log: test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
```

### Status: NEEDS ATTENTION

## Notes

The observed defects above do not establish that already emitted frozen-workload packets have incorrect quality scores. The packet probes demonstrate that the claimed independent verification can falsely pass. Maintenance probes demonstrate states the existing tests do not cover.

Source review found no gold input in indexing, graph seeding, retrieval, fusion, or packing. The runner writes selection successfully before parsing heldout gold. Model/tokenizer hashes are checked locally; no fake embeddings or network fallback are used. Mean pooling and normalization match the frozen procedure. Full claims, common prelude charging, edge scan/output limits, alias ambiguity, directed native graph output, and scope/time projection have focused coverage. Native graph hydration is separate from measured operations. Existing finite-clock, native-format, and Python-process-only RSS limitations are disclosed.

The eight Python tests do not validate source-only mixed snapshots, maintenance error recovery, or corruption of packet text/rendering metadata. No assertion is made about release-daemon latency, production migration compatibility, or full-suite health.

Parent agent continues: resolve the findings within the authorized experiment scope, rerun focused checks, and qualify or regenerate result claims as necessary. Preserve the frozen labels and protocol; none of these corrections requires changing gold.
