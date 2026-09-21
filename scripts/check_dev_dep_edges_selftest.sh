#!/usr/bin/env bash
#
# check_dev_dep_edges_selftest.sh — fixture suite for
#   scripts/check_dev_dep_edges.sh (issue #8341).
#
# Why: the gate's whole value is that it FAILS on a test-only workspace
#   dev-dependency edge. A gate that cannot be shown to fail is a gate that
#   silently stops working — the #4618 lesson, applied to a graph walk rather
#   than to a scan floor. Each case below drives the real script over a
#   synthetic `cargo metadata` document, so the suite needs no cargo, no
#   network and no build.
#
# What: eight cases.
#   1. a clean workspace passes;
#   2. a dev edge outside the normal tree fails, naming `consumer -> dep`;
#   3. the same edge allowlisted passes;
#   4. an allowlist row whose edge is gone fails as stale;
#   5. an allowlist reason with no issue reference fails as malformed;
#   6. a dev edge the normal tree reaches TRANSITIVELY is not a violation;
#   7. a build-dependency edge is caught the same way a dev edge is;
#   8. an empty package set trips the scan floor.
#   Then the live repository scan must pass.
#
# Usage:
#   bash scripts/check_dev_dep_edges_selftest.sh
#
# Exit: 0 when every case behaves; 1 on the first case that does not.
#
# Test: this file IS the test; CI runs it in .github/workflows/line-cap.yml
#   immediately before scripts/check_dev_dep_edges.sh.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
GATE="${SCRIPT_DIR}/check_dev_dep_edges.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

TAB="$(printf '\t')"
failures=0

fail() {
  printf 'self-test FAIL: %s\n' "$1" >&2
  failures=$((failures + 1))
}

# ---------------------------------------------------------------------------
# fixture <file> <spec...> — write a cargo-metadata document.
#
# Each spec is `name:normal1,normal2|dev1,dev2|build1`, where any section may be
# empty. Enough shape for this gate: it reads `packages[].name` and each
# dependency's `name` and `kind`.
# ---------------------------------------------------------------------------
fixture() {
  local out="$1"
  shift
  python3 - "$out" "$@" <<'PY'
import json, sys
out, specs = sys.argv[1], sys.argv[2:]
packages = []
for spec in specs:
    name, _, rest = spec.partition(":")
    normal, dev, build = (rest.split("|") + ["", ""])[:3]
    deps = []
    for kind, names in (("normal", normal), ("dev", dev), ("build", build)):
        for dep in filter(None, names.split(",")):
            deps.append({"name": dep, "kind": None if kind == "normal" else kind,
                         "req": "*", "optional": False})
    packages.append({"name": name, "version": "0.1.0", "dependencies": deps})
with open(out, "w", encoding="utf-8") as fh:
    json.dump({"packages": packages, "workspace_members": [], "version": 1}, fh)
PY
}

# `pad <file> <n>` — append n filler members so a case clears the scan floor.
pad() {
  python3 - "$1" "$2" <<'PY'
import json, sys
path, n = sys.argv[1], int(sys.argv[2])
with open(path, encoding="utf-8") as fh:
    meta = json.load(fh)
for i in range(n):
    meta["packages"].append({"name": f"filler-{i}", "version": "0.1.0", "dependencies": []})
with open(path, "w", encoding="utf-8") as fh:
    json.dump(meta, fh)
PY
}

# `run <metadata> <allowlist>` — run the gate, capture output, print exit code.
run() {
  local meta="$1" allow="$2" rc=0
  DEV_DEP_METADATA="${meta}" DEV_DEP_ALLOWLIST="${allow}" \
    bash "${GATE}" > "${WORK}/out" 2>&1 || rc=$?
  printf '%s' "${rc}"
}

: > "${WORK}/empty-allowlist.tsv"

# --- case 1: clean workspace ------------------------------------------------
fixture "${WORK}/clean.json" "app:lib|lib," "lib:||"
pad "${WORK}/clean.json" 20
rc="$(run "${WORK}/clean.json" "${WORK}/empty-allowlist.tsv")"
if [ "${rc}" != "0" ]; then
  fail "a clean workspace must pass, got exit ${rc}:"
  cat "${WORK}/out" >&2
fi

# --- case 2: a test-only edge fails -----------------------------------------
fixture "${WORK}/dirty.json" "app:lib|other," "lib:||" "other:||"
pad "${WORK}/dirty.json" 20
rc="$(run "${WORK}/dirty.json" "${WORK}/empty-allowlist.tsv")"
if [ "${rc}" = "0" ]; then
  fail "a dev edge outside the normal tree must fail, got exit 0:"
  cat "${WORK}/out" >&2
