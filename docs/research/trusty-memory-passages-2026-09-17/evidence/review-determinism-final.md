## Verdict: APPROVE

The prior MEDIUM finding is resolved as a guard against reverting canonical summation. No remaining material finding in this test-only review.

`experiments/trusty-memory-passages/test_passage.py:95–132` now exercises two distinct document frequencies, verifies the exact-quarter selection, and records the actual numerator/denominator arguments passed to `math.fsum` under three hash seeds. The wrapper delegates arithmetic to the real `math.fsum`; it does not substitute a result. Reverting the runtime change now fails the test.

The remaining limitation is precise: this is a test of the chosen summation mechanism plus expected selection behavior. It does not reproduce a user-visible selection difference in the old implementation. The old implementation still selects the candidate in this fixture and fails because it makes no recorded `math.fsum` calls. A future alternative deterministic summation implementation would require updating the mechanism assertion. This does not reopen the resolved finding that the earlier test allowed a direct reversion to pass.

## Verification

Read the provided synthetic logs:

- `/tmp/passage-engineer-determinism-old.txt`: prior policy SHA256 `5da8c643dd61de4317f7a10b0a845635576c35c7905dfaed6e0da730c753bc44`; `1 failed, 15 deselected in 0.79s`. The failure is at the expected argument assertion, with the correct selected ID and an empty recorded-call list.
- `/tmp/passage-engineer-determinism-final.txt`: `1 passed, 15 deselected in 0.77s`.
- `/tmp/passage-engineer-tests-final2.txt`: `25 passed in 2.99s`.

Independently ran:

```sh
/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-passages/test_passage.py::test_exact_quarter_support_across_hash_seeds > /tmp/passage-critic-determinism-final-test.txt 2>&1; echo EXIT=$?
```

Observed: `EXIT=0`.

SHA256 confirms runtime source remains unchanged from the approved version:

```text
cdd884f24cb377f837731fcb4aeb909ab3f7512c3ad08b2b0ef97c51b8b598e4  passage_policy.py
30cc941090ce2183fb4a2d00c9ee9749753e9821fb15d430a3b33731e909178b  passage_evaluate.py
de85ec8ad4355b1cd0f2d8d236ced0be9da708c4b70792d963ebd4aadefbf54f  test_passage.py
```

No private inputs or rankings were read; no runtime source edits or Rust builds occurred. Parent continues with the approved experiment and records this test-only resolution separately.
