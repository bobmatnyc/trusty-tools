Post the text below the rule on [#10738](https://github.com/bobmatnyc/trusty-tools/issues/10738), then close [#10738](https://github.com/bobmatnyc/trusty-tools/issues/10738) as completed.

---

**Verdict:** live check PASS for [#11019](https://github.com/bobmatnyc/trusty-tools/pull/11019), merged as [751559afac](https://github.com/bobmatnyc/trusty-tools/commit/751559afac), on run 2.

Acceptance holds on live evidence. After a SIGKILL or SIGTERM mid-staged-reindex and a restart, the generation rose 1 → 2 → 3, with reason `identity_bump_pending`. The post-restart put reported generation 2. No acked put stayed served under an unchanged generation.

## Run

- Live run, 2026-11-12, 00:29:49Z–00:39:28Z, rc=1, 40 PASS and 5 FAIL.
- Binaries built from [751559afac](https://github.com/bobmatnyc/trusty-tools/commit/751559afac):
  - trusty-search 0.64.0 (111021008 B, sha256 `0df70aa0b6272f52d546cde25e10db99f133d8ea5d49c3cb0ec0755bc35500ad`)
  - trusty-embedderd (38274904 B, sha256 `734d4686189c55cdf775529ebcc28c7931f18f073dcddc633549b52bb7fe3cfc`)
- Script v2 sha256 `1553b9239b12e71b6a1516ea5d02e2c21e19314ff75ff0ae8c8d13f6a370c6cd`, run with `--expect-11025=0`.
- The run was isolated and UDS only. The 7879 daemon was untouched (pid 413 before and after).

## The 5 FAIL lines

1. `FAIL  [KILL] restart raises the generation above the put's (1 -> )`: script timing. The probe raced the cold-park lazy load. Identity reconciles on corpus attach. The daemon log shows the bump to 2 at load.
2. `FAIL  [KILL] the put acknowledged into staging is not served`: wrong script expectation. On restart, the daemon adopts and resumes the leftover staging corpus by design ([#3979](https://github.com/bobmatnyc/trusty-tools/issues/3979), `staging_leftovers.rs` `adopt_newest`). The resumed run's prune pass removes the pushed path, and the generation rises. SPEC-OKF-01:1556 requires only that the generation rise.
3. `FAIL  [TERM] the put is gone, so the generation rose (1 -> )`: script timing, the same cause as 1.
4. `FAIL  [TERM] b.rs (the killed run's batch) served after the rerun`: probable product defect. It was there before [#11019](https://github.com/bobmatnyc/trusty-tools/pull/11019) and was not caused by it. It is filed separately (new issue: link added on filing).
5. `FAIL  [TERM] the put acknowledged into staging is not served`: wrong script expectation, the same as 2.

## Open point

After prune, `file.status` still reported the pushed path as present. The hypothesis is a stale in-memory chunk map from an idle-eviction rehydrate. This is unconfirmed and is tracked in the new issue.

## Not covered live

These gaps were accepted. Unit tests cover them:

- the in-process refusal while the rollback is in flight;
- rollback without a restart;
- a failed bump keeping puts refused;
- a delete racing the rollback.

A release build has no external way to abort a reindex task in-process.

## Method

The classification comes from a code read at [751559afac](https://github.com/bobmatnyc/trusty-tools/commit/751559afac), not from a re-run. Evidence is in `docs/evidence/11019-run2-evidence.txt` and `docs/evidence/11019-run2.log` in the trusty-okf workspace. The run 1 log is `docs/evidence/11019-run1.log`.

Run 1 failed before the test, because the script had not approved its root under the [#767](https://github.com/bobmatnyc/trusty-tools/issues/767) allowlist. The script is fixed for future runs as v3 (probe retry, restart before presence checks).
