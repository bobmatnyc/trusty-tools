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
# Exit: 0 when every checked crate is SemVer-clean (or is a recorded skip);
#   1 when a crate needs a bump it does not have, or when the gate itself could
#   not do its job.
#
# Test: `scripts/check_semver_selftest.sh` proves the two ways this gate could
#   lie — a vacuous scan and an unreachable registry — both exit non-zero. The
#   catch itself is demonstrated in PR #5051 against #4088's real shape.
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
      sed -n '2,89p' "$0" >&2
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
    exit 1
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
    exit 1
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
    exit 1
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
    exit 1
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
  exit 1
fi

# pkg_field <crate> <field> — one line of package metadata. A crate the metadata
# does not describe is a gate failure, never an empty answer that reads as a skip.
pkg_field() {
  python3 "$PY_HELPER" field "$META_FILE" "$1" "$2" || {
    echo "FAIL: TOOL ERROR — no workspace metadata for package '${1}' (field '${2}')." >&2
    exit 1
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
  return 1
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
  return 1
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
    exit 1
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
      exit 1
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
    exit 1
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
    exit 1
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

  current="$(pkg_field "$crate" version)" || exit 1

  # Subshell re-raise, as at --probe above: a helper's `exit 1` dies with the
  # command substitution unless the caller checks for it.
  if ! baseline="$(registry_latest "$crate")"; then
    exit 1
  fi

  if [[ "$baseline" == "NONE" ]]; then
    echo "SKIP ${crate} v${current}: no installable release on crates.io — no baseline exists"
    skipped=$((skipped + 1))
    continue
  fi

  if ! rtype="$(release_type "$baseline" "$current")"; then
    echo "FAIL: TOOL ERROR — could not compare '${baseline}' and '${current}' for ${crate}." >&2
    exit 1
  fi
  if [[ "$rtype" == "major" ]]; then
    echo "SKIP ${crate}: ${baseline} -> ${current} is already a major release — every lint is inapplicable"
    skipped=$((skipped + 1))
    continue
  fi

  # Checked lazily — a run whose every candidate skipped never needed the tool,
  # and demanding it there would be friction with no coverage behind it.
  if [[ -z "${TOOL_OK:-}" ]]; then
    require_tool || exit 1
    TOOL_OK=1
  fi

  if ! feature_args "$crate" > "${SCRATCH}/features.txt"; then
    exit 1
  fi

  # shellcheck disable=SC2046
  set -- $(cat "${SCRATCH}/features.txt")
  echo "CHECK ${crate}: ${baseline} -> ${current} (${rtype} release), $(($# / 2)) feature(s)"

  rc=0
  SKIP_UI_BUILD=1 cargo semver-checks \
    --package "$crate" \
    --baseline-version "$baseline" \
    --only-explicit-features "$@" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL ${crate}: cargo semver-checks exited ${rc} against baseline ${baseline}" >&2
    fail=1
  fi
  checked=$((checked + 1))
done <<<"$CANDIDATES"

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<'EOF'

A public API change requires a matching version bump.

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
  exit 1
fi

echo "semver gate: scanned ${SCANNED}; ${checked} crate(s) checked, ${skipped} skipped — OK."
