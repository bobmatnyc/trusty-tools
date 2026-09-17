## Verdict: APPROVE

No issues found at >80% confidence. APPROVED for next pipeline stage.

## Resolved findings

- `experiments/trusty-memory-passages/passage_policy.py:351–359` now computes eligibility from the complete original corpus before narrowing reconstruction. It rejects superseded/forged seeds and partially ineligible referenced sources. This satisfies the independent-authority requirement in `interface.md:146` and supersession coverage in `interface.md:173` and `research.md:61`.
- `passage_policy.py:108–109` excludes deleted or factless sources before consulting fact metadata. A parsed empty tombstone now passes both lexicon and passage derivation. The new tests at `test_passage.py:201`, `:216`, and `:227` cover global supersession, valid empty tombstones, and partial source eligibility.

## Verification performed

Re-ran the original independent probe unchanged:

`/Users/masa/trusty-search-experiment/venv/bin/python /tmp/passage-critic-probe.py`

```text
superseded_seed_eligible= False
superseded_packet_validation= IntegrityError ineligible or forged seed fact
tombstone_parse=ACCEPTED
valid_tombstone=ACCEPTED
```

The probe exits 0. Before the fix, it accepted the superseded packet and rejected the tombstone; both outcomes now meet the contract.

From the worktree root, `/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-passages/test_passage.py experiments/trusty-memory-real-validation/test_real_validation.py > /tmp/passage-critic-tests-final.txt 2>&1; echo EXIT=$?` returned `EXIT=0`.

From `experiments/trusty-memory-passages`, `MYPYPATH=../trusty-memory-real-validation:../trusty-memory-query-plan:../trusty-memory-relevance:../trusty-memory-prompt-enrichment /Users/masa/trusty-search-experiment/venv/bin/python -m mypy --strict passage_policy.py passage_evaluate.py > /tmp/passage-critic-mypy-final.txt 2>&1; echo EXIT=$?` returned `EXIT=0`.

Reviewed source SHA256:

```text
5da8c643dd61de4317f7a10b0a845635576c35c7905dfaed6e0da730c753bc44  passage_policy.py
30cc941090ce2183fb4a2d00c9ee9749753e9821fb15d430a3b33731e909178b  passage_evaluate.py
f2a5fcd1e646ecd82620b91a0953432ec93c775e8432120a0232056abe519f44  test_passage.py
```

## Scope and handoff

This re-review covers the prior findings and changed policy/tests against the same frozen specifications. The unchanged evaluator and remainder of the policy retain the previous source review. Synthetic CLI/native-helper coverage ran through the suite. No private inputs, gold, official rankings, Rust builds, Git mutations, or live changes were performed. This approval establishes review readiness for the bounded experiment, not retrieval quality or production readiness.

Parent continues to the remaining security/input-validation gates before any official measurement.
