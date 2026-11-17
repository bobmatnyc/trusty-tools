#!/usr/bin/env bash
#
# preflight-check2-selftest.sh — preflight CHECK 2, identity (#10587).
#
# Why: CHECK 2 asked `gh auth status`. With the tm gh shim's token in GH_TOKEN
#   that command learns the login through GraphQL, so a GraphQL failure prints
#   "The token in GH_TOKEN is invalid." and exits 1 while `gh api user` with the
#   same token answers. That stopped the trusty-common 0.67.4 publish with the
#   right account and a working credential. CHECK 2 is an identity gate with no
#   override, so the test must also prove that a credential that does not
#   authenticate as bobmatnyc still stops the publish.
#
# What: drives check2_identity against a fake gh that answers `auth status` and
#   `api user` per case. PATH holds only the fake gh and the tools the function
#   needs, so neither the real gh nor a tm shim can answer, nothing reaches the
#   network, and no credential exists. Every case asserts the label and the
#   status.
#
#   The `auth status` answers follow real gh output, captured against a local
#   fake GitHub with a synthetic token.
#
#   Permit:
#     1. THE DEFECT          `auth status` reports GH_TOKEN invalid, exit 1;
#                            `api user` answers bobmatnyc. [PASS].
#     2. stored account      both commands agree on bobmatnyc. [PASS]. Pinned so
#                            the fix keeps the plain case.
#     3. stderr noise        `api user` answers bobmatnyc and warns on stderr.
#                            The warning is not part of the login. [PASS].
#   Stop:
#     4. wrong account       a valid credential for another login. [FAIL].
#     5. rejected credential HTTP 401 from `api user`. [FAIL], and gh's reason
#                            is shown, but the API body is not.
#     6. no credential       exit 4 from `api user`. [FAIL].
#     7. shim cannot mint    the shim's own exit 1 and message. [FAIL].
#     8. empty answer        exit 0 with nothing on stdout. [FAIL].
#     9. similar login       exit 0 with a login that only starts with
#                            bobmatnyc. [FAIL].
#    10. two lines           exit 0 with bobmatnyc plus another line. [FAIL].
#    11. other host          GH_HOST names a host where bobmatnyc answers, and
#                            github.com answers another login. [FAIL]: the
#                            publisher's identity is the github.com one.
#    12. gh missing          no gh on PATH. [FAIL].
#    13. unusable temp dir   gh's answer cannot be read. [FAIL].
#
# Usage: bash scripts/preflight-check2-selftest.sh
# Exit: 0 when every assertion holds; 1 otherwise, printing both sides.
#
# The function under test is EXTRACTED from scripts/preflight-publish.sh BY
#   PATTERN, as in preflight-check8-selftest.sh, so this tests the shipped code.
#   PREFLIGHT_SELFTEST_SCRIPT points this at a different preflight-publish.sh.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_TOP="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
UNDER_TEST="${PREFLIGHT_SELFTEST_SCRIPT:-${REPO_TOP}/scripts/preflight-publish.sh}"

if [ ! -r "$UNDER_TEST" ]; then
  echo "preflight-check2-selftest: cannot read ${UNDER_TEST}" >&2
  exit 1
fi
FN_BODY="$(awk '/^check2_identity\(\) \{/,/^\}/' "$UNDER_TEST")"
HELPER_BODY="$(awk '/^identity_active_account_from_status\(\) \{/,/^\}/' "$UNDER_TEST")"
if [ -z "$FN_BODY" ]; then
  echo "preflight-check2-selftest: check2_identity not found in ${UNDER_TEST}" >&2
  exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check2.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# The only tools on the function's PATH, resolved before PATH is narrowed.
TOOLS="${WORK}/tools"
mkdir -p "$TOOLS"
for tool in mktemp rm sed cat; do
  ln -s "$(command -v "$tool")" "${TOOLS}/${tool}"
done

# A fake gh selected by FAKE_GH_CASE. The fixture body below is the API's failure
# body, which `gh api` writes to stdout; case 5 asserts that it stays unprinted.
FAKE="${WORK}/fake"
mkdir -p "$FAKE"
cat > "${FAKE}/gh" <<FAKE_GH
#!${BASH}
PATH="${TOOLS}"
status_ok() {
  echo "github.com"
  echo "  ✓ Logged in to github.com account \$1 (keyring)"
  echo "  - Active account: true"
}
status_invalid() {
  echo "github.com" >&2
  echo "  X Failed to log in to github.com using token (GH_TOKEN)" >&2
  echo "  - Active account: true" >&2
  echo "  - The token in GH_TOKEN is invalid." >&2
  exit 1
}
if [ "\$1 \$2" = "auth status" ]; then
  case "\$FAKE_GH_CASE" in
    defect) status_invalid ;;
    stored|noise) status_ok bobmatnyc ;;
    wrong) status_ok someone-else ;;
    *) status_invalid ;;
  esac
  exit 0
fi
if [ "\$1 \$2 \$3 \$4 \$5 \$6" != "api --hostname github.com user --jq .login" ]; then
  if [ "\$FAKE_GH_CASE" = "otherhost" ] && [ "\$1" = "api" ]; then
    echo bobmatnyc
    exit 0
  fi
  echo "fake gh: unexpected argv: \$*" >&2
  exit 99
