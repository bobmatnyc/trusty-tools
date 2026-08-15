#!/usr/bin/env bash
#
# check_semver_types_selftest.sh — tests for the type-level differ.
#
# Why: `scripts/check_semver_types.sh` exists because cargo-semver-checks 0.50.0
#   compares no types, and it is only worth having if it keeps catching the
#   substitutions that tool misses AND never reports a clean run it did not earn.
#   Those are two different failure modes and this file pins both.
#
# What: three groups.
#
#   THE CATCH (cases 1-3). The fixture pair is the real rustdoc JSON of the
#   9-break probe crate, against which cargo-semver-checks caught 2 and missed 7
#   at --release-type patch. Each of the 7 is asserted BY NAME, so a regression
#   that drops one kind of position — say, stops walking trait definitions —
#   fails on that row instead of quietly reducing the count.
#     1. all 7 missed substitutions are reported, and the run exits 1.
#     2. the 2 breaks cargo-semver-checks already catches do NOT become failures
#        here. A removed fn and an added variant are counted as removed/added and
#        never listed as CHANGED — double-reporting them would make this differ
#        an unreliable narrator about the tool it supplements.
#     3. an ADDITIVE pair is clean. Not an identical pair: comparing a document
#        with itself only proves determinism.
#
#   FAILING CLOSED (cases 4-10). rustdoc JSON's schema moves with the toolchain,
#   so "I did not understand this" is a normal outcome and must never render as
#   "nothing changed". This repo has already shipped a gate that printed [PASS]
#   over a comparison that never happened (#5620); every refusal below is one way
#   a second one could appear.
#     4. unsupported format_version.
#     5. the two documents disagree on format_version.
#     6. an unrecognised type node — the shape of the NEXT schema change.
#     7. valid JSON that is not a rustdoc document.
#     8. JSON that does not parse.
#     9. a missing file.
#     10. zero public items in common, so nothing was compared.
#   Every one must exit 3 and none may exit 0. Case 3 is what proves they fail on
#   classification rather than because every path through the differ is broken.
#
#   FINDING THE INPUT (cases 11-14). `--crate` reads the rustdoc JSON the
#   existing gate already cached, so a wrong file silently compared is a false
#   clean. The cache layout is cargo-semver-checks', not ours, and it can change
#   under us.
#     11. a populated cache is found and compared.
#     12. a cold cache is a NO VERDICT naming the command that warms it, never a
#         pass.
#     13. two cached baselines for one version are ambiguous — they differ by
#         feature set — and are refused rather than guessed at.
#     14. the `.metadata.json` sidecar that sits beside every cached baseline is
#         not mistaken for a second baseline. Without the filter, case 11's
#         layout would trip case 13's ambiguity refusal.
#
#   Cases 11-14 drive discovery through --cache-root against a synthesised
#   layout, so they cost no rustdoc build. The documents in it are the probe
#   crate's, filed under a workspace crate's name — discovery keys on the path,
#   never on what the document says it is.
#
# Usage:  bash scripts/check_semver_types_selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs python3, which the
# differ needs anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
DIFFER="${REPO_ROOT}/scripts/check_semver_types.sh"
FIXTURES="${REPO_ROOT}/scripts/test-data/semver-types"
MUTATE="${FIXTURES}/mutate.py"

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/semver-types-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# run_differ <expected-exit> — run with the given arguments, capture combined
# output into $OUT and the status into $RC.
OUT=""
RC=0
run_differ() {
  RC=0
  OUT="$(cd "$REPO_ROOT" && bash "$DIFFER" "$@" 2>&1)" || RC=$?
}

BASE="${FIXTURES}/probe-base.json"
CUR="${FIXTURES}/probe-cur.json"

for f in "$BASE" "$CUR" "$MUTATE"; do
  if [[ ! -f "$f" ]]; then
    echo "SELF-TEST FAIL: fixture ${f} is missing; see ${FIXTURES}/README.md." >&2
    exit 1
  fi
done

