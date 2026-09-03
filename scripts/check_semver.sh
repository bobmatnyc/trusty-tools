#!/usr/bin/env bash
#
# check_semver.sh — public-API / SemVer gate (issue #5050, evidence #4088).
#
# Why: this workspace publishes 20+ crates to crates.io and had NO mechanical
#   check that a public-API change carried a matching version bump. #4088 is the
#   proof: `trusty-common` 0.22.5 added a required public field
#   (`DaemonBridgeConfig.no_spawn_hint`, daemon_bridge.rs:117) and shipped it as
#   a PATCH bump. Every dependent on a `^0.22` floor re-resolved to it on a
#   lockfile-free `cargo install` and failed to compile with E0063;
#   `trusty-analyze` 0.7.3 was yanked as a result. A workspace `cargo check`
#   CANNOT reproduce that class of bug, because the path override in the root
#   Cargo.toml always pairs local source with local dependency. The break is
#   only visible against the REGISTRY, which is what this gate compares to.
#
# What: for one or more crates, resolves the PREVIOUS release on crates.io — the
#   greatest non-yanked version STRICTLY BELOW the crate's declared version — and
#   runs `cargo semver-checks` against it. Crate selection is either explicit
#   (`--crate`) or, with no arguments, every crate whose `crates/<crate>/src/**`
#   changed against a base ref.
#
# THE BASELINE IS THE PREVIOUS RELEASE, NOT THE LATEST (issue #5296). Until this
#   fix the baseline was crates.io's LATEST release, which is only the previous
#   one while the crate is unpublished. `.github/workflows/semver-checks.yml`
#   fires on the `<crate>-v<version>` tag push, and by the time that job queries
#   the registry the `cargo publish` has already landed — so latest == the
#   version under test, the crate was compared against itself, and every run
#   reported no change. Observed on trusty-agents-common 0.5.0, trusty-progress
#   0.3.0 and tga 2.12.0 during the 1.3.5 cut; still reproducible on main against
#   trusty-search 0.45.0. Selecting "greatest published below the declared
#   version" is the same version at preflight time and the right one after the
#   publish, so the gate no longer depends on where in the release sequence it
#   happens to run.
#
# Where this runs (#5149, moved from per-PR): the BLOCKING caller is
#   `scripts/preflight-publish.sh` CHECK 5, which runs immediately before
#   `cargo publish` and whose nonzero exit is the documented absolute stop — so
#   a break is caught while the upload can still be prevented, not after
#   crates.io has made it permanent. `.github/workflows/semver-checks.yml` runs
#   the same command on every `<crate>-v<version>` tag push as a second,
#   independent report. The per-PR trigger was removed: installing the pinned
#   tool and warming a cold rustdoc cache cost 20+ minutes on every PR, and a
#   SemVer break only matters when something is actually published.
#
# Baseline policy — SCOPED TO PUBLISHED CRATES, and an absent baseline is a
#   RECORDED SKIP, never a silent one:
#     - listed in scripts/semver-checks-crate-exclusions.tsv
#                                    -> skip (no LIBRARY consumer to protect —
#                                       see "Crate exclusions" below, which is
#                                       also where the guard that keeps this row
#                                       from going silent lives)
#     - `publish = false`            -> skip (never reaches crates.io)
#     - no library target            -> skip (a bin-only crate has no API surface
#                                       cargo-semver-checks can compare)
#     - registry says 404            -> skip (never published; no baseline exists)
#     - registry has only yanked vers-> skip (no installable baseline)
#     - no non-yanked STABLE release
#       BELOW the declared version   -> skip (no stable predecessor: the crate's
#                                       first real release, so there is no
#                                       earlier API a dependent could be on. A
#                                       pre-release below it is NAMED in the skip
#                                       line, never silently passed over)
#     - registry probe fails any
#       other way (network, 5xx,
#       malformed index, curl error) -> HARD FAIL, exit non-zero
#   The last line is the point. This repo's recurring defect shape is a failure
#   branch that advances state anyway ("fail-open / cursor-advance"), and a
#   SemVer gate that reports green because it could not reach crates.io is worse
#   than no gate at all. Every skip above is a fact about the crate; a probe
#   error is a fact about the GATE, and the gate does not get to excuse itself.
#
# Already-breaking -> INVENTORY, not a skip (issue #5297(b)). When the declared
#   version already carries a breaking bump (0.28.1 -> 0.29.0 is major under
#   Cargo's 0.x rules, as is 1.3.4 -> 2.0.0), no bump requirement can be
#   violated, so there is no PASS/FAIL question left to ask. The gate used to
#   stop there. Under the 0.x rule that fires on every MINOR bump of a 0.y.z
#   crate — trusty-search 0.44.0 is where the owner hit it — which erased all
#   coverage on exactly the releases most likely to break something by accident.
#
#   So the run happens anyway, with `--release-type minor` forcing the full
#   breaking-change lint set to apply, and its result is reported as an
#   INVENTORY: the complete list of what this release breaks, for a human to
#   check against what they meant to break. Its FINDINGS are ADVISORY and cannot
#   fail this gate — a major release is entitled to break things, and turning a
#   permitted break into a red gate would just teach people to ignore it. The
#   gate's own preconditions are unchanged: cargo-semver-checks must still be
#   installed, because "the tool is missing" is a fact about the gate's readiness
#   and never about the crate. What it costs is
#   ~4 minutes of rustdoc on already-breaking releases only; what it buys is the
#   one question the skip could not answer: did an UNINTENDED break ride along?
#
#   An inventory that cannot be computed says so on its own line and in the
#   summary. It is never rendered as "checked", and never silently absent.
#
# A BUILD FAILURE IS NOT A VERDICT (issue #5289). cargo-semver-checks has to
#   build rustdoc for both sides before it can compare anything, and that build
#   can fail on its own — a broken dependency graph, an MSRV floor a dependency
#   raised, a baseline the registry would not hand over. Until #5289 every one of
#   those exited non-zero and fell straight into the "a public API change
#   requires a matching version bump" remediation below, so a rustdoc error was
#   presented as a SemVer verdict and the two were indistinguishable from the
#   output. The gate now classifies on POSITIVE EVIDENCE instead: a verdict
#   exists only when the tool printed its own per-crate `N checks:` summary line
#   AND that line says at least one check RAN (issue #5440 — a summary reporting
#   `0 checks: 0 pass, 254 skip` is the tool declining to compare, and the gate
#   used to read it as a pass).
#   No summary line means no comparison happened, whatever the exit status was —
#   including exit 0 — and that is reported as NO VERDICT on its own exit code.
#   Absence of evidence is never a pass; same rule as the scan floor below.
#   The line is matched with ANSI colour stripped (issue #5500), because CI
#   forces colour into the middle of the marker token and the check went blind
#   to every run — see verdict_computed below for the bytes and for PR #5458's
#   two casualties.
#
# Toolchain (issue #5289): cargo-semver-checks re-resolves the dependency graph
#   in a scratch project that IGNORES this workspace's Cargo.lock, so it can pick
#   a newer transitive dependency than the lockfile pins and inherit that
#   dependency's MSRV. Observed: the scratch resolution took `takecell` 0.1.2
#   (rust-version 1.96) where Cargo.lock pins 0.1.1 via teloxide-core, and
#   rustdoc then refused to build under this repo's MSRV 1.94. `takecell` reaches
#   every default build of trusty-mpm through `cli` -> `telegram`, so this is the
#   normal case and not an edge one. The gate therefore runs under the NEWEST
#   rustc it can find rather than the pinned one — see select_toolchain below for
#   why that costs no coverage and cannot manufacture a pass.
#
# Crate exclusions: the gate protects LIBRARY consumers, so a crate nothing
#   links as a library has nobody to protect. trusty-mpm is installed as the `tm`
#   executable, and a binary user gets a whole new executable on every
#   `cargo install`, so the library API it happens to expose is not part of what
#   they consume. Rows live in scripts/semver-checks-crate-exclusions.tsv, each
#   with a written reason, and each is a real coverage hole.
#
#   A ROW IS A CLAIM ABOUT CONSUMPTION, AND CLAIMS GO STALE. A skip list is the
#   exact shape this repo keeps getting wrong: it stops protecting and reports
#   success. So the premise is re-checked on every run rather than trusted — the
#   gate asks `cargo metadata` whether any workspace package depends on the
#   excluded crate, and REFUSES the skip (exit 3, naming the dependent) when one
#   does. What it cannot see is an out-of-repo consumer on crates.io; that stays
#   a stated assumption, re-checkable with
#   `curl -s https://crates.io/api/v1/crates/<crate>/reverse_dependencies`.
#   See docs/reference/semver-gate.md, "Crate exclusions".
#
# Features: cargo-semver-checks' default heuristic enables every feature, which
#   here means building CUDA and CoreML backends that no CI runner can build
#   (`cudarc` panics with "nvcc --version failed"). Passing --default-features
#   instead would be THEATRE: `trusty-common`'s default feature set is literally
#   `default = []`, and `DaemonBridgeConfig` lives behind `mcp`, so the #4088
#   break passes cleanly under it (verified: "196 checks: 196 pass"). The gate
#   therefore enumerates every declared feature and subtracts only the ones
#   listed in scripts/semver-checks-feature-exclusions.tsv, each with a written
#   reason. A new unbuildable feature is a deliberate line in that file, not a
#   silent hole.
#
# Usage:
#   bash scripts/check_semver.sh --crate trusty-common # one crate (release path)
#   bash scripts/check_semver.sh --probe trusty-common # baseline decision only
#   bash scripts/check_semver.sh --probe <c> --probe-version 1.0.1
#                                                      # ...as if it declared 1.0.1
#   bash scripts/check_semver.sh                       # diff vs origin/main
#   bash scripts/check_semver.sh --base <ref>          # explicit base
#   SEMVER_GATE_BASE=<ref> bash scripts/check_semver.sh
#
#   `--crate` accepts either the crates.io package name (`tga`) or the crates/
#   directory name (`trusty-git-analytics`), so a release tag's prefix can be
#   handed straight to it in either accepted form (#1128).
#
#   SEMVER_GATE_TOOLCHAIN_BIN=<dir>  pin the rustc/cargo the check runs under.
#   SEMVER_GATE_TOOLCHAIN_BIN=       (set but empty) keep the ambient toolchain.
#   SEMVER_GATE_CRATE_EXCLUSIONS=<file>  read crate exclusions from <file>. A
#                                    self-test seam; no caller sets it.
#   PREFLIGHT_NO_SCCACHE=1           do not wrap rustc with sccache even when it
#                                    is installed.
#
# Build acceleration: when sccache is on PATH this gate runs cargo-semver-checks
#   under RUSTC_WRAPPER=sccache, so a fresh worktree gets its dependency closure
#   from cache instead of recompiling openssl-sys and the rest. It is applied
#   ONLY to that subprocess, as an `env` prefix — see semver_checks_run below —
#   and never exported, so nothing else this script or its callers spawn inherits
#   it. No sccache means this gate issues exactly the command it issued before.
#
#   IT CANNOT MAKE THE GATE PASS. A wrapper changes which process invokes rustc,
#   not what rustc is invoked on. A cache that served a wrong object fails the
#   build, which prints no `N checks:` summary, which verdict_computed
#   classifies as NO VERDICT — the same loud exit 3 a rustdoc crash gets, never
#   a skip and never a pass. check_semver_selftest.sh case 27 pins that.
#
#   A persistent CARGO_TARGET_DIR was tried and rejected: it moves the current
#   crate's rustdoc JSON to a flat, name-keyed path, blinding
#   check_semver_types.sh and letting parallel worktrees overwrite each other's
#   document. The measurement is in scripts/lib/build_accel.sh; do not re-add it.
#   An AMBIENT CARGO_TARGET_DIR does the same damage, and this repo sanctions
#   exporting one, so semver_checks_run unsets it for that subprocess.
#
# Exit (#5289 split 1 from 3 — a verdict and a non-verdict are different facts):
#   0  every checked crate is SemVer-clean, or is a recorded skip.
#   1  A VERDICT WAS COMPUTED AND IT SAYS BREAK — a crate needs a bump it does
#      not have. This is the only status that means "the API changed".
#   2  usage error.
#   3  NO VERDICT — the gate could not do its job: cargo-semver-checks never
#      completed a check run (rustdoc build failure, baseline retrieval
#      failure), the diff scanned nothing, the registry was unreachable, the
#      tool is missing, or a crate exclusion's premise has died. Nothing was
#      compared, so nothing may be concluded.
#
# Test: `scripts/check_semver_selftest.sh` proves every way this gate could lie
#   — a vacuous scan, an unreachable registry, and (since #5289) a rustdoc build
#   failure or a silent no-op rendered as a SemVer verdict — all exit non-zero,
#   and that a real break is still reported as a break. Cases 9-11 (#5296,
#   #5297b) add the post-publish shape: a crate whose declared version is already
#   on crates.io must still be compared against the release BEFORE it, a first
#   release is a recorded skip, and an already-breaking bump produces an
#   inventory instead of a skip. Cases 13-15 pin pre-release handling: the
#   reproduction on record is `["0.9.9", "1.0.0-rc1", "1.0.0", "1.0.1-beta"]`
#   declared 1.0.1, which selected 1.0.0-rc1 before the rank element existed.
#   Cases 16-18 (#5500) replay PR #5458's own CI bytes, where forced colour hid the
#   summary line and both a clean run and a four-failure run were reported as
#   never having happened. Cases 20-21 (#5440) pin the zero-check false pass:
#   a summary that says `0 checks: 0 pass, 254 skip` at exit 0 is NO VERDICT in
#   the PASS/FAIL arm and a BLIND inventory in the advisory one. Cases 23-24 pin
#   the crate-exclusion arm: an excluded crate is a recorded skip that attempts
#   no comparison, and an exclusion contradicted by a workspace dependency
#   refuses the skip rather than granting it.
#   The catch itself is demonstrated in PR #5051 against #4088's real shape.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   plus `git`, `curl`, `cargo`, `python3` (JSON parsing only).

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Build acceleration (sccache wrapper). Resolves to empty on a machine without
# sccache, and empty means this gate runs exactly the command it ran before.
# See scripts/lib/build_accel.sh.
# shellcheck source=lib/build_accel.sh
. "${REPO_ROOT}/scripts/lib/build_accel.sh"

