## Verdict: APPROVE

The weighted-sum change preserves the frozen policy and removes dependence on set iteration order. Zero CRITICAL or HIGH findings. The test coverage finding below remains.

## Findings

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| MEDIUM | experiments/trusty-memory-passages/test_passage.py | 89–113 | The new hash-seed fixture uses eight terms with identical document frequency and weight. The previous unsorted `sum` implementation also selects the expected candidate under all three tested seeds, so the regression does not detect reverting the arithmetic fix. | Use invented sources with heterogeneous document frequencies and a near/exact-quarter boundary that fails the previous arithmetic under at least one chosen seed. Assert the required selection across seeds and demonstrate failure with the previous implementation. Retain this exact-quarter fixture as additional coverage if useful. | Fix here |

## Verification

`passage_policy.py:141–155` now sorts query terms and matched terms, and uses `math.fsum` for both sums. Candidate iteration remains native order. The informative-term count, natural-log weights, two-term minimum and `>= .25` comparison remain consistent with `interface.md:31,35`. No epsilon or threshold relaxation was added.

Focused command:

```sh
/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-passages/test_passage.py::test_exact_quarter_support_across_hash_seeds > /tmp/passage-critic-determinism-test.txt 2>&1; echo EXIT=$?
```

Observed: `EXIT=0`.

Independent probe reconstructed the prior function in a fresh in-memory namespace by replacing sorted query iteration with `_words(task.prompt)`, numerator `math.fsum` with `sum(weights[w] for w in matched)`, and denominator `math.fsum` with `sum(weights.values())`. It used the exact new fixture and asserted its expected selected evidence in separate Python processes. No source files or imported module globals were modified. Raw output, exit 0:

```text
PYTHONHASHSEED=1: prior implementation passes added fixture
PYTHONHASHSEED=7: prior implementation passes added fixture
PYTHONHASHSEED=99: prior implementation passes added fixture
```

Reviewed SHA256: `cdd884f24cb377f837731fcb4aeb909ab3f7512c3ad08b2b0ef97c51b8b598e4` (`passage_policy.py`); `4554805e6717b9681346c6cbe78f2b043b6d4478915687bce2ca5975357c62bf` (`test_passage.py`).

This review is limited to deterministic summation and its new regression. Prior findings remain resolved. No private inputs, rankings, Rust builds or source edits occurred. Parent continues with the regression improvement and remaining authorized gates.
