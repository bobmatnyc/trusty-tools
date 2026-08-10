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
# What: for one or more crates, resolves the latest non-yanked version on
#   crates.io and runs `cargo semver-checks` against it. A crate needs a check
#   only when its declared version does not already carry a breaking bump; see
#   "Already-breaking" below. Crate selection is either explicit (`--crate`) or,
#   with no arguments, every crate whose `crates/<crate>/src/**` changed against
#   a base ref.
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
#     - `publish = false`            -> skip (never reaches crates.io)
#     - no library target            -> skip (a bin-only crate has no API surface
#                                       cargo-semver-checks can compare)
#     - registry says 404            -> skip (never published; no baseline exists)
#     - registry has only yanked vers-> skip (no installable baseline)
#     - registry probe fails any
#       other way (network, 5xx,
#       malformed index, curl error) -> HARD FAIL, exit non-zero
#   The last line is the point. This repo's recurring defect shape is a failure
#   branch that advances state anyway ("fail-open / cursor-advance"), and a
#   SemVer gate that reports green because it could not reach crates.io is worse
#   than no gate at all. Every skip above is a fact about the crate; a probe
#   error is a fact about the GATE, and the gate does not get to excuse itself.
#
# Already-breaking: when the declared version is already a major bump over the
#   baseline (0.28.1 -> 0.29.0 is major under Cargo's 0.x rules, as is
#   1.3.4 -> 2.0.0), cargo-semver-checks itself runs ZERO lints and exits 0 —
#   there is no rule left to break. Observed directly on trusty-common 0.28.1 ->
#   0.29.0: "0 checks: 0 pass, 254 skip". So the gate reaches the same verdict by
#   comparing the versions, and does not spend ~4 minutes of CI building two
#   rustdoc trees to be told nothing applies. This is a cost cut, not a coverage
#   cut: the skipped run had no coverage to give.
#
# A BUILD FAILURE IS NOT A VERDICT (issue #5289). cargo-semver-checks has to
#   build rustdoc for both sides before it can compare anything, and that build
#   can fail on its own — a broken dependency graph, an MSRV floor a dependency
#   raised, a baseline the registry would not hand over. Until #5289 every one of
#   those exited non-zero and fell straight into the "a public API change
#   requires a matching version bump" remediation below, so a rustdoc error was
#   presented as a SemVer verdict and the two were indistinguishable from the
#   output. The gate now classifies on POSITIVE EVIDENCE instead: a verdict
#   exists only when the tool printed its own per-crate `N checks:` summary line.
#   No summary line means no comparison happened, whatever the exit status was —
#   including exit 0 — and that is reported as NO VERDICT on its own exit code.
#   Absence of evidence is never a pass; same rule as the scan floor below.
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
#
# Exit (#5289 split 1 from 3 — a verdict and a non-verdict are different facts):
#   0  every checked crate is SemVer-clean, or is a recorded skip.
#   1  A VERDICT WAS COMPUTED AND IT SAYS BREAK — a crate needs a bump it does
#      not have. This is the only status that means "the API changed".
#   2  usage error.
#   3  NO VERDICT — the gate could not do its job: cargo-semver-checks never
#      completed a check run (rustdoc build failure, baseline retrieval
#      failure), the diff scanned nothing, the registry was unreachable, or the
#      tool is missing. Nothing was compared, so nothing may be concluded.
#
# Test: `scripts/check_semver_selftest.sh` proves every way this gate could lie
#   — a vacuous scan, an unreachable registry, and (since #5289) a rustdoc build
#   failure or a silent no-op rendered as a SemVer verdict — all exit non-zero,
#   and that a real break is still reported as a break. The catch itself is
#   demonstrated in PR #5051 against #4088's real shape.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   plus `git`, `curl`, `cargo`, `python3` (JSON parsing only).

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

BASE="${SEMVER_GATE_BASE:-origin/main}"
EXPLICIT_CRATES=""
PROBE_ONLY=""
INDEX_BASE="${SEMVER_GATE_INDEX_BASE:-https://index.crates.io}"
EXCLUSIONS_FILE="${REPO_ROOT}/scripts/semver-checks-feature-exclusions.tsv"

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
# registry_latest <crate> — print the latest NON-YANKED version on crates.io,
# or the literal `NONE` when the crate has no installable release.
#
# Why: this is the one place the gate talks to the network, so it is the one
#   place a fail-open can hide. `curl -f` alone is not enough — it conflates a
#   404 (a real fact: never published) with a 503 (the gate is blind). The two
#   are separated here by reading the HTTP status explicitly, and everything that
#   is neither 200 nor 404 exits the whole script.
# What: reads the crates.io sparse index (newline-delimited JSON, one object per
#   version) and returns the greatest non-yanked `vers`. The sparse index is used
#   rather than the crates.io API because it is CDN-cached and unrate-limited.
# ---------------------------------------------------------------------------
registry_latest() {
  local crate="$1" path body code
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
    echo "NONE"
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

  local latest rc=0
  latest="$(python3 - "$body" <<'PY'
import json, sys

def key(v):
    core = v.split("+")[0].split("-")[0]
    parts = (core.split(".") + ["0", "0", "0"])[:3]
    try:
        return tuple(int(p) for p in parts)
    except ValueError:
        raise SystemExit(2)

best = None
with open(sys.argv[1]) as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        rec = json.loads(line)
        if rec.get("yanked"):
            continue
        v = rec["vers"]
        if best is None or key(v) > key(best):
            best = v
print(best if best else "NONE")
PY
  )" || rc=$?

  rm -f "$body"
  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: TOOL ERROR — the crates.io index entry for '${crate}' did not parse." >&2
    echo "      Whether a baseline exists is UNKNOWN, so no skip may be granted." >&2
    echo "      This is NOT a pass (issue #5050)." >&2
    exit "$EXIT_NO_VERDICT"
  fi
  echo "$latest"
}