BASE="${SEMVER_GATE_BASE:-origin/main}"
EXPLICIT_CRATES=""
PROBE_ONLY=""
PROBE_VERSION_OVERRIDE=""
INDEX_BASE="${SEMVER_GATE_INDEX_BASE:-https://index.crates.io}"
EXCLUSIONS_FILE="${REPO_ROOT}/scripts/semver-checks-feature-exclusions.tsv"
# Whole-crate exclusions, one row per crate with a written reason. Overridable so
# check_semver_selftest.sh can drive the arm from a fixture instead of the real
# file; the default is the only path any caller uses.
CRATE_EXCLUSIONS_REL="scripts/semver-checks-crate-exclusions.tsv"
CRATE_EXCLUSIONS_FILE="${SEMVER_GATE_CRATE_EXCLUSIONS:-${REPO_ROOT}/${CRATE_EXCLUSIONS_REL}}"

# Exit statuses. `1` is reserved for a COMPUTED verdict that says break; every
# way the gate can fail to compute one is `3` (#5289). Callers that only test
# for zero keep working; callers that want to tell "your API changed" from "the
# gate is broken" now can — see preflight-publish.sh CHECK 5.
EXIT_SEMVER_BREAK=1
EXIT_NO_VERDICT=3

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --base needs a ref" >&2
        exit 2
      }
      BASE="$2"
      shift 2
      ;;
    --crate)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --crate needs a package name" >&2
        exit 2
      }
      EXPLICIT_CRATES="${EXPLICIT_CRATES}${2}"$'\n'
      shift 2
      ;;
    --probe)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --probe needs a package name" >&2
        exit 2
      }
      PROBE_ONLY="$2"
      shift 2
      ;;
    --probe-version)
      # Diagnostic only: answer "what would you compare against if the declared
      # version were X?" without editing a Cargo.toml. It is read ONLY by the
      # --probe branch, which prints and exits, so it cannot influence a verdict.
      [[ $# -lt 2 ]] && {
        echo "ERROR: --probe-version needs a version" >&2
        exit 2
      }
      PROBE_VERSION_OVERRIDE="$2"
      shift 2
      ;;
    -h | --help)
      # Prints the contiguous comment block after the shebang, so editing the
      # header above never silently truncates --help against a stale line range.
      awk 'NR > 1 && /^#/ { print; next } NR > 1 { exit }' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# registry_probe <crate> <declared-version-or-empty> — print three lines:
#     1. the latest non-yanked version on crates.io, of any kind, or `NONE`
#     2. the BASELINE: the greatest non-yanked STABLE version strictly BELOW
#        <declared-version>, or `NONE`
#     3. the greatest non-yanked PRE-RELEASE strictly below it, or `NONE` —
#        diagnostic only, so a skip can name what it declined to use
# With an empty declared version there is no upper bound, so every release is a
# candidate (that is the `--probe` form, which may be handed a crate this
# workspace does not build).
#
# Why three lines: they answer different questions and the gate needs all three.
# The baseline is what `cargo semver-checks` compares against (#5296 — using the
# latest made a published crate compare against itself). The latest separates
# "never published" from "published, but this is the earliest release", and
# those two skips read very differently to whoever has to trust the run. Line 3
# exists so the "nothing below" skip can never be silent about a pre-release it
# saw and rejected.
#
# PRE-RELEASES ARE NEVER A BASELINE (code-critic on PR #5445). A dependent
# resolves `^1.0` to a stable release; Cargo will not pick `1.0.0-rc1` unless
# someone names it exactly. So a break measured against a pre-release is a break
# no ordinary consumer can experience, while a real break against the stable
# release it shadows goes unreported — and `release_type` strips the suffix, so
# `1.0.1-beta -> 1.0.1` computes as `none` and the gate would demand a bump over
# a version nobody resolves to. Ordering pre-releases correctly is not enough on
# its own; they have to be out of the candidate set. When the ONLY release below
# the declared version is a pre-release, the gate skips and says which one it
# refused, because a crate with no stable predecessor has no consumer to break.
#
# Why the network handling is spelled out: this is the one place the gate talks
#   to the network, so it is the one place a fail-open can hide. `curl -f` alone
#   is not enough — it conflates a 404 (a real fact: never published) with a 503
#   (the gate is blind). The two are separated here by reading the HTTP status
#   explicitly, and everything that is neither 200 nor 404 exits the whole
#   script.
# What: reads the crates.io sparse index (newline-delimited JSON, one object per
#   version). The sparse index is used rather than the crates.io API because it
#   is CDN-cached and unrate-limited.
# ---------------------------------------------------------------------------
registry_probe() {
  local crate="$1" current="${2:-}" path body code
  case "${#crate}" in
    1) path="1/${crate}" ;;
    2) path="2/${crate}" ;;
    3) path="3/${crate:0:1}/${crate}" ;;
    *) path="${crate:0:2}/${crate:2:2}/${crate}" ;;
  esac

  body="$(mktemp "${TMPDIR:-/tmp}/semver.idx.XXXXXX")"
  code="$(curl -sS --retry 3 --retry-connrefused --max-time 60 \
    -o "$body" -w '%{http_code}' "${INDEX_BASE}/${path}" 2>/dev/null)" || {
    rm -f "$body"
    echo "FAIL: TOOL ERROR — could not reach the crates.io index for '${crate}'." >&2
    echo "      ${INDEX_BASE}/${path} — curl failed." >&2
    echo "      Whether a baseline exists is UNKNOWN, so no skip may be granted." >&2
    echo "      This is NOT a pass (issue #5050)." >&2
    exit "$EXIT_NO_VERDICT"
  }

  if [[ "$code" == "404" ]]; then
    rm -f "$body"
    printf 'NONE\nNONE\nNONE\n'
    return 0
  fi

  if [[ "$code" != "200" ]]; then
    echo "FAIL: TOOL ERROR — crates.io index returned HTTP ${code} for '${crate}'." >&2
    echo "      ${INDEX_BASE}/${path}" >&2
    sed 's/^/       /' "$body" >&2 | head -5
    echo "      Whether a baseline exists is UNKNOWN, so no skip may be granted." >&2
    echo "      This is NOT a pass (issue #5050)." >&2
    rm -f "$body"
    exit "$EXIT_NO_VERDICT"
  fi

  local answer rc=0
  answer="$(python3 - "$body" "$current" <<'PY'
import json, sys

def prerelease(v):
    return "-" in v.split("+")[0]

def key(v):
    """Total order over version strings.

    The 4th element is the pre-release rank: 0 for a pre-release, 1 for a final
    release, so 1.0.0-rc1 sorts strictly BELOW 1.0.0 instead of collapsing onto
    it. Without it both keyed to (1, 0, 0) and the tie went to whichever the
    index listed first — which is how 1.0.0-rc1 beat 1.0.0 for the baseline
    slot. The 5th element only makes ties between two pre-releases of the same
    core deterministic; it compares identifiers as text, not by SemVer's
    numeric-identifier rule, and nothing but a diagnostic depends on it.
    """
    core = v.split("+")[0].split("-")[0]
    parts = (core.split(".") + ["0", "0", "0"])[:3]
    suffix = v.split("+")[0].split("-", 1)[1] if prerelease(v) else ""
    try:
        return tuple(int(p) for p in parts) + (0 if prerelease(v) else 1, suffix)
    except ValueError:
        raise SystemExit(2)

# An empty declared version means "no upper bound" — the --probe form, which may
# name a crate this workspace does not build and therefore has no version for.
ceiling = key(sys.argv[2]) if sys.argv[2] else None

latest = prev = pre_below = None
with open(sys.argv[1]) as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        rec = json.loads(line)
        if rec.get("yanked"):
            continue
        v = rec["vers"]
        if latest is None or key(v) > key(latest):
            latest = v
        # STRICTLY below: a version equal to the declared one is the release
        # under test, never its own baseline (#5296). No ceiling means no upper
        # bound, so every release is a candidate.
        if ceiling is not None and key(v) >= ceiling:
            continue
        if prerelease(v):
            # Recorded, never selected — see the header. Kept so the caller's
            # skip message can name the pre-release it refused.
            if pre_below is None or key(v) > key(pre_below):
                pre_below = v
        elif prev is None or key(v) > key(prev):
            prev = v
print(latest if latest else "NONE")
print(prev if prev else "NONE")
print(pre_below if pre_below else "NONE")
PY
  )" || rc=$?

  rm -f "$body"
  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: TOOL ERROR — the crates.io index entry for '${crate}' did not parse." >&2
    echo "      Whether a baseline exists is UNKNOWN, so no skip may be granted." >&2
    echo "      This is NOT a pass (issue #5050)." >&2
    exit "$EXIT_NO_VERDICT"
  fi
  printf '%s\n' "$answer"
}

# ---------------------------------------------------------------------------
# release_type <baseline> <current> — print major | minor | patch | none.
#
# Cargo's compatibility rules, not plain SemVer: for 0.x the MINOR position is
# the breaking one, so 0.28.1 -> 0.29.0 is a major release. `none` means the two
# versions are equal, or current is older than baseline; since #5296 the caller
# selects a baseline STRICTLY BELOW the declared version, so `none` is now
# unreachable from the gate and survives only as a total function's zero case.
#
# 0.0.z IS ALWAYS MAJOR (#5440, code-critic on PR #5522). Cargo gives a 0.0.z
# crate NO compatible range at all — every 0.0.z bump is breaking. Classified
# `minor`, such a crate lands in the PASS/FAIL arm, where cargo-semver-checks
# reads the same pair as a permitted break, runs 0 of 254 checks, and the gate
# correctly refuses the non-verdict — a publish blocked by an exit 3 that nothing
# can clear. `major` routes it to the advisory INVENTORY arm instead, which is
# what every other permitted break gets. Latent today: the lowest declared
# version in this workspace is 0.1.0.
# ---------------------------------------------------------------------------
release_type() {
  python3 - "$1" "$2" <<'PY'
import sys

def parse(v):
    core = v.split("+")[0].split("-")[0]
    parts = (core.split(".") + ["0", "0", "0"])[:3]
    return tuple(int(p) for p in parts)

b = parse(sys.argv[1])
c = parse(sys.argv[2])
if c <= b:
    print("none")
elif b[0] == 0 and c[0] == 0:
    # #5440: a 0.0.z baseline has no compatible range, so any bump off it breaks.
    if b[1] == 0:
        print("major")
    else:
        print("major" if c[1] != b[1] else ("minor" if c[2] != b[2] else "none"))
elif c[0] != b[0]:
    print("major")
elif c[1] != b[1]:
    print("minor")
else:
    print("patch")
PY
}

# ---------------------------------------------------------------------------
# feature_args <crate> — print one `--features <name>` pair per line for every
# declared feature except `default` (implied), the tool's own reserved prefixes,
# and anything listed in the exclusions TSV for this crate.
# ---------------------------------------------------------------------------
feature_args() {
  local crate="$1" excluded=""
  if [[ -f "$EXCLUSIONS_FILE" ]]; then
    excluded="$(awk -F'\t' -v c="$crate" '$0 !~ /^#/ && $1 == c { print $2 }' "$EXCLUSIONS_FILE")"
  fi
  python3 "$PY_HELPER" features "$META_FILE" "$crate" "$excluded" || {
    echo "FAIL: TOOL ERROR — could not resolve the feature set for '${crate}'." >&2
    exit "$EXIT_NO_VERDICT"
  }
}

# ---------------------------------------------------------------------------
# Workspace metadata, captured ONCE to a file. `--no-deps` does not resolve the
# dependency graph, so this neither reads nor rewrites Cargo.lock — required,
# because refreshing the lockfile turns the MSRV 1.94 job red.
#
# The metadata goes to a FILE rather than a shell variable piped into `python3 -`
# because `python3 -` reads its program from stdin: a pipe and a heredoc cannot
# both feed it, and the pipe silently loses.
# ---------------------------------------------------------------------------
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/semver-gate.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT
META_FILE="${SCRATCH}/metadata.json"
PY_HELPER="${SCRATCH}/meta.py"

cat > "$PY_HELPER" <<'PY'
"""Metadata queries for check_semver.sh. Exits 2 on anything unanswerable."""
import json
import os
import sys

mode, meta_path = sys.argv[1], sys.argv[2]
with open(meta_path) as fh:
    meta = json.load(fh)

