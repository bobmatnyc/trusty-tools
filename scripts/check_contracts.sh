#!/usr/bin/env bash
#
# check_contracts.sh — the behavioural half of the public-API gate family.
#
# Why: three gates now guard trusty-common's public API and each sees something
#   the others cannot.
#     cargo-semver-checks       existence and shape
#     check_semver_types.sh     types           (#5723 — the tool never compares them)
#     check_contracts.sh        BEHAVIOUR       (this one)
#
#   Nothing static could ever close the third gap. `latest_trusty_mpm_snapshot`
#   carries a byte-identical signature from trusty-common 0.24.2 through 0.34.0
#   while its precondition inverted: `None` for the session id used to return
#   the newest snapshot overall and now returns `None` (#5272). Every tool
#   reported clean. The only way a machine can see that is if the contract is
#   WRITTEN DOWN somewhere it can read — which is what the `# Code Contract`
#   blocks and the extracted artifact are for. See ADR-0047.
#
# What: two modes.
#
#   DRIFT (default) — regenerate the artifact from the crate's current source
#     and compare it with the copy checked into the repo. A contract edited in a
#     doc comment without regenerating, or an artifact edited by hand, fails
#     here. `UPDATE_CONTRACTS=1` rewrites the artifact instead of failing, the
#     same shape as this repo's generated-doc regions
#     (docs/reference/generated-doc-regions.md).
#
#   DIFF (--diff A B) — compare two artifacts and report every contract that
#     CHANGED on an item present in both. That is the payoff: run it between a
#     published baseline and a release candidate and a silent behavioural break
#     becomes a listed finding, exactly the way the type differ lists
#     `u64 -> Result<u64>`.
#
# FAIL CLOSED, the same rule as the rest of this gate family. This repo has been
#   bitten twice by a gate reporting success while checking nothing (#5620's
#   `0 compared` printing [PASS]; the type gap #5723 closed). Every one of these
#   is a NO VERDICT (3), never a pass:
#     - the rustdoc build fails, or nightly is unavailable
#     - a `# Code Contract` block exists and does not parse
#     - zero contracts are extracted
#     - either artifact is missing, unreadable, or an unknown artifact_version
#     - in --diff, zero items are present in both artifacts
#
# Usage:
#   bash scripts/check_contracts.sh --crate trusty-common
#   UPDATE_CONTRACTS=1 bash scripts/check_contracts.sh --crate trusty-common
#   bash scripts/check_contracts.sh --diff <baseline.json> <current.json>
#
# Exit:
#   0  the artifact matches the source (or --diff found no contract change)
#   1  DRIFT, or --diff found contract changes. Each is listed.
#   2  usage error.
#   3  NO VERDICT — nothing was checked, so nothing may be concluded.
#
# Test: `scripts/check_contracts_selftest.sh`.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
# plus `git`, `cargo`, `rustup` (nightly, for rustdoc JSON) and `python3`.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

EXIT_DRIFT=1
EXIT_USAGE=2
EXIT_NO_VERDICT=3

CRATE=""
DIFF_A=""
DIFF_B=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --crate)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --crate needs a package name" >&2
        exit "$EXIT_USAGE"
      }
      CRATE="$2"
      shift 2
      ;;
    --diff)
      [[ $# -lt 3 ]] && {
        echo "ERROR: --diff needs two artifact paths" >&2
        exit "$EXIT_USAGE"
      }
      DIFF_A="$2"
      DIFF_B="$3"
      shift 3
      ;;
    -h | --help)
      awk 'NR > 1 && /^#/ { print; next } NR > 1 { exit }' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit "$EXIT_USAGE"
      ;;
  esac
done

if [[ -n "$CRATE" && -n "$DIFF_A" ]]; then
  echo "ERROR: --crate and --diff are alternatives, not a pair." >&2
  exit "$EXIT_USAGE"
fi
if [[ -z "$CRATE" && -z "$DIFF_A" ]]; then
  echo "ERROR: give either --crate <name> or --diff <baseline> <current>." >&2
  exit "$EXIT_USAGE"
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/contracts.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT

# ---------------------------------------------------------------------------
# --diff: compare two artifacts. No build, no toolchain — just the two files.
# ---------------------------------------------------------------------------
if [[ -n "$DIFF_A" ]]; then
  echo "CONTRACTS diff"
  echo "  baseline: ${DIFF_A}"
  echo "  current:  ${DIFF_B}"
  rc=0
  python3 "${REPO_ROOT}/scripts/diff_contracts.py" "$DIFF_A" "$DIFF_B" || rc=$?
  case "$rc" in
    0) exit 0 ;;
    1) exit "$EXIT_DRIFT" ;;
    *) exit "$EXIT_NO_VERDICT" ;;
  esac