# ===========================================================================
# 1. The 7 substitutions cargo-semver-checks missed are each reported by name.
# ===========================================================================
run_differ --baseline-json "$BASE" --current-json "$CUR"
MISSED_BY_CARGO_SEMVER_CHECKS="fn lintprobe::S::method_ret -> : u64 -> Result<u64, String>
fn lintprobe::S::method_param(#1 x): u64 -> String
fn lintprobe::free_ret -> : u64 -> Result<u64, String>
fn lintprobe::free_param(#0 x): u64 -> String
field lintprobe::F.f: u64 -> i64
const lintprobe::C: u64 -> i64
fn lintprobe::T::tm -> : u64 -> Result<u64, String>"

missing=""
while IFS= read -r want; do
  [[ -z "$want" ]] && continue
  [[ "$OUT" == *"CHANGED ${want}"* ]] || missing="${missing}${want}"$'\n'
done <<<"$MISSED_BY_CARGO_SEMVER_CHECKS"

if [[ "$RC" -ne 1 ]]; then
  fail_case "catch: expected exit 1 over 7 type substitutions, got ${RC}" "$OUT"
elif [[ -n "$missing" ]]; then
  fail_case "catch: these substitutions were NOT reported" "$missing" "--- output ---" "$OUT"
elif [[ "$OUT" != *"7 changed"* ]]; then
  fail_case "catch: exit 1 and all 7 named, but the summary did not count 7" "$OUT"
else
  pass_case "all 7 type substitutions cargo-semver-checks misses are reported (exit 1)"
fi

# ===========================================================================
# 2. The 2 breaks cargo-semver-checks DOES catch stay out of the failure set.
#    `removed_fn` is gone in probe-cur and `E::B` is new; both must land in the
#    advisory removed/added counts, and neither may be listed as a type change.
# ===========================================================================
if [[ "$OUT" == *"CHANGED"*"removed_fn"* ]]; then
  fail_case "no-double-report: a removed fn was reported as a type change" "$OUT"
elif [[ "$OUT" != *"1 removed"* ]]; then
  fail_case "no-double-report: the removed fn was not counted as removed" "$OUT"
elif [[ "$OUT" == *"CHANGED"*"::B"* ]]; then
  fail_case "no-double-report: an added enum variant was reported as a type change" "$OUT"
else
  pass_case "removed and added items are counted, never failed on"
fi

# ===========================================================================
# 3. An ADDITIVE pair is clean. This is what proves cases 4-10 fail on
#    classification rather than because every path through the differ is broken.
# ===========================================================================
python3 "$MUTATE" additive "$BASE" "${WORK}/additive.json"
run_differ --baseline-json "$BASE" --current-json "${WORK}/additive.json"
if [[ "$RC" -ne 0 ]]; then
  fail_case "clean pair: an additive change must not fail (exit ${RC})" "$OUT"
elif [[ "$OUT" != *"0 type change(s) — OK"* ]]; then
  fail_case "clean pair: exit 0 but the summary did not report 0 type changes" "$OUT"
elif [[ "$OUT" != *"2 added"* ]]; then
  fail_case "clean pair: the added fn's two positions were not counted as added" "$OUT"
else
  pass_case "an additive pair is clean (exit 0)"
fi

# ===========================================================================
# 4-10. Fail-closed. Each row: name, baseline, current, the text the refusal
#       must contain. Every one must exit 3.
# ===========================================================================
python3 "$MUTATE" bad-format "$BASE" "${WORK}/bad-format.json"
python3 "$MUTATE" unknown-type "$BASE" "${WORK}/unknown-type.json"
python3 "$MUTATE" empty "$BASE" "${WORK}/empty.json"

TAB="$(printf '\t')"
CLOSED_CASES="unsupported format_version${TAB}${WORK}/bad-format.json${TAB}${CUR}${TAB}format_version
format_version mismatch${TAB}${BASE}${TAB}${WORK}/bad-format.json${TAB}format_version
unrecognised type node${TAB}${BASE}${TAB}${WORK}/unknown-type.json${TAB}quantum_ref
JSON that is not rustdoc${TAB}${FIXTURES}/not-rustdoc.json${TAB}${CUR}${TAB}not a rustdoc document
JSON that does not parse${TAB}${FIXTURES}/malformed.json${TAB}${CUR}${TAB}did not parse
a missing file${TAB}${WORK}/absent.json${TAB}${CUR}${TAB}could not be read
nothing in common${TAB}${WORK}/empty.json${TAB}${CUR}${TAB}0 public items"

while IFS="$TAB" read -r name base cur want; do
  [[ -z "$name" ]] && continue
  run_differ --baseline-json "$base" --current-json "$cur"
  if [[ "$RC" -eq 0 ]]; then
    fail_case "fail-closed/${name}: exited 0 — a differ that could not compare reported clean" "$OUT"
  elif [[ "$RC" -ne 3 ]]; then
    fail_case "fail-closed/${name}: expected exit 3 (no verdict), got ${RC}" "$OUT"
  elif [[ "$OUT" != *"NO TYPE VERDICT WAS COMPUTED"* ]]; then
    fail_case "fail-closed/${name}: exit 3 without saying no verdict was computed" "$OUT"
  elif [[ "$OUT" != *"$want"* ]]; then
    fail_case "fail-closed/${name}: refused without naming the cause ('${want}'), so it may be refusing for an unrelated reason" "$OUT"
  else
    pass_case "${name} -> NO VERDICT (exit 3), naming '${want}'"
  fi
done <<<"$CLOSED_CASES"

# ===========================================================================
# 11-14. --crate discovery against a synthesised cache.
#
# The layout mirrors what cargo-semver-checks 0.50.0 writes under
# target/semver-checks/. trusty-common is used because --crate resolves the
# package's declared version from cargo metadata; the DOCUMENTS filed under that
# name are the probe crate's, which is the point — discovery keys on the path.
# ===========================================================================
TC_VERSION="$(cd "$REPO_ROOT" && cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "trusty-common":
        print(p["version"])
        break
')"
if [[ -z "$TC_VERSION" ]]; then
  echo "SELF-TEST FAIL: cargo metadata did not report a version for trusty-common;" >&2
  echo "                cases 11-14 cannot construct a cache layout." >&2
  exit 1
fi
TC_US="$(printf '%s' "$TC_VERSION" | tr '.' '_')"

CACHE="${WORK}/cache-root"
mkdir -p "${CACHE}/cache" "${CACHE}/local-trusty_common-${TC_US}-fake-hash/target/doc"
cp "$CUR" "${CACHE}/local-trusty_common-${TC_US}-fake-hash/target/doc/trusty_common.json"
cp "$BASE" "${CACHE}/cache/trusty_common-0_9_9-fake-hash.json"
# Every cached baseline has this sidecar beside it; case 14 is that it is not
# counted as a second candidate.
printf '{}\n' > "${CACHE}/cache/trusty_common-0_9_9-fake-hash.metadata.json"

# --- 11 + 14. A populated cache is found, and the sidecar does not make it
#              ambiguous.
run_differ --crate trusty-common --cache-root "$CACHE"
if [[ "$RC" -ne 1 ]]; then
  fail_case "discovery/found: expected exit 1 from the probe pair via --crate, got ${RC}" "$OUT"
elif [[ "$OUT" != *"cache/trusty_common-0_9_9-fake-hash.json"* ]]; then
  fail_case "discovery/found: the run did not name the baseline document it read" "$OUT"
elif [[ "$OUT" != *"7 changed"* ]]; then
  fail_case "discovery/found: the discovered pair did not produce the 7 known changes" "$OUT"
else
  pass_case "--crate finds the cached pair, and the .metadata.json sidecar is not a candidate"
fi

# --- 12. A cold cache is a NO VERDICT, not a pass.
run_differ --crate trusty-common --cache-root "${WORK}/empty-cache-root"
if [[ "$RC" -eq 0 ]]; then
  fail_case "discovery/cold: an empty cache exited 0 — nothing was compared and it reported clean" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "discovery/cold: expected exit 3, got ${RC}" "$OUT"
elif [[ "$OUT" != *"check_semver.sh --crate trusty-common"* ]]; then
  fail_case "discovery/cold: refused without naming the command that warms the cache" "$OUT"
else
  pass_case "a cold cache is NO VERDICT naming how to warm it (exit 3)"
fi

# --- 13. Two baselines for one version differ by feature set. Refused.
cp "$BASE" "${CACHE}/cache/trusty_common-0_9_9-other-hash.json"
run_differ --crate trusty-common --cache-root "$CACHE" --baseline 0.9.9
if [[ "$RC" -eq 0 ]]; then
  fail_case "discovery/ambiguous: two candidate baselines exited 0 — one was silently picked" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "discovery/ambiguous: expected exit 3, got ${RC}" "$OUT"
elif [[ "$OUT" != *"ambiguous"* ]]; then
  fail_case "discovery/ambiguous: refused without saying the choice was ambiguous" "$OUT"
elif [[ "$OUT" == *"Build the cache first"* ]]; then
  # An ambiguity refusal that also prints the cold-cache remedy means the exit
  # escaped through a command substitution and the caller carried on — the trap
  # check_semver.sh's registry_probe documents.
  fail_case "discovery/ambiguous: the refusal also printed the cold-cache remedy, so it ran past its own exit" "$OUT"
else
  pass_case "two cached baselines for one version are refused, not guessed at (exit 3)"
fi

# ===========================================================================
# 14. The format-61 pair is read, and the same substitutions are caught there.
#     Cases 1-13 all run on format-57 fixtures. That is what let the differ ship
#     supporting only 57 while every rustdoc on the machine emitted 61: it was
#     inert on every real crate and its self-test never noticed, because the
#     self-test is the one caller that never feeds it a current document.
#
#     `async_ret` is the eighth row and the reason the pair carries an async fn.
#     rustdoc records an `async fn` UN-DESUGARED — `sig.output` holds the inner
#     type, not the `impl Future` the source implies — so an async return is an
#     ordinary type position. That is worth pinning: if a future schema starts
#     recording the desugared future instead, both sides would render as the
#     same opaque node and every async return would silently compare equal.
# ===========================================================================
BASE61="${FIXTURES}/probe-v61-base.json"
CUR61="${FIXTURES}/probe-v61-cur.json"

for f in "$BASE61" "$CUR61"; do
  if [[ ! -f "$f" ]]; then
    echo "SELF-TEST FAIL: fixture ${f} is missing; see ${FIXTURES}/README.md." >&2
    exit 1
  fi
done

run_differ --baseline-json "$BASE61" --current-json "$CUR61"
SUBSTITUTIONS_AT_61="fn lintprobe::S::method_ret -> : u64 -> Result<u64, String>
fn lintprobe::S::method_param(#1 x): u64 -> String
fn lintprobe::S::async_ret -> : Vec<u64> -> Result<Vec<u64>, String>
fn lintprobe::free_ret -> : u64 -> Result<u64, String>
fn lintprobe::free_param(#0 x): u64 -> String
field lintprobe::F.f: u64 -> i64
const lintprobe::C: u64 -> i64
fn lintprobe::T::tm -> : u64 -> Result<u64, String>"

missing61=""
while IFS= read -r want; do
  [[ -z "$want" ]] && continue
  [[ "$OUT" == *"CHANGED ${want}"* ]] || missing61="${missing61}${want}"$'\n'
done <<<"$SUBSTITUTIONS_AT_61"

if [[ "$RC" -eq 3 ]]; then
  fail_case "format-61: the differ does not understand the format its own toolchain emits" "$OUT"
elif [[ "$RC" -ne 1 ]]; then
  fail_case "format-61: expected exit 1 over 8 type substitutions, got ${RC}" "$OUT"
elif [[ -n "$missing61" ]]; then
  fail_case "format-61: these substitutions were NOT reported" "$missing61" "--- output ---" "$OUT"
elif [[ "$OUT" != *"8 changed"* ]]; then
  fail_case "format-61: exit 1 and all 8 named, but the summary did not count 8" "$OUT"
else
  pass_case "format-61 input is understood, async return included (exit 1)"
fi

# ===========================================================================
# 15. Every version in SUPPORTED_FORMAT_VERSIONS has a fixture pair behind it.
#     The list is a claim about what the differ can read, and cases 1-14 only
#     substantiate the versions they happen to carry fixtures for. Without this,
#     adding a version to that tuple to make a red run green is a one-line edit
#     that nothing contradicts — which is the shape of the defect this file's
#     format-61 case exists to document.
# ===========================================================================
declared="$(sed -n 's/^SUPPORTED_FORMAT_VERSIONS = (\(.*\))$/\1/p' "$DIFFER" | tr -d ' ,' | fold -w2 | sort -u | tr '\n' ' ')"
covered="$(python3 - "$BASE" "$BASE61" <<'PY'
import json, sys
print(" ".join(sorted({str(json.load(open(p))["format_version"]) for p in sys.argv[1:]})))
PY
)"

uncovered=""
for v in $declared; do
  [[ -z "$v" ]] && continue
  [[ " $covered " == *" $v "* ]] || uncovered="${uncovered}${v} "
done

if [[ -z "$declared" ]]; then
  fail_case "coverage: could not read SUPPORTED_FORMAT_VERSIONS out of ${DIFFER}"
elif [[ -n "$uncovered" ]]; then
  fail_case "coverage: format version(s) claimed as supported with no fixture proving it" \
    "declared: ${declared}" "covered by fixtures: ${covered}" "uncovered: ${uncovered}" \
    "Add a fixture pair at that version; see ${FIXTURES}/README.md."
else
  pass_case "every declared format version has a fixture pair behind it (${covered})"
fi

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_semver_types_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_semver_types_selftest: ${PASSED} passed, 0 failed."
