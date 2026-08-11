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
no author's worktree path is committed. The two `*-colored.out` files were taken
from a GitHub Actions log rather than a shell, so they carry two more mechanical
rewrites: the runner's `CARGO_HOME` becomes `/CARGO_HOME`, and each line's
leading `2026-08-11T17:55:29.5352927Z ` timestamp — added by the Actions log
service, never printed by the tool — is removed. Their ANSI escapes are
untouched, because those escapes are the whole point of the fixture.

| File | Stub exit | Captured from |
|---|---|---|
| `break.out` | 100 | `cargo semver-checks -p trusty-mpm --baseline-version 1.3.4 --only-explicit-features` + the 11 default-set features, under rustc 1.97.1. The real 1.3.4 -> 1.3.5 break: 9 major failures. |
| `clean.out` | 0 | `cargo semver-checks -p trusty-progress --baseline-version 0.2.0 --only-explicit-features --release-type minor`. A real all-pass verdict: `196 checks: 196 pass, 58 skip`. |
| `all-skipped.out` | 0 | The same command WITHOUT `--release-type`, so the tool infers "major change" from 0.2.0 -> 0.3.0 and skips its entire lint set: `0 checks: 0 pass, 254 skip` + `Summary no semver update required`, at exit 0. This file used to be named `clean.out` and drove the gate's "clean" case — which is the #5440 defect exactly: zero work done was the fixture for a pass. |
| `build-error.out` | 101 | The same `trusty-mpm` command under the repo MSRV 1.94.1, where the scratch resolution takes `takecell` 0.1.2 (`rust-version` 1.96) and rustdoc refuses to build. This is the defect #5289 was filed for. |
| `silent-noop.out` | 0 | **Synthetic.** No real invocation produces exit 0 with no `checks:` summary today. It pins the fail-closed rule: "the tool said nothing" must never be read as "the tool said pass". |
| `clean-colored.out` | 0 | The `tga` 2.16.0 -> 2.17.0 run of PR #5458, [job 93874563097](https://github.com/bobmatnyc/trusty-tools/actions/runs/31520044458/job/93874563097). 196 pass, no break — and the gate announced it as "exited 0 without completing a check run". |
| `break-colored.out` | 100 | The `trusty-review` 0.14.1 -> 0.15.0 run of the same job: 4 real major failures, announced as "exited 100 without completing a run". |
| `private-mode.out` | 100 | **Synthetic.** A break-shaped run carrying private-mode CSI sequences — `ESC[?25l` / `ESC[?25h` (cursor hide/show, what a spinner renderer emits) plus `ESC[2K\r`. The hide sits between `Checked` and its space, where an SGR-only strip leaves it and the marker check goes blind again. No cargo-semver-checks 0.50.0 output carries these; it pins the strip to the ECMA-48 CSI grammar rather than to the shape that happened to be observed. |

Both `*-colored.out` files exist (issue #5500) because every fixture above was captured by
redirecting to a file on a workstation, where cargo-semver-checks emits plain
text. CI is not that environment: `dtolnay/rust-toolchain` exports
`CARGO_TERM_COLOR=always` into `$GITHUB_ENV`, so the summary line arrives as
`ESC[1mESC[32m     CheckedESC[0m [   0.871s] 196 checks: …` and the reset
sequence sits between `Checked` and its trailing space. Refreshing either one
means re-capturing under `CARGO_TERM_COLOR=always`; a plain-text capture would
still pass its case while testing nothing.

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