if mode == "field":
    crate, field = sys.argv[3], sys.argv[4]
    for p in meta["packages"]:
        if p["name"] != crate:
            continue
        if field == "version":
            print(p["version"])
        elif field == "publishable":
            print("no" if p.get("publish") == [] else "yes")
        elif field == "has_lib":
            kinds = {k for t in p["targets"] for k in t["kind"]}
            print("yes" if kinds & {"lib", "rlib", "cdylib", "proc-macro"} else "no")
        else:
            raise SystemExit(2)
        break
    else:
        raise SystemExit(2)

elif mode == "dir":
    root, want = sys.argv[3], sys.argv[4]
    target = os.path.realpath(os.path.join(root, "crates", want, "Cargo.toml"))
    for p in meta["packages"]:
        if os.path.realpath(p["manifest_path"]) == target:
            print(p["name"])
            break

elif mode == "exists":
    want = sys.argv[3]
    print("yes" if any(p["name"] == want for p in meta["packages"]) else "no")

elif mode == "dependents":
    # Workspace packages that declare <want> as a dependency, one per line. A
    # Cargo dependency IS a library dependency — nothing can depend on another
    # crate's binary — so a non-empty answer means the crate has an in-repo
    # library consumer, which is what a crate exclusion claims it does not.
    # Dev- and build-dependencies count: they link the library too.
    want = sys.argv[3]
    for p in meta["packages"]:
        if p["name"] == want:
            continue
        if any(d["name"] == want for d in p["dependencies"]):
            print(p["name"])

elif mode == "features":
    crate = sys.argv[3]
    excluded = set(filter(None, sys.argv[4].split()))
    for p in meta["packages"]:
        if p["name"] != crate:
            continue
        for f in sorted(p["features"]):
            if f == "default" or f in excluded:
                continue
            # Mirrors cargo-semver-checks' own reserved-name heuristic.
            if f.startswith(("_", "unstable")) or f in ("nightly", "bench", "no_std"):
                continue
            print("--features")
            print(f)
        break
    else:
        raise SystemExit(2)

else:
    raise SystemExit(2)
PY

if ! cargo metadata --no-deps --format-version 1 > "$META_FILE" 2> "${SCRATCH}/meta-err.txt"; then
  echo "FAIL: TOOL ERROR — 'cargo metadata --no-deps' failed:" >&2
  sed 's/^/       /' "${SCRATCH}/meta-err.txt" >&2
  exit "$EXIT_NO_VERDICT"
fi

# pkg_field <crate> <field> — one line of package metadata. A crate the metadata
# does not describe is a gate failure, never an empty answer that reads as a skip.
pkg_field() {
  python3 "$PY_HELPER" field "$META_FILE" "$1" "$2" || {
    echo "FAIL: TOOL ERROR — no workspace metadata for package '${1}' (field '${2}')." >&2
    exit "$EXIT_NO_VERDICT"
  }
}

# crate_exclusion_reason <crate> — the reason column for <crate> in the crate
# exclusions TSV, or empty when the crate is not excluded. Keyed on the package
# name, which is what the loop below has already resolved to.
crate_exclusion_reason() {
  [[ -f "$CRATE_EXCLUSIONS_FILE" ]] || return 0
  awk -F'\t' -v c="$1" '$0 !~ /^#/ && $1 == c { print $2; exit }' "$CRATE_EXCLUSIONS_FILE"
}

# dir_to_crate <dirname> — package name for crates/<dirname>/, or empty when the
# directory holds no package. The two names differ in this workspace
# (crates/trusty-git-analytics/ is package `tga`).
dir_to_crate() {
  python3 "$PY_HELPER" dir "$META_FILE" "$REPO_ROOT" "$1"
}

# resolve_crate <name-or-dir> — print the package name for either a crates.io
# package name (`tga`) or a crates/ directory name (`trusty-git-analytics`).
#
# Why: the release-time caller has a git tag, and a tag prefix is whichever of
# the two the tagger used — `tga-v1.4.2` and `trusty-git-analytics-v1.4.2` are
# both accepted tags for the same crate (#1128). Accepting both here mirrors
# preflight-publish.sh's resolver so this workspace keeps ONE lookup convention.
resolve_crate() {
  local want="$1" name
  if [[ "$(python3 "$PY_HELPER" exists "$META_FILE" "$want")" == "yes" ]]; then
    echo "$want"
    return 0
  fi
  name="$(dir_to_crate "$want")"
  if [[ -n "$name" ]]; then
    echo "$name"
    return 0
  fi
  echo "FAIL: '${want}' is neither a workspace package name nor a crates/ directory." >&2
  return "$EXIT_NO_VERDICT"
}

# require_tool — cargo-semver-checks must be installed, and its absence is a
# HARD FAIL with a remedy, never a skip. This is the gate's last fail-open
# surface on the release path: preflight-publish.sh runs it as the final barrier
# before `cargo publish`, so "the tool wasn't there" reporting green would put
# the repo back in the state that yanked trusty-analyze 0.7.3 (#4088).
require_tool() {
  if cargo semver-checks --version > /dev/null 2>&1; then
    return 0
  fi
  echo "FAIL: TOOL ERROR — 'cargo semver-checks' is not installed." >&2
  echo "      Install it before publishing:" >&2
  echo "        cargo install cargo-semver-checks@0.50.0 --locked" >&2
  echo "      A missing tool is NOT a pass (issue #5050)." >&2
  return "$EXIT_NO_VERDICT"
}

# ---------------------------------------------------------------------------
# select_toolchain — put the newest installed rustc/cargo on PATH for the
# duration of this run, and report which one the check will use.
#
# Why: cargo-semver-checks builds rustdoc in a scratch project that ignores this
#   workspace's Cargo.lock (see "Toolchain" in the header). The re-resolved graph
#   can therefore demand a HIGHER rustc than the repo's MSRV, and did: `takecell`
#   0.1.2 requires 1.96, which killed the whole gate under the pinned 1.94.1.
#   Pinning the offending transitive dependency instead would fix one name until
#   the next dependency raises its floor — a treadmill, not a fix.
#
#   This costs no coverage. The gate compares the PUBLIC API of our source
#   against the registry; which rustc renders the rustdoc JSON does not change
#   what that API is. MSRV compliance is a different question answered by a
#   different CI job (dtolnay/rust-toolchain@1.94), and nothing here weakens it.
#
#   It also cannot manufacture a pass, which is the property that matters. The
#   only thing a wrong toolchain choice can do is fail the rustdoc build, and
#   since #5289 that is NO VERDICT and exits non-zero. A machine with no newer
#   toolchain installed keeps the ambient one and degrades to exactly that
#   honest path — never to a silent green.
#
# What: honours SEMVER_GATE_TOOLCHAIN_BIN when it is set (empty disables the
#   search and keeps the ambient toolchain); otherwise scans
#   $RUSTUP_HOME/toolchains/*/bin for the highest `rustc -V` and prepends it when
#   it beats the active one. RUSTUP_TOOLCHAIN is deliberately NOT used: where
#   rustc/cargo resolve through mise shims, the shim execs a pinned toolchain
#   directly and never consults rustup, so the variable is silently ignored.
#   Prepending the toolchain's own bin directory is the mechanism that works
#   under both mise shims and a plain rustup install.
# ---------------------------------------------------------------------------
select_toolchain() {
  local active best_dir

  active="$(rustc -V 2>/dev/null | awk '{print $2}')"
  active="${active:-unknown}"

  if [[ -n "${SEMVER_GATE_TOOLCHAIN_BIN+x}" ]]; then
    if [[ -z "$SEMVER_GATE_TOOLCHAIN_BIN" ]]; then
      echo "toolchain: ambient rustc ${active} (selection disabled by SEMVER_GATE_TOOLCHAIN_BIN=)"
      return 0
    fi
    PATH="${SEMVER_GATE_TOOLCHAIN_BIN}:${PATH}"
    export PATH
    echo "toolchain: rustc $(rustc -V 2>/dev/null | awk '{print $2}') (pinned by SEMVER_GATE_TOOLCHAIN_BIN)"
    return 0
  fi

  best_dir="$(python3 - "${RUSTUP_HOME:-${HOME}/.rustup}/toolchains" "$active" <<'PY'
import os, sys, subprocess

root, active = sys.argv[1], sys.argv[2]

def key(v):
    parts = (v.split("-")[0].split(".") + ["0", "0", "0"])[:3]
    try:
        return tuple(int(p) for p in parts)
    except ValueError:
        return (0, 0, 0)

best, best_dir = key(active), ""
try:
    names = sorted(os.listdir(root))
except OSError:
    names = []
for name in names:
    binp = os.path.join(root, name, "bin")
    rustc = os.path.join(binp, "rustc")
    if not (os.path.isfile(rustc) and os.access(rustc, os.X_OK)):
        continue
    if not os.path.isfile(os.path.join(binp, "cargo")):
        continue  # rustdoc builds need cargo from the same directory
    try:
        out = subprocess.run([rustc, "-V"], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        continue
    if out.returncode != 0 or len(out.stdout.split()) < 2:
        continue
    v = key(out.stdout.split()[1])
    if v > best:
        best, best_dir = v, binp
print(best_dir)
PY
  )" || best_dir=""

  if [[ -n "$best_dir" ]]; then
    PATH="${best_dir}:${PATH}"
    export PATH
    echo "toolchain: rustc $(rustc -V 2>/dev/null | awk '{print $2}') (selected over ambient ${active}; the scratch resolution ignores Cargo.lock)"
  else
    echo "toolchain: ambient rustc ${active} (no newer toolchain installed)"
  fi
}