fi

# ---------------------------------------------------------------------------
# --crate: build rustdoc JSON, extract, compare with the checked-in artifact.
# ---------------------------------------------------------------------------
ARTIFACT="${REPO_ROOT}/crates/${CRATE}/contracts.json"

if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
  {
    echo "NO VERDICT: rustdoc JSON needs the nightly toolchain and none is installed."
    echo "            Nothing was checked. Install it:"
    echo "              rustup toolchain install nightly --profile minimal"
  } >&2
  exit "$EXIT_NO_VERDICT"
fi

# The feature set is the crate's declared features minus the rows in
# scripts/semver-checks-feature-exclusions.tsv — the SAME subtraction
# check_semver.sh applies, and for the same reason: an excluded feature's public
# surface goes unchecked, so each row has to state why it cannot be built here.
# Reusing that file rather than keeping a second list is the point; two lists
# would drift and the drift would be silent.
EXCLUSIONS_FILE="${REPO_ROOT}/scripts/semver-checks-feature-exclusions.tsv"
FEATURES="$(
  CRATE="$CRATE" EXCL="$EXCLUSIONS_FILE" python3 - <<'PY'
import json, os, subprocess, sys

crate = os.environ["CRATE"]
excluded = {"default"}
try:
    with open(os.environ["EXCL"]) as fh:
        for line in fh:
            if line.startswith("#") or not line.strip():
                continue
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 2 and parts[0] == crate:
                excluded.add(parts[1])
except OSError as e:
    print("could not read the feature-exclusions file: %s" % e, file=sys.stderr)
    sys.exit(3)

meta = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        capture_output=True, text=True, check=True,
    ).stdout
)
pkg = next((p for p in meta["packages"] if p["name"] == crate), None)
if pkg is None:
    print("'%s' is not a workspace package" % crate, file=sys.stderr)
    sys.exit(3)
print(",".join(f for f in sorted(pkg["features"]) if f not in excluded))
PY
)" || {
  echo "NO VERDICT: could not resolve the feature set for '${CRATE}'. Nothing was checked." >&2
  exit "$EXIT_NO_VERDICT"
}

CRATE_US="$(printf '%s' "$CRATE" | tr '-' '_')"
DOC_JSON="${REPO_ROOT}/target/doc/${CRATE_US}.json"

echo "CONTRACTS ${CRATE}"
echo "  building rustdoc JSON (nightly, $(printf '%s' "$FEATURES" | tr ',' '\n' | grep -c . ) feature(s))..."
if ! SKIP_UI_BUILD=1 RUSTDOCFLAGS="-Z unstable-options --output-format json" \
  cargo +nightly rustdoc -p "$CRATE" --lib --no-default-features \
  --features "$FEATURES" > "${SCRATCH}/rustdoc.log" 2>&1; then
  # `tail -20` CANNOT show why this build failed, and reporting it as though it
  # could is what made this gate unactionable. rustdoc prints diagnostics in
  # source order and its warnings vastly outnumber its errors: when #5744's
  # `#![deny(rustdoc::broken_intra_doc_links)]` first failed this build, the two
  # `error:` blocks sat at lines 391 and 405 of an 1117-line log and the tail
  # showed twenty lines of unrelated `unclosed HTML tag` warnings. A developer
  # reading that has no way to reach the cause. Print the error BLOCKS instead,
  # and when there are none say so, because "no diagnostic" means the failure is
  # a build script or the toolchain, which is a different thing to go fix.
  # The block ends at a TRULY empty line, not at `/^[[:space:]]*$/`: rustdoc
  # renders the offending source line inside its `= note:` block and indents the
  # blank line above it, so a whitespace-tolerant terminator cuts the block
  # before `no item named ... in scope` — the one line that says what to fix.
  ERRORS="${SCRATCH}/rustdoc.errors"
  awk '/^error/ { blk = 1 } blk { print } /^$/ { blk = 0 }' \
    "${SCRATCH}/rustdoc.log" > "$ERRORS" || true
  SAVED_LOG="${REPO_ROOT}/target/contracts-rustdoc-failure.log"
  mkdir -p "$(dirname "$SAVED_LOG")" 2>/dev/null || true
  cp "${SCRATCH}/rustdoc.log" "$SAVED_LOG" 2>/dev/null || SAVED_LOG=""
  {
    echo "NO VERDICT: the rustdoc JSON build failed, so no contract was read."
    echo "            This is NOT a pass."
    echo ""
    if [ -s "$ERRORS" ]; then
      echo "            rustdoc reported $(grep -c '^error' "$ERRORS") error(s):"
      echo ""
      head -80 "$ERRORS" | sed 's/^/       /'
    else
      echo "            rustdoc emitted no 'error:' diagnostic, so this is a BUILD"
      echo "            failure rather than a documentation failure — a dependency's"
      echo "            build script or the toolchain itself."
      if grep -qiE 'dbus|pkg-config|pkg_config' "${SCRATCH}/rustdoc.log"; then
        echo ""
        echo "            The log names pkg-config/dbus. This crate's full feature set"
        echo "            pulls in libdbus-sys, whose build.rs panics when the dbus"
        echo "            headers are absent — the failure that took down this gate's"
        echo "            first CI run (31858449749). Install them:"
        echo "              Debian/Ubuntu:  sudo apt-get install -y libdbus-1-dev"
        echo "              macOS:          brew install dbus pkg-config"
      fi
      echo ""
      echo "            Last 20 lines:"
      tail -20 "${SCRATCH}/rustdoc.log" | sed 's/^/       /'
    fi
    if [ -n "$SAVED_LOG" ]; then
      echo ""
      echo "            Full build log: ${SAVED_LOG}"
    fi
  } >&2
  exit "$EXIT_NO_VERDICT"
