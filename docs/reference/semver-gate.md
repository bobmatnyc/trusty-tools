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

For the named crate, the gate resolves the latest non-yanked crates.io release
and runs `cargo semver-checks` against it. `--crate` accepts either the package
name (`tga`) or the `crates/` directory name (`trusty-git-analytics`), because a
release tag's prefix can be either
([#1128](https://github.com/bobmatnyc/trusty-tools/issues/1128)).

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
| `publish = false` | skip — never reaches crates.io |
| no library target | skip — a bin-only crate has no API surface to compare |
| crates.io returns 404 | skip — never published, so no baseline exists |
| only yanked versions | skip — no installable baseline |
| declared version is already a major bump over the baseline | skip — see below |
| **registry probe fails any other way** | **hard failure** |

The last row is the point. A SemVer gate that reports green because it could not
reach crates.io is worse than no gate, and "the failure branch advances state
anyway" is this repo's most-repeated defect shape. A network error, a 5xx, or a
malformed index entry exits non-zero and names `TOOL ERROR`.

Every skip prints its reason, and the final line reports how many crates were
checked versus skipped, so a run that verified nothing says so.

### The already-a-major-bump skip

When the declared version already carries a breaking bump, `cargo-semver-checks`
itself runs zero lints. Observed directly on `trusty-common`:

```
Checking trusty-common v0.28.1 -> v0.29.0 (major change)
 Checked 0 checks: 0 pass, 254 skip
 Summary no semver update required
```

The gate reaches that verdict by comparing versions instead of building two
rustdoc trees to be told nothing applies. This cuts cost, not coverage — the
skipped run had no coverage to give. Cargo's 0.x rule applies: `0.28.1 → 0.29.0`
is a major release.

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
position makes the gate record an already-breaking release and skip it, so a
false positive and a real break have the same safe remedy.

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

`scripts/check_semver_selftest.sh` runs first in CI and covers the gate's two
fail-open surfaces — an unscanned diff and an unreachable or erroring index —
plus the 404 case, which must stay a clean skip so the other cases are known to
fail for the right reason.