# ---------------------------------------------------------------------------
# verdict_computed <output-file> — succeed only when cargo-semver-checks printed
# its own per-crate check summary AND that summary says it actually EXECUTED at
# least one check, e.g.
#     Checked [   0.388s] 223 checks: 214 pass, 9 fail, 0 warn, 31 skip   <- verdict
#     Checked [   0.000s] 0 checks: 0 pass, 254 skip                      <- NOT one
#
# ZERO CHECKS EXECUTED IS NOT A PASS (issue #5440). The leading `N checks:` count
#   is the number of lints the tool RAN; the trailing `N skip` is what it declined
#   to run. When the version delta is one that permits breakage — a major bump
#   under Cargo's rules, which for a 0.y.z crate is any change to the MINOR
#   position — every lint is skipped, the tool prints
#
#       Checked [   0.000s] 0 checks: 0 pass, 254 skip
#       Summary no semver update required
#
#   and exits 0. Until this fix the presence of that summary line WAS the verdict
#   test, so the gate read "the tool declined to check anything" as "the tool
#   found nothing wrong" and preflight-publish.sh CHECK 5 passed having verified
#   nothing. Measured on trusty-common under rustc 1.94.1: 0 of 254 checks run,
#   gate exit 0. It failed closed on this machine only by accident — under the
#   newest installed toolchain the same crate crashed rustdoc, printed no summary
#   at all, and was correctly classified NO VERDICT. Removing that toolchain
#   would have turned the gate green for every publish.
#
#   Same family as `cargo test -- <name> --exact` printing `running 0 tests` and
#   exiting 0: work not done must never read as work passed. So the count is
#   asserted, not merely the marker. A summary whose count does not parse is also
#   NO VERDICT — this fails closed, like every other branch here.
#
# Why a MARKER and not the exit status: the tool's statuses are not a contract
#   the gate can lean on, and observation shows they collide. 101 is a rustdoc
#   build failure AND a "crate not found in registry" baseline failure; 100 is a
#   real break; 0 is clean. Keying on the status alone would still mix a
#   non-verdict in with a verdict. The summary line is the tool's own statement
#   that it finished comparing, so requiring it means the gate concludes only
#   from evidence that the comparison happened.
#
# THE MARKER ARRIVES COLOURED IN CI, AND THE COLOUR LANDS MID-TOKEN (issue #5500).
#   `dtolnay/rust-toolchain` writes CARGO_TERM_COLOR=always into $GITHUB_ENV
#   when it is unset, so every step after it inherits forced colour even though
#   this gate redirects the tool to a file. cargo-semver-checks then emits
#
#       ESC[1m ESC[32m     Checked ESC[0m [   0.871s] 196 checks: 196 pass, ...
#
#   with the reset sequence BETWEEN `Checked` and the space that follows it. A
#   pattern anchored on `Checked<space>` therefore matches nothing, and a run
#   that compared 196 checks is announced as one that never ran. Both halves of
#   PR #5458 are that single defect: tga exited 0 having found no break, and
#   trusty-review exited 100 having listed four real ones, and the gate called
#   both "without completing a run". No exit status was misread — the exit-status
#   handling below is correct and unchanged; only the evidence test was blind.
#
#   The escapes are therefore stripped before matching. Stripping is done on a
#   COPY: the log echoed to the operator keeps its colour, and the regex keeps
#   the exact shape #5289 gave it.
#
#   The strip covers the WHOLE ECMA-48 CSI grammar, not just the SGR colour
#   sequences observed: ESC [, parameter bytes 0x30-0x3F (`0-9:;<=>?`),
#   intermediate bytes 0x20-0x2F (` -/`), one final byte 0x40-0x7E (`@-~`).
#   Narrowing it to `[0-9;]*[A-Za-z]` — the observed shape — would leave a
#   private-mode sequence like `ESC[?25l` (cursor hide, emitted by spinner
#   renderers) unstripped, and that is the same defect again: an escape between
#   `Checked` and its space. Pinned by self-test case 19.
#
#   `[ -/]`, NOT `[ -\/]`. POSIX makes the backslash literal inside a bracket
#   expression, so `[ -\/]` is the range 0x20-0x5C and deletes digits and
#   uppercase letters: `KEEP-ME-A` strips to the empty string under it, and to
#   `KEEPMEA` under the correct form. An over-wide strip is the one way this
#   function could invent a marker rather than miss one.
#
#   Non-CSI escape families (OSC hyperlinks, two-byte sequences) are out of
#   scope deliberately: none appears in cargo-semver-checks 0.50.0's output,
#   and an unhandled one fails CLOSED to NO VERDICT rather than to a pass.
#
#   Written to a file rather than piped into `grep -q`, because `grep -q` exits
#   at the first match and SIGPIPEs the producer — under `set -o pipefail` that
#   turns a FOUND marker into a non-zero pipeline, i.e. a verdict reported as a
#   non-verdict. That is the same lie in the other direction.
#
# Fails CLOSED by construction: if a future cargo-semver-checks renames this
#   line, every run becomes NO VERDICT — loud and non-zero — rather than a
#   silent green. That is the correct direction for the failure to point. The
#   strip cannot manufacture a marker either: it only deletes escape sequences,
#   so text that did not say `Checked … N checks:` still does not.
# ---------------------------------------------------------------------------
# Set by verdict_computed on every refusal, read by no_verdict_because. Three
# shapes: `no-summary`, `zero-checks:<the line>`, `unparsable-count:<the line>`.
VERDICT_REASON=""

# strip_ansi_to <src> <dst> — write <src> to <dst> with every ECMA-48 CSI
# sequence removed. Two readers need the same plain-text copy (verdict_computed
# and scratch_resolution_report, #6740), and a second spelling of this regex is
# a second thing to get wrong: #5500 was one escape sequence in the wrong place.
strip_ansi_to() {
  LC_ALL=C sed -E $'s/\033\\[[0-9:;<=>?]*[ -/]*[@-~]//g' "$1" > "$2"
}

verdict_computed() {
  # #5500: CARGO_TERM_COLOR=always splits `Checked` from its trailing space (PR #5458).
  local plain="${1}.plain" summary executed
  VERDICT_REASON="no-summary"
  strip_ansi_to "$1" "$plain" || return 1

  # `tail -1` and not `grep -q`: the header explains why a short-circuiting
  # reader is wrong here, and the LAST summary is the conservative one to judge.
  summary="$(grep -E '(^|[[:space:]])Checked[[:space:]].*[0-9]+ checks:' "$plain" | tail -1 || true)"
  [[ -z "$summary" ]] && return 1

  # #5440: assert on the CHECK COUNT, not merely on the summary's presence.
  executed="$(printf '%s\n' "$summary" | sed -E 's/.*[^0-9]([0-9]+) checks:.*/\1/')"
  if ! printf '%s\n' "$executed" | grep -Eq '^[0-9]+$'; then
    VERDICT_REASON="unparsable-count:${summary}"
    return 1
  fi
  if [[ "$executed" -lt 1 ]]; then
    VERDICT_REASON="zero-checks:${summary}"
    return 1
  fi
  VERDICT_REASON=""
  return 0
}

# emit_verdict_line <checked> <skipped> <inventoried> <blind> — restate the run's
# tallies in a fixed, machine-readable shape.
#
# Why: `.github/workflows/semver-checks.yml` has to tell "compared and found
#   nothing" apart from "declined to compare" and from "tried and could not", so
#   its GitHub check stops rendering all three as the same green
#   (issue #5688; PR #5686 shipped a real break under one of them). The gate
#   already holds those numbers; the workflow reading them out of the prose
#   SUMMARY line would be a SECOND parser of a format
#   `scripts/preflight-publish.sh` CHECK 5 already parses, free to drift from it.
#   This is the one that is meant to be parsed.
#
# What: one line on stdout, alongside the human summary and never instead of it.
#   `compared` is the number of crates whose API was actually examined — the
#   PASS/FAIL arm and the advisory INVENTORY arm both examine one — so a
#   consumer needs no knowledge of the arms to ask the only question that
#   matters. Deliberately carries neither `crate(s) checked` nor `N checks:`,
#   the substrings CHECK 5 and verdict_computed grep for, so adding it cannot
#   shift either of their answers.
#
# Test: `scripts/check_semver_selftest.sh` — the already-breaking, blind and
#   404-skip cases each assert the counts this line reports.
emit_verdict_line() {
  local checked="$1" skipped="$2" inventoried="$3" blind="$4"
  printf 'semver-gate-verdict: compared=%d examined=%d skipped=%d inventoried=%d blind=%d\n' \
    "$((checked + inventoried))" "$checked" "$skipped" "$inventoried" "$blind"
}

