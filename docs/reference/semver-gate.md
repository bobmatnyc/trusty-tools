# Public-API / SemVer gate

`scripts/check_semver.sh` (issue
[#5050](https://github.com/bobmatnyc/trusty-tools/issues/5050)). It runs at
**release time**, not on pull requests —
[#5149](https://github.com/bobmatnyc/trusty-tools/issues/5060) moved it.

## Where it runs

| Caller | When | Blocks a publish? |
|---|---|---|
| `scripts/preflight-publish.sh` CHECK 5 | immediately before `cargo publish` | **Yes** |
| `.github/workflows/semver-checks.yml` | on a `<crate>-v<version>` tag push | No — reports |
| `bash scripts/check_semver.sh --crate <crate>` | on demand, any time | n/a |

`cargo publish` for this workspace is run locally by a human (`local-ops`), so
no CI job can stop an upload. `preflight-publish.sh` is what can: the release
sequence runs it as the last step before `cargo publish`, and a nonzero exit is
the documented absolute stop (`Skill(skill="cargo-publish")`, step 5). CHECK 5
sits there, so a break is caught while the upload can still be prevented — which
matters because a crates.io publish is irreversible except by yank, and a gate
that reported only after the upload would have caught #4088 with nothing left to
do about it.

The tag-push workflow is a second, independent report. In this project's
sequence the tag is pushed *before* `cargo publish` (steps 4 then 6), so a red
run there is still visible in time to call the release off. Same
blocking-vs-independent split as `release.yml`'s `publish-dry-run` job.

## What is not protected, and when

Between releases, nothing checks this. A breaking public-API change can merge to
main on a patch-versioned crate and sit there; the gate catches it at the release
that would ship it. That is the deliberate trade #5149 made: as a `pull_request`
gate it installed the pinned tool and warmed a cold `target/semver-checks` cache
before crate selection had even decided whether the PR was exempt — 20+ minutes
on every PR, including the docs-only ones. A SemVer break only becomes a defect
when something is published, so the cost belongs at the publish.

The consequence to know: the break surfaces at release time, on the release
critic path, not in the PR that introduced it. Fixing it then means bumping the
breaking position or reworking the API with the release already in motion.

## What it prevents

[#4088](https://github.com/bobmatnyc/trusty-tools/issues/4088). `trusty-common`
0.22.5 added a required public field — `DaemonBridgeConfig.no_spawn_hint` — and
published it as a patch bump. `DaemonBridgeConfig` had neither
`#[non_exhaustive]` nor a `Default`, so every cross-crate struct literal that
omitted the new field became an E0063 compile error. A fresh `cargo install` has
no lockfile, re-resolved a `^0.22` floor to 0.22.5, and paired pre-field source
with post-field dependency. `trusty-analyze` 0.7.3 was yanked over it.

A workspace `cargo check` cannot see this. The root `Cargo.toml` path override
always compiles local source against the local dependency, and those always
agree. The break is only visible against the registry, which is what this gate
compares to.

## What runs

For the named crate, the gate resolves the **previous release** — the greatest
non-yanked **stable** crates.io version strictly below the crate's declared
version — and runs `cargo semver-checks` against it. `--crate` accepts either the package name
(`tga`) or the `crates/` directory name (`trusty-git-analytics`), because a
release tag's prefix can be either
([#1128](https://github.com/bobmatnyc/trusty-tools/issues/1128)).

### Why the previous release and not the latest ([#5296](https://github.com/bobmatnyc/trusty-tools/issues/5296))

The baseline used to be crates.io's *latest* release, which is the previous one
only while the crate is still unpublished. The tag-push workflow fires on
`<crate>-v<version>`, takes minutes to install its tools and reach the registry,
and the human `cargo publish` takes seconds — so the version under test was
usually already the latest release by the time the job looked. The crate was
compared against itself and reported no change, every time. Observed on
`trusty-agents-common` 0.5.0, `trusty-progress` 0.3.0 and `tga` 2.12.0 during the
1.3.5 cut, and still reproducible on `main`:

```
$ bash scripts/check_semver.sh --probe trusty-search   # before
probe trusty-search: baseline=0.45.0                   # declared version: 0.45.0
```

"Greatest published below the declared version" is the same version at preflight
time and the right one after the publish, so the verdict no longer depends on
where in the release sequence the gate happens to run.

### Pre-releases are never a baseline

A pre-release is excluded from baseline selection outright — not merely ordered
correctly below the version it shadows. Nothing resolves `^1.0` to `1.0.0-rc1`
unless someone names it exactly, so a break measured against a pre-release is one
no ordinary dependent can experience, while the real break against the stable
release it shadows goes unreported. `release_type` also strips the suffix, so
`1.0.1-beta → 1.0.1` computes as `none` and the gate would demand a bump over a
version nobody uses.

This was live: `key()` stripped the pre-release suffix before parsing, `1.0.0-rc1`
and `1.0.0` both keyed to `(1, 0, 0)`, and the tie went to whichever the index
listed first.

```
history ["0.9.9", "1.0.0-rc1", "1.0.0", "1.0.1-beta"], declared 1.0.1
  before:  baseline=1.0.0-rc1
  after:   baseline=1.0.0   (pre-release below=1.0.1-beta, recorded, not used)
```

When the **only** release below the declared version is a pre-release, the gate
skips and names the version it refused — it does not share the generic "nothing
below" message, which would hide a rejection behind a fact. A crate with no
stable predecessor has no dependent to break.

`--probe <crate> --probe-version <X>` answers "what would you compare against if
this crate declared X?" without editing a `Cargo.toml`. It is read only by
`--probe`, which prints and exits, so it cannot reach a verdict.

With no arguments the gate still selects crates by diffing `crates/*/src/**`
against a base ref. Nothing calls it that way now; it is kept because it is the
right shape for a local "what would this branch break" check.

`cargo-semver-checks` must be installed — `cargo install
cargo-semver-checks@0.50.0 --locked`. Its absence is a hard failure with that
remedy, never a skip: on the release path this gate is the last barrier, so a
missing tool reporting green would put the repo back where #4088 found it. CI
installs the same pinned version as a prebuilt binary.

## Skips, and why each one is a fact rather than an excuse

| Condition | Behaviour |
|---|---|
| `cargo-semver-checks` not installed | **hard failure** |
| listed in `scripts/semver-checks-crate-exclusions.tsv` | skip — no library consumer to protect; see below |
| `publish = false` | skip — never reaches crates.io |
| no library target | skip — a bin-only crate has no API surface to compare |
| crates.io returns 404 | skip — never published, so no baseline exists |
| only yanked versions | skip — no installable baseline |
| no non-yanked **stable** release below the declared version | skip — no stable predecessor; a pre-release below it is named in the skip line |
| declared version is already a major bump over the baseline | **inventory** — advisory, see below |
| **registry probe fails any other way** | **hard failure** |

The last row is the point. A SemVer gate that reports green because it could not
reach crates.io is worse than no gate, and "the failure branch advances state
anyway" is this repo's most-repeated defect shape. A network error, a 5xx, or a
malformed index entry exits non-zero and names `TOOL ERROR`.

Every skip prints its reason, and the final line reports how many crates were
checked versus skipped, so a run that verified nothing says so.

### Crate exclusions, and the assumption each one rests on

`scripts/semver-checks-crate-exclusions.tsv` names crates the gate must not
compare, one per row, each carrying a written reason. It is keyed by package
name (`tga`), not the `crates/` directory name.

| Crate | Reason |
|---|---|
| `trusty-mpm` | binary-only consumer surface — installed as the `tm` executable via `cargo install trusty-mpm` |

The gate protects **library** consumers: a dependent that re-resolves a version
floor on a lockfile-free `cargo install` and stops compiling. That is #4088, and
it mattered because `trusty-common` has 17 in-repo consumers. A binary user gets
a whole new executable on every install, so the library API `trusty-mpm` happens
to expose is not part of what they consume, and comparing it protects nobody.

The row is therefore a claim about consumption, not about the API: **no crate
depends on `trusty-mpm` as a library.** Verified from the manifests via
`cargo metadata --no-deps` — zero of the 29 workspace packages declare it as a
dependency, in any dependency table — and crates.io reported 0 reverse
dependencies on 2026-08-12.

Every row is a coverage hole, so a reason has to be a fact about how the crate is
consumed. "It is slow", "it always fails", and "we are mid-refactor" are not
reasons; the remedy for a firing gate is still to bump the breaking position.

**The gate re-checks the assumption before it honours the skip.** It asks
`cargo metadata` whether any workspace package declares a dependency on the
excluded crate. One does, and the skip is refused: the run exits 3 (`NO VERDICT`)
naming the dependent, so `preflight-publish.sh` CHECK 5 stops the publish rather
than approving one it verified nothing about. Removing the row restores full
gating and is the intended fix.

```
FAIL: EXCLUSION NO LONGER HOLDS — trusty-mpm is excluded from the SemVer gate
      because nothing depends on its library, but these workspace crates now do:
        - trusty-code
```

What that guard cannot see is an **out-of-repo consumer**: a crate on crates.io
depending on `trusty-mpm` as a library is invisible to a workspace-local check,
and nothing here detects one appearing. That is a stated assumption, re-checkable
in one command:

```bash
curl -s https://crates.io/api/v1/crates/trusty-mpm/reverse_dependencies | head -c 200
```

Pinned by `check_semver_selftest.sh` cases 23-24: an excluded crate is skipped
without attempting a comparison, and an exclusion whose premise has died refuses
the skip instead of granting it.

## A build failure is not a verdict (#5289)

`cargo-semver-checks` must build rustdoc for both sides before it can compare
anything, and that build has its own ways to fail. Until #5289 every one of them
exited non-zero into the same "a public API change requires a matching version
bump" remediation, so a rustdoc error was presented as a SemVer verdict and the
two were indistinguishable from the output.

The gate now concludes only from **positive evidence**: a verdict exists when
`cargo-semver-checks` printed its own per-crate summary line (`Checked … N
checks: …`). No summary line means no comparison happened — whatever the exit
status was, *including exit 0* — and that is reported as `NO VERDICT` on its own
exit status.

### Zero checks executed is not a pass ([#5440](https://github.com/bobmatnyc/trusty-tools/issues/5440))

The summary line alone was not enough. `cargo-semver-checks` skips its whole lint
set when the baseline → current delta already permits breakage, and prints a
summary saying so at exit 0:

```
Checking trusty-common v0.31.0 -> v0.30.1 (major change)
 Checked [   0.000s] 0 checks: 0 pass, 254 skip
 Summary no semver update required
```

The leading `0 checks:` is the number of lints that **ran**; the trailing
`254 skip` is what the tool declined to run. Keying on the line's presence read
that as a pass, so `preflight-publish.sh` CHECK 5 approved the publish having
verified nothing. Measured on `trusty-common` under rustc 1.94.1; the gate failed
closed on that machine only because a newer installed toolchain crashed rustdoc
instead, which produced no summary at all.

The gate now parses the count and requires it to be at least 1. A summary whose
count is zero, or does not parse, is `NO VERDICT` (exit 3) and says which of the
two happened — "executed no checks" and "never completed a check run" have
different remedies.

| Exit | Meaning |
|---|---|
| 0 | every checked crate is clean, or is a recorded skip |
| 1 | a verdict was computed **and it says break** — the only status that means "the API changed" |
| 2 | usage error |
| 3 | **no verdict** — rustdoc build failure, a run that executed zero checks, unreachable registry, missing tool, a diff that scanned nothing, or a crate exclusion whose premise has died. Nothing was compared, so nothing may be concluded. |

`scripts/preflight-publish.sh` CHECK 5 and `.github/workflows/semver-checks.yml`
both report exit 3 separately from exit 1. Both still stop the publish: a
non-verdict is not a pass. What changes is that neither one tells you to bump a
version on evidence that does not exist.

## Which toolchain the gate runs under (#5289)

`cargo-semver-checks` resolves dependencies in a scratch project that **ignores
this workspace's `Cargo.lock`**, so it can pick a newer transitive dependency
than the lockfile pins and inherit that dependency's MSRV. That is not
hypothetical: the scratch resolution took `takecell` 0.1.2 (`rust-version` 1.96)
where `Cargo.lock` pins 0.1.1 via `teloxide-core`, and rustdoc then refused to
build under this repo's MSRV 1.94. `takecell` reaches every default build of
`trusty-mpm` through `cli` → `telegram`, so this was the normal case, not an edge
one — the gate simply could not run.

So the gate runs under the **newest rustc it can find**, not the pinned one:
`check_semver.sh` scans `$RUSTUP_HOME/toolchains/*/bin` and prepends the highest
version when it beats the ambient one; CI installs `stable` for the same reason.
Pinning the offending dependency instead would fix one name until the next
dependency raises its floor.

This costs no coverage. The gate compares the public API of our source against
the registry, and which rustc renders the rustdoc JSON does not change what that
API is. MSRV compliance is a separate CI job (`dtolnay/rust-toolchain@1.94`) and
is untouched.

It also cannot manufacture a pass, which is the property that matters: the only
thing a wrong toolchain choice can do is fail the rustdoc build, and that is now
`NO VERDICT` with a non-zero exit. A machine with no newer toolchain installed
keeps the ambient one and degrades to exactly that honest path.

`SEMVER_GATE_TOOLCHAIN_BIN=<dir>` pins the rustc/cargo explicitly; set but empty
keeps the ambient toolchain. Note that `RUSTUP_TOOLCHAIN` does **not** work here
— where rustc and cargo resolve through mise shims, the shim execs a pinned
toolchain directly and never consults rustup, so the variable is silently
ignored. Prepending the toolchain's own `bin` directory is the mechanism that
works under both mise and a plain rustup install.

### An already-breaking bump gets an inventory, not a skip ([#5297](https://github.com/bobmatnyc/trusty-tools/issues/5297))

When the declared version already carries a breaking bump, no bump requirement
can be violated, and `cargo-semver-checks` left to itself runs zero lints:

```
Checking trusty-common v0.28.1 -> v0.29.0 (major change)
 Checked 0 checks: 0 pass, 254 skip
 Summary no semver update required
```

The gate used to stop there. Under Cargo's 0.x rule that fires on **every minor
bump of a `0.y.z` crate** — `trusty-search` 0.44.0 is where it was noticed — so
the releases most likely to break something by accident were exactly the ones
getting no coverage at all.

So the run happens anyway, with `--release-type minor` forcing the full
breaking-change lint set to apply, and its result is reported as an `INVENTORY`:
the list of what this release breaks, for a human to read against what they meant
to break.

It is **advisory and cannot fail the gate**. A major release is entitled to break
its API; reddening over a permitted break would only teach people to ignore the
gate. The pass/fail verdict still comes from the version comparison, which is
complete on its own — the inventory adds information, not a condition. An
inventory that could not be computed prints `NO INVENTORY` and is counted
separately in the summary line, so "no inventory" never reads as "inventory
clean".

What it costs is roughly four minutes of rustdoc, on already-breaking releases
only. What it buys is the one question the skip could not answer: did an
unintended break ride along with the intended one?

## Features

`cargo-semver-checks` defaults to enabling every feature, which here means
building CUDA and CoreML backends no CI runner can build. `--default-features`
is not the fix — it is theatre: `trusty-common` declares `default = []` and
`DaemonBridgeConfig` lives behind `mcp`, so the real #4088 break passes clean
under it (196 checks, 196 pass, 0 fail).

So the gate enumerates every declared feature and subtracts only what
`scripts/semver-checks-feature-exclusions.tsv` lists, each row carrying a written
reason. An excluded feature's public API is unchecked, so every row is a real
coverage hole and has to earn its place. "It is slow" is not a reason.

## When it fires

Bump the breaking position, or make the change non-breaking. `#[non_exhaustive]`
on a struct or enum makes future field and variant additions non-breaking by
construction — the fix #4088 asked for and deferred.

```bash
cargo semver-checks --explain constructible_struct_adds_field
```

There is no override flag on CHECK 5, and none is needed. Bumping the breaking
position turns the run into an advisory inventory, so a false positive and a real
break have the same safe remedy.

## Running it locally

```bash
bash scripts/check_semver.sh --crate trusty-common  # one crate — the release path
bash scripts/check_semver.sh --probe trusty-common  # what would it compare to?
bash scripts/check_semver.sh                        # every crate changed vs origin/main
```

Or through the blocking caller, which also reports the other four publish
guards:

```bash
bash scripts/preflight-publish.sh --check-only trusty-common
```

To run it in CI without cutting a release, dispatch the workflow:

```bash
gh workflow run semver-checks.yml -f crate=trusty-common
```

## Self-test

`scripts/check_semver_selftest.sh` runs first in CI. Cases 1-4 cover the gate's
original fail-open surfaces — an unscanned diff and an unreachable or erroring
index — plus the 404 case, which must stay a clean skip so the other cases are
known to fail for the right reason.

Cases 9-12 (#5296, #5297) pin baseline selection and the inventory arm: a crate
whose declared version is already on crates.io must still be compared against the
release *before* it; a crate with nothing published below its declared version is
a recorded skip that attempts no comparison; an already-breaking bump must produce
an inventory rather than a skip and must stay green doing it; and an inventory
that could not run must say so in both its own line and the summary. Cases 9, 11
and 12 fail against the pre-#5296 gate, which is what makes them regression tests
rather than descriptions of current behaviour.

Cases 13-15 pin pre-release handling, case 13 running the reproduction on record
verbatim (`["0.9.9", "1.0.0-rc1", "1.0.0", "1.0.1-beta"]`, declared 1.0.1). Case
14 pins that a *nearer* pre-release still loses to the last stable release, and
case 15 that a skip caused by the exclusion names what it refused.

Cases 5-8 (#5289) pin the verdict/non-verdict split: a real break must report
`BREAK` and exit 1; a rustdoc build error must report `NO VERDICT`, exit 3, and
never reach the version-bump remediation; an exit-0 run that compared nothing
must still fail; and a genuinely clean run must exit 0, which is what proves the
other three fail on classification rather than because the gate is broken
outright.

Cases 20-21 (#5440) pin the other way a completed run says nothing: a summary
reporting `0 checks: 0 pass, 254 skip` at exit 0 must be `NO VERDICT` in the
pass/fail arm and a blind inventory in the advisory one, never "no breaking
changes found". Both fail against the pre-fix gate, which exits 0 on the first
and reports an empty inventory on the second. Their fixture, `all-skipped.out`,
is the former `clean.out` — the case that was supposed to prove the gate can pass
a crate was itself being satisfied by a run that checked nothing.

Cases 23-24 pin the crate-exclusion arm: `trusty-mpm` must be skipped with its
reason on the line and no comparison attempted, and an exclusion listing a crate
that a workspace package actually depends on must refuse the skip and exit 3.
Case 24 uses `trusty-agents-common` — which three crates do depend on — against a
fixture exclusions file, so it fails the moment the dependent check is removed.
Case 8 is what keeps the exclusion from leaking: it runs a non-excluded crate
through a full clean comparison against the real exclusions file.

Those four replace only the `cargo semver-checks` subprocess, via a stub `cargo`
on `PATH` that forwards everything else to the real one — so crate resolution,
the registry probe, feature enumeration, classification, the messages and the
exit status are all the gate's own code. The replayed output is captured
verbatim from real runs; see
[`scripts/test-data/semver-gate/README.md`](../../scripts/test-data/semver-gate/README.md)
for how each fixture was taken and how to refresh one.

The self-test is only worth its runtime if it can fail, so it was checked by
mutation: deleting the classification makes cases 6 and 7 fail (reproducing the
original defect — a build error reported as a required version bump, and a
silent no-op reported as a pass); making the no-verdict status 0 fails five
cases; and forcing the classifier to always answer "no verdict" fails cases 5
and 8.