fi

GENERATED="${SCRATCH}/contracts.json"
rc=0
python3 "${REPO_ROOT}/scripts/extract_contracts.py" "$DOC_JSON" "$CRATE" \
  --out "$GENERATED" 2> "${SCRATCH}/extract.err" || rc=$?
cat "${SCRATCH}/extract.err" >&2

# POSITIVE EVIDENCE, not a bare exit status — the rule check_semver_types.sh
# applies to its differ. A helper that crashed or was refactored into printing
# nothing exits some status this cannot classify, and the only safe reading of
# "no marker" is that nothing was extracted.
MARKER="$(grep -E '^extracted: [0-9]+ contract\(s\)' "${SCRATCH}/extract.err" | tail -1 || true)"
COUNT=""
if [[ -n "$MARKER" ]]; then
  COUNT="$(printf '%s\n' "$MARKER" | sed -nE 's/^extracted: ([0-9]+) contract\(s\)/\1/p')"
fi
if [[ "$rc" -ne 0 ]] || [[ -z "$COUNT" ]] || [[ "$COUNT" -lt 1 ]]; then
  {
    echo ""
    echo "NO CONTRACT VERDICT WAS COMPUTED for ${CRATE}. This is NOT a pass."
    echo "The extractor exited ${rc}$([[ -z "$COUNT" ]] && echo " without printing its 'extracted:' marker" || echo " having extracted ${COUNT}")."
    echo "'It could not read the contracts' is not 'the contracts are unchanged'."
  } >&2
  exit "$EXIT_NO_VERDICT"
fi

if [[ "${UPDATE_CONTRACTS:-}" == "1" ]]; then
  mkdir -p "$(dirname "$ARTIFACT")"
  cp "$GENERATED" "$ARTIFACT"
  echo "  wrote ${ARTIFACT} (${COUNT} contract(s))"
  exit 0
fi

if [[ ! -f "$ARTIFACT" ]]; then
  {
    echo "NO VERDICT: ${ARTIFACT} does not exist, so there is nothing to compare"
    echo "            the ${COUNT} extracted contract(s) against. Create it:"
    echo "              UPDATE_CONTRACTS=1 bash scripts/check_contracts.sh --crate ${CRATE}"
  } >&2
  exit "$EXIT_NO_VERDICT"
fi

if diff -u "$ARTIFACT" "$GENERATED" > "${SCRATCH}/drift.diff" 2>&1; then
  echo "  ${COUNT} contract(s) extracted, artifact matches source — OK."
  exit 0
fi

cat "${SCRATCH}/drift.diff"
cat >&2 <<EOF

CONTRACT DRIFT in ${CRATE}: the checked-in artifact does not match the
\`# Code Contract\` blocks in the source, listed above.

The artifact is the machine-readable form of the contracts and the only thing a
cross-version diff can compare. An artifact that lags the source silently
un-does that.

If the source is right, regenerate:
  UPDATE_CONTRACTS=1 bash scripts/check_contracts.sh --crate ${CRATE}

Then read the regenerated diff as a REVIEW ITEM, not a formality: a changed
precondition or postcondition on an existing item is a behavioural change to a
published API, and it is exactly what no other gate in this family can see.
EOF
exit "$EXIT_DRIFT"