# no_verdict_because — one clause naming why verdict_computed refused, so the two
# causes never share a message. "it never ran" and "it ran and skipped every
# lint" have different remedies, and #5440 is the second one.
no_verdict_because() {
  case "${VERDICT_REASON%%:*}" in
    zero-checks)
      printf '%s\n' "completed a run that EXECUTED NO CHECKS — '$(printf '%s' "${VERDICT_REASON#*:}" | sed 's/^[[:space:]]*//')'"
      ;;
    unparsable-count)
      printf '%s\n' "printed a summary whose check count did not parse — '$(printf '%s' "${VERDICT_REASON#*:}" | sed 's/^[[:space:]]*//')'"
      ;;
    *)
      printf '%s\n' "never completed a check run"
      ;;
  esac
}

# ---------------------------------------------------------------------------
# lockfile_versions <package> — every version this workspace's Cargo.lock pins
# for <package>, comma-separated, or nothing when the lockfile does not name it.
# ---------------------------------------------------------------------------
lockfile_versions() {
  python3 - "${REPO_ROOT}/Cargo.lock" "$1" 2> /dev/null <<'PY' || true
import sys
want = sys.argv[2]
found, name = [], None
try:
    fh = open(sys.argv[1])
except OSError:
    raise SystemExit(0)
with fh:
    for line in fh:
        line = line.strip()
        if line.startswith('name = "') and line.endswith('"'):
            name = line[8:-1]
        elif line.startswith('version = "') and line.endswith('"') and name == want:
            found.append(line[11:-1])
            name = None
print(", ".join(sorted(set(found))))
PY
}

# ---------------------------------------------------------------------------
# scratch_resolution_report <crate> <run-log> — name the dependency that failed
# to compile, beside the version this workspace pins for it (#6740).
#
# Why: cargo-semver-checks builds in a scratch project whose Cargo.lock it
#   REGENERATES on every run, so the resolution ignores this workspace's pins and
#   takes the newest semver-compatible release of every transitive dependency.
#   When one of those releases does not compile, rustdoc dies before a single
#   lint runs and the gate reports NO VERDICT — with several hundred lines of
#   build log between the operator and the one package name that matters. #6740
#   is that case: tinyvec 1.13.0 calls `vec![]` while importing only the
#   `alloc::vec` MODULE, so it fails for every consumer of its `alloc` feature.
#   This workspace pins 1.11.0 and builds clean, which is why the failure read as
#   unreproducible and sent the reporter after toolchains instead.
#
# What: prints the packages cargo reported as failing to compile, each with the
#   scratch version from the tool's own output and this workspace's pin. Prints
#   NOTHING when no package failed to compile, so the other NO VERDICT causes are
#   not handed a resolution story they do not have. Diagnosis only — it never
#   touches the verdict, the counters or the exit status.
#
# Test: `scripts/check_semver_selftest.sh` case 29.
# ---------------------------------------------------------------------------
scratch_resolution_report() {
  local crate="$1" log="$2" plain="${2}.plain" failed pkg scratch locked

  # verdict_computed writes the ANSI-stripped copy; regenerate it when that call
  # bailed before it could, because a coloured log hides these markers too.
  [[ -f "$plain" ]] || strip_ansi_to "$log" "$plain" || return 0

  failed="$(grep -oE 'could not compile `[^`]+`' "$plain" 2> /dev/null |
    sed -E 's/^could not compile `([^`]+)`$/\1/' | LC_ALL=C sort -u || true)"
  [[ -z "$failed" ]] && return 0

  {
    echo "SCRATCH RESOLUTION ${crate}: the scratch project cargo-semver-checks builds in"
    echo "           ignores this workspace's Cargo.lock and regenerates its own on every"
    echo "           run, so it takes the NEWEST semver-compatible release of each"
    echo "           transitive dependency. These failed to compile there:"
  } >&2
  while IFS= read -r pkg; do
    [[ -z "$pkg" ]] && continue
    scratch="$(grep -oE "(Checking|Compiling) ${pkg} v[0-9][^ ]*" "$plain" 2> /dev/null |
      tail -1 | sed -E 's/.* v//' || true)"
    locked="$(lockfile_versions "$pkg")"
    printf '             %s — scratch %s, this workspace pins %s\n' \
      "$pkg" "${scratch:-unknown}" "${locked:-nothing (not in Cargo.lock)}" >&2
  done <<<"$failed"
  {
    echo "           A dependency that is NEWER in the scratch than in Cargo.lock and does"
    echo "           not build is broken UPSTREAM, not here — this workspace compiles on its"
    echo "           pin. A newer toolchain does not fix that, and seeding the scratch"
    echo "           lockfile does not stick: the tool rewrites it before it builds."
    echo "           docs/reference/semver-gate.md names what a release may do about it."
    echo "           Nothing was compared either way, so this stays NO VERDICT."
  } >&2
}

# ---------------------------------------------------------------------------
# --probe: report the baseline decision for one crate and stop. Exists so the
# self-test can drive the registry probe — the gate's only fail-open surface —
# without a Cargo build, and so a human can ask "what would you compare against?"
# ---------------------------------------------------------------------------
if [[ -n "$PROBE_ONLY" ]]; then
  # The declared version is looked up BEST-EFFORT: --probe accepts a crate this
  # workspace does not build (that is how the self-test probes an unpublished
  # name), and an empty version simply means "no upper bound".
  PROBE_VERSION="${PROBE_VERSION_OVERRIDE:-$(python3 "$PY_HELPER" field "$META_FILE" "$PROBE_ONLY" version 2>/dev/null || true)}"

  # `registry_probe` runs in a command substitution, i.e. a SUBSHELL, so its
  # `exit 1` cannot terminate this script on its own — it only ends the subshell.
  # Every caller must re-raise it explicitly. Leaving that to `set -e` is exactly
  # the fail-open shape this gate exists to avoid.
  if ! probe="$(registry_probe "$PROBE_ONLY" "$PROBE_VERSION")"; then
    exit "$EXIT_NO_VERDICT"
  fi
  latest="$(printf '%s\n' "$probe" | sed -n 1p)"
  baseline="$(printf '%s\n' "$probe" | sed -n 2p)"
  pre_below="$(printf '%s\n' "$probe" | sed -n 3p)"
  echo "probe ${PROBE_ONLY}: baseline=${baseline} (declared=${PROBE_VERSION:-unknown}, latest on crates.io=${latest}, pre-release below=${pre_below})"
  exit 0
fi

# ---------------------------------------------------------------------------
# Candidate selection.
# ---------------------------------------------------------------------------
CANDIDATES=""

if [[ -n "$EXPLICIT_CRATES" ]]; then
  while IFS= read -r want; do
    [[ -z "$want" ]] && continue
    if ! name="$(resolve_crate "$want")"; then
      exit "$EXIT_NO_VERDICT"
    fi
    CANDIDATES="${CANDIDATES}${name}"$'\n'
  done <<<"$(printf '%s' "$EXPLICIT_CRATES" | grep -v '^$')"
  CANDIDATES="$(printf '%s' "$CANDIDATES" | grep -v '^$' | LC_ALL=C sort -u)"
  SCANNED="(explicit)"
else
  if ! MERGE_BASE="$(git merge-base "$BASE" HEAD 2>/dev/null)"; then
    echo "FAIL: TOOL ERROR — cannot find a merge base between '${BASE}' and HEAD." >&2
    echo "      Fetch the base ref first (CI must check out with fetch-depth: 0):" >&2
    echo "        git fetch origin main" >&2
    exit "$EXIT_NO_VERDICT"
  fi

  CHANGED="$(git diff --name-only --no-renames "$MERGE_BASE" HEAD)"
  CHANGED_COUNT="$(printf '%s\n' "$CHANGED" | grep -c '[^[:space:]]' || true)"

  # Scan floor (#4618 shape). An empty diff means the base ref is wrong or the
  # checkout is shallow — never that the PR is clean. A gate that scanned nothing
  # has not passed.
  if [[ "${CHANGED_COUNT:-0}" -lt 1 ]]; then
    echo "FAIL: SCAN FLOOR — the diff ${MERGE_BASE}..HEAD lists 0 changed path(s)." >&2
    echo "      Nothing was examined, so this gate could not have failed. Check that" >&2
    echo "      '${BASE}' is the right base and that CI checked out with fetch-depth: 0." >&2
    exit "$EXIT_NO_VERDICT"
  fi
  SCANNED="${CHANGED_COUNT} changed path(s)"

  dirs=""
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    case "$path" in
      crates/*/src/*)
        d="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/src/.*#\1#')"
        [[ "$d" == */* ]] && continue
        dirs="${dirs}${d}"$'\n'
        ;;
    esac
  done <<<"$CHANGED"

  dirs="$(printf '%s' "$dirs" | grep -v '^$' | LC_ALL=C sort -u || true)"
  while IFS= read -r d; do
    [[ -z "$d" ]] && continue
    name="$(dir_to_crate "$d")"
    # A directory with no package at HEAD was deleted by this PR; there is no
    # API left to compare. Same exemption shape as check_changelog_fragment.sh.
    [[ -z "$name" ]] && {
      echo "SKIP crates/${d}: no package at HEAD (crate removed by this PR)"
      continue
    }
    CANDIDATES="${CANDIDATES}${name}"$'\n'
  done <<<"$dirs"
  CANDIDATES="$(printf '%s' "$CANDIDATES" | grep -v '^$' | LC_ALL=C sort -u || true)"
