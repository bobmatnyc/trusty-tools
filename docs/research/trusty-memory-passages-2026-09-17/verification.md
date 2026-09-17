# Verification record

This record covers the offline `memory-passages-v1` experiment for [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246). No production code, live memory, installed daemon or embedding behavior changed.

## Before measurement

The independent judge and parent froze the sample and mandatory original-source spans before rankings. The [protocol](protocol.md) records sample selection, hashes, label adjudication and temporal limits. The existing snapshot and all 391 prior experiment artifacts remained unchanged. The runtime policy was frozen in [this manifest](evidence/frozen-policy.json).

The initial code review found that the independent packet validator narrowed the source set before checking supersession, and rejected a valid empty tombstone. Both were repaired and independently reproduced as resolved. Preserve the [initial findings](evidence/review-initial.md) and [final approval](evidence/review-final.md) together. No official rankings ran while the runtime was blocked.

A parent check then replaced unordered floating-point accumulation with sorted terms and `math.fsum`, retaining the 25% threshold. The [narrow review](evidence/review-determinism.md) approved the runtime code and identified a weakness in its initial equal-weight regression. A test-only strengthening continued during measurement; measured runtime modules stayed frozen.

The full focused suite passed 25 tests; strict mypy passed both new source modules. Raw output is retained in [tests.txt](evidence/tests.txt) and [mypy.txt](evidence/mypy.txt). The [security review](evidence/security.md) passed with zero HIGH/CRITICAL findings. Its guarded run passed 24 tests with the subprocess hash-seed test explicitly excluded and separately covered by the full suite. No forbidden Python network, model or private-input access was attempted under those guards. Native child syscalls were not sandboxed.

## Measurement and independent audit

The operator supplied a private mode-0700 TMPDIR, pinned offline tokenizer cache and umask 077. All raw prompts, notes, labels, packets and execution logs remain outside Git. Query timings exclude corpus setup and source/packet validation. Each case uses one warmup and three measured repetitions and rejects changed content signatures.

Final run evidence and independent aggregate audit are linked from the report. This verifies an offline experiment, not an installed-daemon rollout or historical replay.

The strengthened synthetic test rejects an exact reconstruction of the prior arithmetic and passes the current policy. This tests canonical summation calls plus selection behavior, not a reproduced user-visible threshold flip. See [prior red](evidence/determinism-prior-red.txt), [current green](evidence/determinism-final-green.txt) and [final review](evidence/review-determinism-final.md). The original pre-run manifest intentionally retains the earlier test hash; [final artifacts](evidence/final-artifacts.json) records the test-only update. Runtime hashes are identical in both manifests.

Official run-01 exited 0. All 23 loaded experiment module hashes match their current files, and all 391 pinned old artifacts remain unchanged after execution. Input hashes match the frozen labels/corpus. Independent audit passed for 192 packets, 8,396 spans and 3,072 support metric values; [its report](evidence/results-independent.md) records checks and limitations. Repetition equality is enforced by the runner and was not separately replayed by the auditor.

Private temporary storage has no leftover native database/index directory. It retains hook-owned `.ses` material and debug logs; content-bearing files were verified/set to mode 0600 and kept outside Git. Only aggregate outputs and generic review/test evidence are published here.

The [final security addendum](evidence/security-final.md) passed for the test-only change and complete public artifact set, with no sensitive-data or credential findings. The parent exact-string scan also found zero full private prompts or mandatory gold quotes of at least 30 characters in the public scope. These scans complement, rather than replace, the private-output boundary.

Preservation checks found two trailing-whitespace warnings: one in the synthetic test fixture and one in the retained raw failure log. Both are accepted as nonfunctional for this experimental snapshot; exact reviewed source and raw evidence bytes were preserved. The whole-branch changelog check passed.

The [full-branch credential scan](evidence/security-branch.md) passed after triage: 437 changed paths, 161 decoded gzip artifacts and the 27 new passage files. Six detected strings were tokenizer/cache hashes, not credentials. The final additions are this aggregate record and the scanner report itself.
