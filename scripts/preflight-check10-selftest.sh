#!/usr/bin/env bash
#
# preflight-check10-selftest.sh — the decision half of preflight CHECK 10
# (#6772).
#
# Why: CHECK 10 delegates the comparison to refresh-engagement-pins.sh and then
#   decides what its output means. refresh-engagement-pins-selftest.sh covers
#   the comparison; nothing covered the decision, and the decision had a
#   fail-open arm. On exit 1 the check parsed the stale pins out of
#   `grep '^STALE '`, and when that grep matched nothing all three buckets
#   stayed empty, both guards were skipped, and control reached an
#   unconditional `[WARN] … return 0` — so a stale-pins result passed the gate
#   over an empty list. One drift in how the two scripts spell that line, and
#   the check that exists to stop #6772 stops nothing while printing a WARN
#   that names no pin.
#
#   That is CHECK 5's #5620 failure in a new place: a gate reading exit 0 out of
#   a run that concluded nothing. It gets CHECK 5's treatment — fixtures over
#   the decision, not the delegated gate.
#
# What: drives check10_engagement_pins() — extracted out of
#   preflight-publish.sh by the same awk PATTERN preflight-check5-selftest.sh
#   and preflight-check8-selftest.sh use — over a fixture repo whose
#   scripts/refresh-engagement-pins.sh is a stub with a scripted exit and
#   stdout, and a shell function shadowing `curl` in place of crates.io. No
#   network, no cargo, no real template; the file runs in well under a second.
#
# THE GOVERNING ASSERTION, which every case is a special case of: this check
#   permits a publish only when it can name what it concluded. A pass needs
#   either "every pin is current" from the delegated gate, or a stale pin whose
#   sibling crates.io says is already published. Nothing else — least of all an
#   exit 1 it could not parse — earns exit 0.
#
#   Cases, and the arm each pins:
#
#     1. wrong crate            -> [PASS], n/a (the check is trusty-audit only)
#     2. gate exits 0           -> [PASS], every pin current
#     3. gate exits 2           -> [FAIL], pins unreadable
#     4. exit 1, no STALE line  -> [FAIL]  <- THE REGRESSION; was [WARN]/exit 0
#     5. exit 1, STALE + 404    -> [FAIL], sibling ships in this train
#     6. exit 1, STALE + 200    -> [WARN], sibling already published
#     7. exit 1, STALE + curl   -> [FAIL], crates.io gave no answer
#        failure
#
#   Cases 5-7 are here so the fail-closed arm of case 4 is shown NOT to have
#   swallowed the three verdicts that were already right.
#
# Test: this IS the test. Run directly:
#   bash scripts/preflight-check10-selftest.sh
#
#   PREFLIGHT_SELFTEST_SCRIPT points this at a different preflight-publish.sh.
#
# Portability: POSIX tools plus bash 3.2 (macOS) and bash 5 (Linux CI).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_TOP="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
UNDER_TEST="${PREFLIGHT_SELFTEST_SCRIPT:-${REPO_TOP}/scripts/preflight-publish.sh}"

if [ ! -r "$UNDER_TEST" ]; then
  echo "preflight-check10-selftest: cannot read ${UNDER_TEST}" >&2
  exit 1
fi

FAILURES=0
CASES=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check10-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $*"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-52s -> %s\n' "$1" "$3"
  else
    fail "$1: expected '$2', got '$3'"
  fi
}

# assert_contains <label> <needle> <haystack>
assert_contains() {
  CASES=$((CASES + 1))
  case "$3" in
    *"$2"*) printf '  ok   %-52s -> contains %s\n' "$1" "$2" ;;
    *) fail "$1: output does not contain '$2'. Got: $3" ;;
  esac
}

# ---------------------------------------------------------------------------
# stub_gate <exit> [stdout line...] — write the fixture repo's
# scripts/refresh-engagement-pins.sh. CHECK 10 invokes it as
# `bash "${REPO_ROOT}/scripts/refresh-engagement-pins.sh" --check`, so a plain
# script on that path is the whole seam; no --refresh-script flag is needed.
# ---------------------------------------------------------------------------
FIXTURE_REPO="${WORK}/repo"
mkdir -p "${FIXTURE_REPO}/scripts"