fi

if [[ -z "$CANDIDATES" ]]; then
  echo "semver gate: scanned ${SCANNED}; no crate source changed (docs-only / CI-only) — OK."
  emit_verdict_line 0 0 0 0
  exit 0
fi

# ---------------------------------------------------------------------------
# Per-crate check.
# ---------------------------------------------------------------------------
checked=0
skipped=0
fail=0
noverdict=0
# Advisory tallies (#5297b) — reported, never added to `fail`. `inventory_blind`
# exists so an inventory that could not run is visible in the summary line
# instead of vanishing into the same number as one that ran clean.
inventoried=0
inventory_blind=0

# Selected once, not per crate: the PATH edit is process-wide and re-running the
# scan for every candidate would just re-answer the same question.
select_toolchain

# Resolved once for the same reason, and printed for the same reason the
# toolchain line is printed: which environment a release-blocking gate ran under
# is part of its result. Empty means no sccache, and no sccache means the run is
# byte-for-byte the one this gate made before the knob existed.
BUILD_ACCEL_SCCACHE="$(build_accel_sccache)"
build_accel_mode_line "$BUILD_ACCEL_SCCACHE"

# ---------------------------------------------------------------------------
# semver_checks_run <args...> — `cargo semver-checks <args>` under the resolved
# acceleration environment.
#
# Why a wrapper rather than an inline env prefix: the two call sites below must
# not drift, and `${VAR:+NAME="$VAR"}` word-splits a path containing a space,
# which an sccache installed under a home directory can carry. Building the
# prefix as an `env` argument vector quotes each assignment exactly once.
#
# Why `env` and not `export`: the assignment reaches THIS subprocess and nothing
# else. Nothing the preflight spawns afterwards — and nothing a human runs after
# the preflight passes, `cargo publish` included — inherits RUSTC_WRAPPER from
# here. The acceleration is scoped to the one command it was reasoned about.
#
# CARGO_TARGET_DIR IS UNSET, NOT MERELY UNSET-BY-US. Setting it relocates the
# current crate's rustdoc JSON to a flat, name-keyed path and blinds
# check_semver_types.sh — the measurement is in scripts/lib/build_accel.sh — and
# an AMBIENT one does that just as thoroughly as one this script added. This repo
# sanctions exporting a shared CARGO_TARGET_DIR (DOC-66 §6.2/§6.4 lists it as an
# opt-in disk-saving strategy; vmtest-harness/lib/provision.sh exports one into
# cargo's children), so an inherited value is a real configuration, not a
# hypothetical. `env -u` neutralises it for this subprocess only and leaves the
# caller's environment alone.
#
# `env -u`, NOT `CARGO_TARGET_DIR=`. An empty value is not "use the default" to
# cargo — it is a hard error:
#     error: the target directory is set to an empty string in the
#            `CARGO_TARGET_DIR` environment variable
# (verified against `cargo metadata`, exit 101). Setting it empty would turn an
# ambient variable into a failed gate.
#
# The vector always carries `env -u CARGO_TARGET_DIR SKIP_UI_BUILD=1`, so it is
# never empty and `"${envv[@]}"` is safe under `set -u` on bash 3.2.
# ---------------------------------------------------------------------------
semver_checks_run() {
  local -a envv
  envv=(env -u CARGO_TARGET_DIR SKIP_UI_BUILD=1)
  if [[ -n "$BUILD_ACCEL_SCCACHE" ]]; then
    envv+=("RUSTC_WRAPPER=${BUILD_ACCEL_SCCACHE}")
  fi
  "${envv[@]}" cargo semver-checks "$@"
}

while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue

  # Crate exclusion. This gate protects LIBRARY consumers — a dependent that
  # re-resolves a version floor on a lockfile-free `cargo install` and stops
  # compiling (#4088). A crate nothing links as a library has nobody to protect:
  # trusty-mpm ships as the `tm` executable, and a binary user gets a whole new
  # executable, so its library API compatibility is not part of what they get.
  #
  # That is a claim about consumption, not about the API, and a skip list is the
  # exact shape that stops protecting SILENTLY once the claim goes stale. So the
  # premise is re-checked here instead of trusted: a workspace crate that depends
  # on the excluded one REFUSES the skip. Nothing was compared, so this is a
  # non-verdict rather than a break — preflight-publish.sh CHECK 5 reports it as
  # "the gate could not run" and the publish still stops.
  excl_reason="$(crate_exclusion_reason "$crate")"
  if [[ -n "$excl_reason" ]]; then
    if ! dependents="$(python3 "$PY_HELPER" dependents "$META_FILE" "$crate")"; then
      echo "FAIL: TOOL ERROR — could not list workspace dependents of '${crate}'." >&2
      exit "$EXIT_NO_VERDICT"
    fi
    if [[ -n "$dependents" ]]; then
      {
        echo "FAIL: EXCLUSION NO LONGER HOLDS — ${crate} is excluded from the SemVer gate"
        echo "      because nothing depends on its library, but these workspace crates now do:"
        printf '%s\n' "$dependents" | sed 's/^/        - /'
        echo "      An exclusion that has stopped being true is a silent coverage hole, so the"
        echo "      skip is REFUSED and NOTHING was compared. Delete the '${crate}' row from"
        echo "      ${CRATE_EXCLUSIONS_REL} to restore full gating."
      } >&2
      exit "$EXIT_NO_VERDICT"
    fi
    echo "SKIP ${crate}: excluded by ${CRATE_EXCLUSIONS_REL} — ${excl_reason}"
    skipped=$((skipped + 1))
    continue
  fi

  if [[ "$(pkg_field "$crate" publishable)" == "no" ]]; then
    echo "SKIP ${crate}: publish = false — never reaches crates.io"
    skipped=$((skipped + 1))
    continue
  fi

  if [[ "$(pkg_field "$crate" has_lib)" == "no" ]]; then
    echo "SKIP ${crate}: no library target — no public API surface to compare"
    skipped=$((skipped + 1))
    continue
  fi

  current="$(pkg_field "$crate" version)" || exit "$EXIT_NO_VERDICT"

  # Subshell re-raise, as at --probe above: a helper's `exit 1` dies with the
  # command substitution unless the caller checks for it.
  if ! probe="$(registry_probe "$crate" "$current")"; then
    exit "$EXIT_NO_VERDICT"
  fi
  latest="$(printf '%s\n' "$probe" | sed -n 1p)"
  baseline="$(printf '%s\n' "$probe" | sed -n 2p)"
  pre_below="$(printf '%s\n' "$probe" | sed -n 3p)"

  if [[ "$latest" == "NONE" ]]; then
    echo "SKIP ${crate} v${current}: no installable release on crates.io — no baseline exists"
    skipped=$((skipped + 1))
    continue
  fi

  # #5296: the baseline is the greatest STABLE release below the declared
  # version, so this branch means the crate has no stable predecessor — its
  # first real release. That is a fact about the crate, and the only honest
  # comparison (there is no earlier API a dependent could be on) is none at all.
  # The `latest` value is printed because it is what separates this from a
  # version regression: if it is ABOVE the declared version, the crate is behind
  # the registry and preflight-publish.sh CHECK 4 is the check that says so.
  if [[ "$baseline" == "NONE" ]]; then
    if [[ "$pre_below" != "NONE" ]]; then
      # Never silent about a rejection. A pre-release IS below the declared
      # version and was still not used, so the reason has to be on the line.
      echo "SKIP ${crate} v${current}: no STABLE release below v${current} — v${pre_below} is below it but a pre-release is never a baseline (nothing resolves to it); latest on crates.io is v${latest}"
    else
      echo "SKIP ${crate} v${current}: no non-yanked release below v${current} (latest on crates.io is v${latest}) — no previous release to compare against"
    fi
    skipped=$((skipped + 1))
    continue
  fi

  if ! rtype="$(release_type "$baseline" "$current")"; then
    echo "FAIL: TOOL ERROR — could not compare '${baseline}' and '${current}' for ${crate}." >&2
    exit "$EXIT_NO_VERDICT"
  fi

  # Checked lazily — a run whose every candidate skipped never needed the tool,
  # and demanding it there would be friction with no coverage behind it.
  if [[ -z "${TOOL_OK:-}" ]]; then
    require_tool || exit "$EXIT_NO_VERDICT"
    TOOL_OK=1
  fi

  if ! feature_args "$crate" > "${SCRATCH}/features.txt"; then
    exit "$EXIT_NO_VERDICT"
  fi

  # shellcheck disable=SC2046
  set -- $(cat "${SCRATCH}/features.txt")

  # -------------------------------------------------------------------------
  # INVENTORY arm (#5297b). The bump is already breaking, so no verdict is owed
  # and none is produced — but the run happens anyway, with --release-type minor
  # forcing the breaking-change lints to apply, so a human gets the list of what
  # this release actually breaks. ADVISORY BY CONSTRUCTION: nothing in this
  # branch touches `fail` or `noverdict`, because a major release is entitled to
  # break its API and a gate that reddened over a permitted break would be noise.
  # -------------------------------------------------------------------------
  if [[ "$rtype" == "major" ]]; then
    echo "INVENTORY ${crate}: ${baseline} -> ${current} is already a breaking release — no bump can be owed, so this run is advisory: what does it break?"
    rc=0
    RUN_LOG="${SCRATCH}/inventory-${crate}.txt"
    semver_checks_run \
      --package "$crate" \
      --baseline-version "$baseline" \
      --release-type minor \
      --only-explicit-features "$@" > "$RUN_LOG" 2>&1 || rc=$?
    cat "$RUN_LOG"

    if ! verdict_computed "$RUN_LOG"; then
      # Loud, and counted. The release still proceeds — the permitted-break fact
      # is decided by the version numbers, not by this run — but "no inventory"
      # must never read as "inventory clean".
      echo "NO INVENTORY ${crate}: cargo semver-checks exited ${rc} and $(no_verdict_because)." >&2
      echo "             The release is still permitted (${baseline} -> ${current} already carries the break)," >&2
      echo "             but WHAT it breaks is unknown. Fix the run above to get the list." >&2
      # #6740: the inventory arm hits the same broken-dependency wall.
      scratch_resolution_report "$crate" "$RUN_LOG"
      inventory_blind=$((inventory_blind + 1))
    elif [[ "$rc" -ne 0 ]]; then
      echo "INVENTORY ${crate}: breaking change(s) listed above — permitted by the ${rtype} bump. Confirm every one was intended (#5297)."
      inventoried=$((inventoried + 1))
    else
      echo "INVENTORY ${crate}: no breaking changes found against v${baseline}."
      inventoried=$((inventoried + 1))
    fi
    continue
  fi

  echo "CHECK ${crate}: ${baseline} -> ${current} (${rtype} release), $(($# / 2)) feature(s)"

  # Captured rather than streamed so the run can be CLASSIFIED afterwards
  # (#5289); echoed straight back so a human still sees every lint the tool
  # printed, and so preflight-publish.sh's captured log is unchanged in content.
  rc=0
  RUN_LOG="${SCRATCH}/run-${crate}.txt"
  semver_checks_run \
    --package "$crate" \
    --baseline-version "$baseline" \
    --only-explicit-features "$@" > "$RUN_LOG" 2>&1 || rc=$?
  cat "$RUN_LOG"

  # Order matters: "did it compare anything?" is asked BEFORE "what did it say?".
  # An exit 0 with no summary line is a no-op, not a pass, and must not reach the
  # clean branch — that is the fail-closed half of this check.
  if ! verdict_computed "$RUN_LOG"; then
    echo "NO VERDICT ${crate}: cargo semver-checks exited ${rc} and $(no_verdict_because)." >&2
    echo "           No public API comparison against baseline ${baseline} was performed." >&2
    # #6740: name the dependency that would not build before the generic help
    # block sends the reader after a toolchain that is not the problem.
    scratch_resolution_report "$crate" "$RUN_LOG"
    noverdict=$((noverdict + 1))
    continue
  fi

  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL ${crate}: cargo semver-checks exited ${rc} against baseline ${baseline}" >&2
    fail=1
  fi
  checked=$((checked + 1))
