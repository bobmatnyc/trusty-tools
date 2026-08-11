# SemVer-gate fixtures (issue #5289)

Captured `cargo-semver-checks` 0.50.0 output, replayed by
`scripts/check_semver_selftest.sh` cases 5-8 through a stub `cargo`. They exist
so the self-test can prove `scripts/check_semver.sh` tells a build failure apart
from a SemVer verdict without spending minutes building two rustdoc trees per
case.

Each file is the tool's stdout+stderr **only** — never the gate's own output. A
fixture that includes the gate's remediation text would let the "must not claim
a version bump is needed" assertion pass on the fixture's own contents rather
than on what the gate decided. That mistake was made once while writing these
and the self-test caught it.

The one edit applied to a captured file: absolute paths rewritten to `/REPO`, so
no author's worktree path is committed.

| File | Stub exit | Captured from |
|---|---|---|
| `break.out` | 100 | `cargo semver-checks -p trusty-mpm --baseline-version 1.3.4 --only-explicit-features` + the 11 default-set features, under rustc 1.97.1. The real 1.3.4 -> 1.3.5 break: 9 major failures. |
| `clean.out` | 0 | `cargo semver-checks -p trusty-progress --baseline-version 0.2.0 --only-explicit-features`. Already a major bump, so 0 lints apply — the tool's "nothing to compare, and that is fine" shape. |
| `build-error.out` | 101 | The same `trusty-mpm` command under the repo MSRV 1.94.1, where the scratch resolution takes `takecell` 0.1.2 (`rust-version` 1.96) and rustdoc refuses to build. This is the defect #5289 was filed for. |
| `silent-noop.out` | 0 | **Synthetic.** No real invocation produces exit 0 with no `checks:` summary today. It pins the fail-closed rule: "the tool said nothing" must never be read as "the tool said pass". |

## Replayed by baseline (#5296)

Cases 9-12 register fixtures per `--baseline-version`
(`SEMVER_SELFTEST_BY_BASELINE="0.31.0=break.out:100;0.31.1=clean.out:0"`), so the
stub answers according to which release the gate ASKED to compare against. That
is what makes case 9 a regression test: the pre-#5296 gate asks for the crate's
own version and gets `clean.out`, while the fixed gate asks for the release
before it and gets `break.out`. A baseline with no registered fixture exits 111
naming the version it was asked for, so a case can never pass by accident.

## Refreshing one

Run the command in the table, redirect stdout+stderr to the file, and rewrite
your absolute repo path to `/REPO`. Then run
`bash scripts/check_semver_selftest.sh` — a fixture that no longer carries (or
wrongly carries) the `Checked … N checks:` marker will fail the case that
depends on it, which is the point.