fi
case "\$FAKE_GH_CASE" in
  defect|stored) echo bobmatnyc ;;
  noise) echo "warning: a new release of gh is available" >&2; echo bobmatnyc ;;
  wrong|otherhost) echo someone-else ;;
  rejected)
    echo '{"message":"Bad credentials","fixture":"API-BODY-CANARY"}'
    echo "gh: Bad credentials (HTTP 401)" >&2
    exit 1 ;;
  nocred)
    echo "gh: To use GitHub CLI in automation, set the GH_TOKEN environment variable." >&2
    exit 4 ;;
  shimfail)
    echo "tm: gh account 'bobmatnyc' has no token gh can read on github.com." >&2
    exit 1 ;;
  empty) ;;
  similar) echo bobmatnyc-evil ;;
  twolines) echo bobmatnyc; echo someone-else ;;
  *) echo "fake gh: unknown case \$FAKE_GH_CASE" >&2; exit 98 ;;
esac
FAKE_GH
chmod +x "${FAKE}/gh"

CASES=0
FAILURES=0

# run_check2 <case> <path> [gh-host] [tmpdir] — run the extracted function in a
# clean subshell. Prints "<status>|<stderr collapsed to one line>". The helper
# exists only in pre-fix scripts, where this file proves the defect is red.
run_check2() {
  local case_name="$1" path="$2" gh_host="${3:-}" tmp="${4:-$WORK}"
  local out
  out="$(
    set +e
    # shellcheck disable=SC2030 # PATH narrows for this subshell only.
    PATH="$path"
    export FAKE_GH_CASE="$case_name" TMPDIR="$tmp"
    unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN GH_HOST
    [ -n "$gh_host" ] && export GH_HOST="$gh_host"
    # shellcheck disable=SC2034 # Read by the eval'd pre-fix helper.
    IDENTITY_ACTIVE_ACCOUNT=""
    eval "$HELPER_BODY"
    eval "$FN_BODY"
    check2_identity 2>&1 >/dev/null
    printf '\nEXIT=%s' "$?"
  )"
  local rc text
  rc="${out##*EXIT=}"
  text="${out%EXIT=*}"
  printf '%s|%s' "$rc" "$(printf '%s' "$text" | tr '\n' ' ')"
}

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $1"
  echo "    expected: $2"
  echo "    actual:   $3"
}

assert_contains() {
  CASES=$((CASES + 1))
  case "$3" in
    *"$2"*) echo "  ok: $1" ;;
    *) fail "$1" "contains '$2'" "$3" ;;
  esac
}

assert_absent() {
  CASES=$((CASES + 1))
  case "$3" in
    *"$2"*) fail "$1" "does not contain '$2'" "$3" ;;
    *) echo "  ok: $1" ;;
  esac
}

# expect_permit <label> <case> <path> [gh-host]
expect_permit() {
  local raw
  raw="$(run_check2 "$2" "$3" "${4:-}")"
  assert_contains "$1: status 0" "0|" "${raw%%|*}|"
  assert_contains "$1: [PASS]" "[PASS] identity" "$raw"
}

# expect_stop <label> <case> <path> [gh-host] [tmpdir]
expect_stop() {
  local raw
  raw="$(run_check2 "$2" "$3" "${4:-}" "${5:-$WORK}")"
  assert_contains "$1: status 1" "1|" "${raw%%|*}|"
  assert_contains "$1: [FAIL]" "[FAIL] identity" "$raw"
  assert_absent "$1: no [PASS]" "[PASS]" "$raw"
  LAST_RAW="$raw"
}

GH_PATH="${FAKE}:${TOOLS}"
LAST_RAW=""

expect_permit "1 the defect: auth status says invalid, REST answers bobmatnyc" defect "$GH_PATH"
expect_permit "2 stored account" stored "$GH_PATH"
expect_permit "3 stderr warning is not part of the login" noise "$GH_PATH"

expect_stop "4 wrong account" wrong "$GH_PATH"
assert_contains "4 wrong account: names the login" "someone-else" "$LAST_RAW"
expect_stop "5 rejected credential" rejected "$GH_PATH"
assert_contains "5 rejected credential: shows gh's reason" "Bad credentials (HTTP 401)" "$LAST_RAW"
assert_absent "5 rejected credential: API body stays unprinted" "API-BODY-CANARY" "$LAST_RAW"
expect_stop "6 no credential" nocred "$GH_PATH"
expect_stop "7 shim cannot mint a token" shimfail "$GH_PATH"
assert_contains "7 shim cannot mint a token: shows the shim's reason" "has no token gh can read" "$LAST_RAW"
expect_stop "8 empty answer" empty "$GH_PATH"
expect_stop "9 similar login" similar "$GH_PATH"
expect_stop "10 two lines" twolines "$GH_PATH"
expect_stop "11 GH_HOST does not move the question" otherhost "$GH_PATH" "ghe.example.com"
expect_stop "12 gh missing" stored "$TOOLS"
expect_stop "13 unusable temp dir" stored "$GH_PATH" "" "${WORK}/no-such-dir"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "preflight-check2-selftest: ${CASES} assertion(s) passed."
  exit 0
fi
echo "preflight-check2-selftest: ${FAILURES} of ${CASES} assertion(s) FAILED." >&2
exit 1
