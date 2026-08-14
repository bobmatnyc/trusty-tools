# preflight CHECK 5 decision fixtures (issue #5620)

Captured `scripts/check_semver.sh` output, replayed by
`scripts/preflight-check5-selftest.sh` through a stub gate. They exist so the
self-test can prove `preflight-publish.sh`'s CHECK 5 tells a verified pass from
a blind one without spending four minutes of rustdoc per case — which is why
that decision had no test until the trusty-review 0.16.0 publish shipped
unverified.

These are the GATE's output, not `cargo-semver-checks`' — the other direction of
the same seam. `scripts/test-data/semver-gate/` holds the tool-side fixtures that
`check_semver_selftest.sh` replays into the gate; these hold the gate-side
fixtures that this self-test replays into the decision.

The one edit applied to a captured file: absolute paths rewritten to
`/CARGO_HOME` and `/HOME`, so no author's worktree path is committed.

| File | Stub exit | Captured from |
|---|---|---|
| `inventory-blind.out` | 0 | **The defect.** `bash scripts/check_semver.sh --crate trusty-review` on `main` at `293dfa68`, 2026-08-12. `cargo-semver-checks` exits 101 because 0.15.0 cannot be documented — `pipeline/mapreduce/reduce.rs:28` imports a `profile`-gated item unconditionally — so the gate reports `0 crate(s) checked, 0 skipped, 1 inventory NOT computed` and exits 0. This is the run that printed `[PASS]` on the real publish. |
| `checked-clean.out` | 0 | `--crate tga`, the real 2.18.0 -> 2.19.0 comparison: `196 checks: 196 pass, 58 skip`, `1 crate(s) checked`. The happy path, and what proves the other cases stop on classification rather than because every path now stops. |
| `recorded-skip.out` | 0 | `--crate trusty-mpm`, excluded by `semver-checks-crate-exclusions.tsv`. `0 crate(s) checked, 1 skipped` — nothing compared, for a reason that is a fact about the crate. |
| `no-verdict.out` | 3 | `SEMVER_GATE_INDEX_BASE=http://127.0.0.1:1 --crate trusty-common`. A real unreachable-registry run: the gate refuses to grant a skip it cannot justify. |
| `inventory-clean.out` | 0 | **Composed** from real captures: the gate's own `INVENTORY` lines around a real `196 checks: 196 pass, 58 skip` summary. No crate in this workspace sits at an already-breaking bump with a buildable baseline right now, so the advisory-arm-that-succeeded shape has to be assembled. It pins that an inventory COUNTS as a comparison — the fix must not turn the already-breaking arm into a blanket stop. |
| `break.out` | 1 | **Synthetic**, carrying the gate's real `VERDICT: BREAK` remediation text. The decision keys on exit 1 alone here, so a captured break would add bytes and no coverage. |
| `no-summary.out` | 0 | **Synthetic.** A clean-looking run whose final line does not match the summary shape the decision parses. Pins the fail-closed rule: a reworded summary turns CHECK 5 red, never green. |