STUB_STDOUT="${FIXTURE_REPO}/stub-stdout.txt"

stub_gate() {
  local rc="$1"; shift
  printf '%s\n' "$@" > "$STUB_STDOUT"
  cat > "${FIXTURE_REPO}/scripts/refresh-engagement-pins.sh" <<EOF
#!/usr/bin/env bash
cat "${STUB_STDOUT}"
exit ${rc}
EOF
}

# ---------------------------------------------------------------------------
# run_check [PKG_NAME] — drive check10_engagement_pins in a clean subshell with
# the shipped function extracted into it. Prints "<exit>|<stderr collapsed>".
#
# PKG_NAME, REPO_ROOT, CRATE_UA, TMP_PINS and TMP_PIN_BODY are the globals the
# extracted function reads; they are declared here exactly as the shipped
# script declares them. CURL_HTTP scripts the stub curl: a bare HTTP code is
# echoed as crates.io's status, and the empty string is a transport failure
# (nonzero exit), which is what makes the shipped `|| http="000"` fire. It is
# assigned per case rather than prefixed onto the call: bash leaves a prefix
# assignment to a function call SET afterwards, which would leak case 6's 200
# into case 7.
# ---------------------------------------------------------------------------
run_check() {
  local pkg="${1:-trusty-audit}"
  local out rc
  # Every global below is read only by the eval'd function, which shellcheck
  # cannot see into.
  # shellcheck disable=SC2034
  out="$(
    set +e
    PKG_NAME="$pkg"
    REPO_ROOT="$FIXTURE_REPO"
    CRATE_UA="preflight-check10-selftest"
    TMP_PINS="${WORK}/pins.log"
    TMP_PIN_BODY="${WORK}/pinbody"

    # Shadows the real curl. Honours -o so the shipped invocation's body file
    # is written, and returns the scripted status on stdout the way
    # `-w '%{http_code}'` does.
    curl() {
      local body=""
      while [ "$#" -gt 0 ]; do
        case "$1" in
          -o) body="$2"; shift 2 ;;
          *) shift ;;
        esac
      done
      [ -n "$body" ] && : > "$body"
      [ -n "${CURL_HTTP:-}" ] || return 7
      printf '%s' "$CURL_HTTP"
    }

    eval "$(awk '/^check10_engagement_pins\(\) \{/,/^\}/' "$UNDER_TEST")"
    check10_engagement_pins 2>&1 >/dev/null
    printf '\nEXIT=%s' "$?"
  )"
  rc="$(printf '%s' "$out" | sed -n 's/^EXIT=//p' | tail -n1)"
  out="$(printf '%s' "$out" | grep -v '^EXIT=' | tr '\n' ' ' | tr -s ' ')"
  printf '%s|%s' "${rc:-?}" "$out"
}

status_of() { printf '%s' "$1" | cut -d'|' -f1; }
text_of() { printf '%s' "$1" | cut -d'|' -f2-; }

echo "check10_engagement_pins — the arms that were already right:"

# --- 1. not trusty-audit: nothing to check ----------------------------------
stub_gate 1
CURL_HTTP="404"
r="$(run_check trusty-search)"
assert_eq       "1 wrong crate: permits"   "0"        "$(status_of "$r")"
assert_contains "1 wrong crate: says n/a"  "[PASS]"   "$(text_of "$r")"
assert_contains "1 wrong crate: names why" "n/a"      "$(text_of "$r")"

# --- 2. every pin current ----------------------------------------------------
stub_gate 0 "refresh-engagement-pins: OK — every [tools] pin is current."
CURL_HTTP="404"
r="$(run_check)"
assert_eq       "2 pins current: permits" "0"      "$(status_of "$r")"
assert_contains "2 pins current: PASS"    "[PASS]" "$(text_of "$r")"

