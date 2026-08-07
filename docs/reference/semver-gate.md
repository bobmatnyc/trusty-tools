# Public-API / SemVer gate

`scripts/check_semver.sh`, wired into CI by `.github/workflows/semver-checks.yml`
(issue [#5050](https://github.com/bobmatnyc/trusty-tools/issues/5050)).

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

For each crate whose `crates/<crate>/src/**` changed in the PR, the gate resolves
the latest non-yanked crates.io release and runs `cargo semver-checks` against
it. The tool version is pinned in the workflow.

## Skips, and why each one is a fact rather than an excuse

| Condition | Behaviour |
|---|---|
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

Bump the version, or make the change non-breaking. `#[non_exhaustive]` on a
struct or enum makes future field and variant additions non-breaking by
construction — the fix #4088 asked for and deferred.

```bash
cargo semver-checks --explain constructible_struct_adds_field
```

## Running it locally

```bash
bash scripts/check_semver.sh                        # diff against origin/main
bash scripts/check_semver.sh --crate trusty-common  # one crate, ignore the diff
bash scripts/check_semver.sh --probe trusty-common  # what would it compare to?
```

## Self-test

`scripts/check_semver_selftest.sh` runs first in CI and covers the gate's two
fail-open surfaces — an unscanned diff and an unreachable or erroring index —
plus the 404 case, which must stay a clean skip so the other cases are known to
fail for the right reason.