done <<<"$CANDIDATES"

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<'EOF'

VERDICT: BREAK. A public API change requires a matching version bump.

  0.x crates: a break needs the MINOR position (0.28.1 -> 0.29.0).
  1.x+ crates: a break needs the MAJOR position (1.3.4 -> 2.0.0).

Either bump the version in the crate's Cargo.toml, or make the change
non-breaking. `#[non_exhaustive]` on a struct/enum makes future field and
variant additions non-breaking by construction — the fix #4088 asked for.

Explain any lint:  cargo semver-checks --explain <lint_name>

This is not advisory. #4088: trusty-common 0.22.5 shipped exactly this class of
break as a patch bump and bricked `cargo install` for every dependent on a
^0.22 floor, costing trusty-analyze 0.7.3 a yank.
EOF
  exit "$EXIT_SEMVER_BREAK"
fi

# Reported AFTER the break block so a run that hit both says both, and exits on
# the non-verdict — an incomplete run is the one result that must never be read
# as a conclusion about the API.
if [[ "$noverdict" -ne 0 ]]; then
  cat >&2 <<'EOF'

NO SEMVER VERDICT WAS COMPUTED. This is NOT a pass, and it is NOT a report that
the API changed — cargo-semver-checks never finished comparing, so nothing is
known either way about this crate's public API.

The tool's own output is above. The usual causes:

  * IT RAN AND CHECKED NOTHING — `0 checks: 0 pass, N skip` (issue #5440). The
    tool skips its whole lint set when IT reads the baseline -> current delta as
    already permitting breakage. Exit 0 and "no semver update required" there
    mean "nothing was examined", not "nothing is wrong".
    A NORMAL 0.x MINOR BUMP DOES NOT LAND HERE. The gate classifies 0.28.1 ->
    0.29.0 as major itself and sends it to the advisory INVENTORY arm, which
    passes --release-type minor and gets a full lint run. Do not weaken this
    check on the theory that a routine bump tripped it.
    What reaches this arm is a BASELINE AT OR ABOVE the declared version: the
    gate reads that pair as no change while the tool reads it by position and
    calls it a permitted break. Read the baseline off the `CHECK` line above. If
    it is not below the version you are publishing, the declared version is
    behind the registry — preflight-publish.sh CHECK 4 is the check that says
    so — and that is what to fix.
  * rustdoc failed to build. cargo-semver-checks resolves dependencies in a
    scratch project that IGNORES this workspace's Cargo.lock, so it can pick a
    newer transitive dependency whose `rust-version` exceeds the rustc running
    the gate. The `toolchain:` line above says which rustc that was. Install a
    newer one (`rustup toolchain install stable`), or point the gate at one:
        SEMVER_GATE_TOOLCHAIN_BIN=<dir> bash scripts/check_semver.sh --crate <c>
    A NEWER TOOLCHAIN IS THE WRONG MOVE WHEN THE RELEASE SIMPLY DOES NOT COMPILE
    (#6740). The scratch resolution takes the newest semver-compatible release of
    every transitive dependency, so a broken upstream release reaches the gate
    while this workspace keeps building on its Cargo.lock pin. The SCRATCH
    RESOLUTION block above names the package and both versions when that is what
    happened; there is no toolchain that fixes it.
  * A NATIVE LIBRARY THE FEATURE SET NEEDS IS ABSENT (issue #5440). The gate
    builds with --only-explicit-features, so every non-excluded feature is on
    and its build scripts run. On Linux, trusty-common's `keyring-store` pulls
    libdbus-sys, whose build.rs shells out to `pkg-config dbus-1` and panics
    when the headers are missing. The error text names the pkg-config package,
    not the cargo feature. Remedy on Debian/Ubuntu:
        sudo apt-get install -y libdbus-1-dev
    macOS needs nothing here — keyring resolves to security-framework there and
    libdbus-sys is not in the graph at all. CI installs the package in
    .github/workflows/semver-checks.yml.
  * the baseline could not be fetched from crates.io.
  * a genuine compile error in the crate — build it directly to see it.

Fix the gate, then re-run it. Do not publish on the strength of this output:
"the check could not run" is not "the check passed" (issue #5289).
EOF
  exit "$EXIT_NO_VERDICT"
fi

SUMMARY="semver gate: scanned ${SCANNED}; ${checked} crate(s) checked, ${skipped} skipped"
if [[ "$inventoried" -ne 0 ]]; then
  SUMMARY="${SUMMARY}, ${inventoried} inventoried (advisory)"
fi
if [[ "$inventory_blind" -ne 0 ]]; then
  SUMMARY="${SUMMARY}, ${inventory_blind} inventory NOT computed"
fi
echo "${SUMMARY} — OK."
emit_verdict_line "$checked" "$skipped" "$inventoried" "$inventory_blind"