# --- 3. the delegated gate could not read the pins ---------------------------
# Exit 2 is refresh-engagement-pins.sh's usage/unreadable code. An unreadable
# table is exactly the state a silent pass would hide.
stub_gate 2 "refresh-engagement-pins: ERROR: no readable pins in a [tools] table."
CURL_HTTP="404"
r="$(run_check)"
assert_eq       "3 unreadable pins: stops"    "1"                 "$(status_of "$r")"
assert_contains "3 unreadable pins: FAIL"     "[FAIL]"            "$(text_of "$r")"
assert_contains "3 unreadable pins: names rc" "rc=2"              "$(text_of "$r")"
assert_contains "3 unreadable pins: replays"  "no readable pins"  "$(text_of "$r")"

echo
echo "check10_engagement_pins — THE REGRESSION (#6772):"

# --- 4. exit 1 with no STALE line the check can parse ------------------------
# The delegated gate says stale pins EXIST. The line naming them does not match
# what this check greps for, so all three buckets stay empty. Before the fix
# this printed `[WARN] … pin(s) lag a sibling` over an empty list and returned
# 0, letting a publish carry the exact stale template #6772 is about.
stub_gate 1 \
  "STALE-PIN tga pinned 6.0.0 workspace 7.1.0" \
  "refresh-engagement-pins: 1 stale pin(s)."
CURL_HTTP="404"
r="$(run_check)"
assert_eq       "4 unparsable stale result: stops"      "1"        "$(status_of "$r")"
assert_contains "4 unparsable stale result: FAIL"       "[FAIL]"   "$(text_of "$r")"
assert_contains "4 unparsable stale result: not a WARN" "exit 1"   "$(text_of "$r")"
assert_contains "4 unparsable stale result: replays"    "STALE-PIN tga" "$(text_of "$r")"

# The WARN wording is the fail-open path's fingerprint. Reaching it here is the
# defect itself, so assert it is gone by name.
CASES=$((CASES + 1))
case "$(text_of "$r")" in
  *"[WARN]"*) fail "4 unparsable stale result: still reached the WARN arm" ;;
  *) printf '  ok   %-52s -> no WARN\n' "4 unparsable stale result: no WARN" ;;
esac

echo
echo "check10_engagement_pins — the verdicts case 4 must not swallow:"

# --- 5. stale pin whose sibling is NOT published: ships in this train --------
stub_gate 1 "STALE tga pinned=6.0.0 workspace=7.1.0"
CURL_HTTP="404"
r="$(run_check)"
assert_eq       "5 sibling unpublished: stops"    "1"       "$(status_of "$r")"
assert_contains "5 sibling unpublished: FAIL"     "[FAIL]"  "$(text_of "$r")"
assert_contains "5 sibling unpublished: names it" "tga"     "$(text_of "$r")"

# --- 6. stale pin whose sibling IS published: a legitimate lag ---------------
stub_gate 1 "STALE tga pinned=6.0.0 workspace=7.1.0"
CURL_HTTP="200"
r="$(run_check)"
assert_eq       "6 sibling published: permits"  "0"       "$(status_of "$r")"
assert_contains "6 sibling published: WARN"     "[WARN]"  "$(text_of "$r")"
assert_contains "6 sibling published: names it" "tga"     "$(text_of "$r")"

# --- 7. crates.io gave no answer --------------------------------------------
# CURL_HTTP unset makes the stub exit nonzero, so the shipped `|| http="000"`
# fires — the same landing as an unexpected status.
stub_gate 1 "STALE tga pinned=6.0.0 workspace=7.1.0"
CURL_HTTP=""
r="$(run_check)"
assert_eq       "7 crates.io silent: stops"     "1"        "$(status_of "$r")"
assert_contains "7 crates.io silent: FAIL"      "[FAIL]"   "$(text_of "$r")"
assert_contains "7 crates.io silent: names 000" "HTTP 000" "$(text_of "$r")"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "preflight-check10-selftest: ${CASES} assertion(s) passed."
  exit 0
fi
echo "preflight-check10-selftest: ${FAILURES} of ${CASES} assertion(s) FAILED." >&2
exit 1