# ---------------------------------------------------------------------------
# release_type <baseline> <current> — print major | minor | patch | none.
#
# Cargo's compatibility rules, not plain SemVer: for 0.x the MINOR position is
# the breaking one, so 0.28.1 -> 0.29.0 is a major release. `none` means the two
# versions are equal, or current is older than baseline (which is a version
# mistake, not a SemVer question — it is reported and checked, never skipped).
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
# its own per-crate check summary, e.g.
#     Checked [   0.388s] 223 checks: 214 pass, 9 fail, 0 warn, 31 skip
#     Checked [   0.000s] 0 checks: 0 pass, 254 skip
#
# Why a MARKER and not the exit status: the tool's statuses are not a contract
#   the gate can lean on, and observation shows they collide. 101 is a rustdoc
#   build failure AND a "crate not found in registry" baseline failure; 100 is a
#   real break; 0 is clean. Keying on the status alone would still mix a
#   non-verdict in with a verdict. The summary line is the tool's own statement
#   that it finished comparing, so requiring it means the gate concludes only
#   from evidence that the comparison happened.
#
# Fails CLOSED by construction: if a future cargo-semver-checks renames this
#   line, every run becomes NO VERDICT — loud and non-zero — rather than a
#   silent green. That is the correct direction for the failure to point.
# ---------------------------------------------------------------------------
verdict_computed() {
  grep -Eq '(^|[[:space:]])Checked[[:space:]].*[0-9]+ checks:' "$1"
}

# ---------------------------------------------------------------------------
# --probe: report the baseline decision for one crate and stop. Exists so the
# self-test can drive the registry probe — the gate's only fail-open surface —
# without a Cargo build, and so a human can ask "what would you compare against?"
# ---------------------------------------------------------------------------
if [[ -n "$PROBE_ONLY" ]]; then
  # `registry_latest` runs in a command substitution, i.e. a SUBSHELL, so its
  # `exit 1` cannot terminate this script on its own — it only ends the subshell.
  # Every caller must re-raise it explicitly. Leaving that to `set -e` is exactly
  # the fail-open shape this gate exists to avoid.
  if ! latest="$(registry_latest "$PROBE_ONLY")"; then
    exit "$EXIT_NO_VERDICT"
  fi
  echo "probe ${PROBE_ONLY}: baseline=${latest}"
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
  exit 0
fi

# ---------------------------------------------------------------------------
# Per-crate check.
# ---------------------------------------------------------------------------
checked=0
skipped=0
fail=0
noverdict=0

# Selected once, not per crate: the PATH edit is process-wide and re-running the
# scan for every candidate would just re-answer the same question.
select_toolchain

while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue

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
  if ! baseline="$(registry_latest "$crate")"; then
    exit "$EXIT_NO_VERDICT"
  fi

  if [[ "$baseline" == "NONE" ]]; then
    echo "SKIP ${crate} v${current}: no installable release on crates.io — no baseline exists"
    skipped=$((skipped + 1))
    continue
  fi

  if ! rtype="$(release_type "$baseline" "$current")"; then
    echo "FAIL: TOOL ERROR — could not compare '${baseline}' and '${current}' for ${crate}." >&2
    exit "$EXIT_NO_VERDICT"
  fi
  if [[ "$rtype" == "major" ]]; then
    echo "SKIP ${crate}: ${baseline} -> ${current} is already a major release — every lint is inapplicable"
    skipped=$((skipped + 1))
    continue
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
  echo "CHECK ${crate}: ${baseline} -> ${current} (${rtype} release), $(($# / 2)) feature(s)"

  # Captured rather than streamed so the run can be CLASSIFIED afterwards
  # (#5289); echoed straight back so a human still sees every lint the tool
  # printed, and so preflight-publish.sh's captured log is unchanged in content.
  rc=0
  RUN_LOG="${SCRATCH}/run-${crate}.txt"
  SKIP_UI_BUILD=1 cargo semver-checks \
    --package "$crate" \
    --baseline-version "$baseline" \
    --only-explicit-features "$@" > "$RUN_LOG" 2>&1 || rc=$?
  cat "$RUN_LOG"

  # Order matters: "did it compare anything?" is asked BEFORE "what did it say?".
  # An exit 0 with no summary line is a no-op, not a pass, and must not reach the
  # clean branch — that is the fail-closed half of this check.
  if ! verdict_computed "$RUN_LOG"; then
    echo "NO VERDICT ${crate}: cargo semver-checks exited ${rc} without completing a check run." >&2
    echo "           No public API comparison against baseline ${baseline} was performed." >&2
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

  * rustdoc failed to build. cargo-semver-checks resolves dependencies in a
    scratch project that IGNORES this workspace's Cargo.lock, so it can pick a
    newer transitive dependency whose `rust-version` exceeds the rustc running
    the gate. The `toolchain:` line above says which rustc that was. Install a
    newer one (`rustup toolchain install stable`), or point the gate at one:
        SEMVER_GATE_TOOLCHAIN_BIN=<dir> bash scripts/check_semver.sh --crate <c>
  * the baseline could not be fetched from crates.io.
  * a genuine compile error in the crate — build it directly to see it.

Fix the gate, then re-run it. Do not publish on the strength of this output:
"the check could not run" is not "the check passed" (issue #5289).
EOF
  exit "$EXIT_NO_VERDICT"
fi

echo "semver gate: scanned ${SCANNED}; ${checked} crate(s) checked, ${skipped} skipped — OK."