elif ! grep -q "app -> other" "${WORK}/out"; then
  fail "the failure must name the offending edge \`app -> other\`, got:"
  cat "${WORK}/out" >&2
fi

# --- case 3: that edge allowlisted passes -----------------------------------
printf 'app%sother%s#8341: the blocker, named.\n' "${TAB}" "${TAB}" \
  > "${WORK}/allow.tsv"
rc="$(run "${WORK}/dirty.json" "${WORK}/allow.tsv")"
if [ "${rc}" != "0" ]; then
  fail "an allowlisted edge must pass, got exit ${rc}:"
  cat "${WORK}/out" >&2
fi

# --- case 4: a stale allowlist row fails ------------------------------------
rc="$(run "${WORK}/clean.json" "${WORK}/allow.tsv")"
if [ "${rc}" = "0" ]; then
  fail "an allowlist row whose edge is gone must fail, got exit 0:"
  cat "${WORK}/out" >&2
elif ! grep -q "no longer exists" "${WORK}/out"; then
  fail "the stale-row failure must say the edge no longer exists, got:"
  cat "${WORK}/out" >&2
fi

# --- case 5: a reason with no issue reference is malformed ------------------
printf 'app%sother%sbecause I said so\n' "${TAB}" "${TAB}" > "${WORK}/noissue.tsv"
rc="$(run "${WORK}/dirty.json" "${WORK}/noissue.tsv")"
if [ "${rc}" = "0" ]; then
  fail "an allowlist reason naming no issue must fail, got exit 0:"
  cat "${WORK}/out" >&2
elif ! grep -q "must name an issue" "${WORK}/out"; then
  fail "the malformed-row failure must say the reason needs an issue, got:"
  cat "${WORK}/out" >&2
fi

# --- case 6: a transitively-normal dep is not a violation -------------------
# app -normal-> mid -normal-> deep, and app -dev-> deep. Resolver v2 already
# compiles `deep` for any `cargo build -p app`, so the dev edge adds nothing.
fixture "${WORK}/transitive.json" "app:mid|deep," "mid:deep||" "deep:||"
pad "${WORK}/transitive.json" 20
rc="$(run "${WORK}/transitive.json" "${WORK}/empty-allowlist.tsv")"
if [ "${rc}" != "0" ]; then
  fail "a dev edge the normal tree reaches transitively must pass, got exit ${rc}:"
  cat "${WORK}/out" >&2
fi

# --- case 7: a build-dependency edge is caught too --------------------------
fixture "${WORK}/buildish.json" "app:lib||gen" "lib:||" "gen:||"
pad "${WORK}/buildish.json" 20
rc="$(run "${WORK}/buildish.json" "${WORK}/empty-allowlist.tsv")"
if [ "${rc}" = "0" ]; then
  fail "a build-dependency edge outside the normal tree must fail, got exit 0:"
  cat "${WORK}/out" >&2
elif ! grep -q "app -> gen" "${WORK}/out"; then
  fail "the failure must name the offending build edge \`app -> gen\`, got:"
  cat "${WORK}/out" >&2
fi

# --- case 8: the scan floor refuses a vacuous read --------------------------
fixture "${WORK}/tiny.json" "app:||"
rc="$(run "${WORK}/tiny.json" "${WORK}/empty-allowlist.tsv")"
if [ "${rc}" = "0" ]; then
  fail "a one-crate metadata read must trip the scan floor, got exit 0:"
  cat "${WORK}/out" >&2
elif ! grep -q "SCAN FLOOR" "${WORK}/out"; then
  fail "the vacuous-scan failure must name the scan floor, got:"
  cat "${WORK}/out" >&2
fi

# --- the live repository must pass ------------------------------------------
live_rc=0
( cd "${REPO_ROOT}" && bash "${GATE}" ) > "${WORK}/live" 2>&1 || live_rc=$?
if [ "${live_rc}" != "0" ]; then
  fail "the live repository scan must pass, got exit ${live_rc}:"
  cat "${WORK}/live" >&2
fi

if [ "${failures}" -gt 0 ]; then
  printf '\n%d self-test case(s) failed.\n' "${failures}" >&2
  exit 1
fi

printf 'check_dev_dep_edges.sh self-test: 8 fixture cases + the live scan, all clean.\n'
