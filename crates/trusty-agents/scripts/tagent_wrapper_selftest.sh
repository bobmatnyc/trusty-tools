#!/usr/bin/env bash
#
# tagent_wrapper_selftest.sh — self-test for scripts/tagent-wrapper.sh
# (issue #9224).
#
# Why: `make install` installs the wrapper as `~/.cargo/bin/tagent`, and it
#   exports every line of the project `.env.local` before tagent starts. Under
#   `TRUSTY_SANDBOX=1` that export must not happen, and the file must never
#   change the flag itself.
# What: renders the wrapper template the way install-wrapper.sh does
#   (`__PROJECT_DIR__` -> a temp project dir), places a stub tagent at
#   <project>/target/release/tagent, and runs the rendered wrapper from a
#   cleared environment (`env -i`). The stub prints only whether a canary is
#   set and how TRUSTY_SANDBOX classifies (unset / exactly-1 / set-not-1),
#   never a value. Nothing is written outside the temp dir. Cases:
#     flag-1            TRUSTY_SANDBOX=1: canary unset
#     flag-unset        no flag: canary set, and the file's TRUSTY_SANDBOX=0
#                       line does not set the flag
#     flag-0, flag-true, flag-empty
#                       any value other than "1": canary set (today's
#                       behaviour), flag unchanged by the file
#     file-cannot-unset TRUSTY_SANDBOX=1 with a `TRUSTY_SANDBOX=0` line in
#                       the file: the flag stays exactly-1
#     file-cannot-set   TRUSTY_SANDBOX=0 with a `TRUSTY_SANDBOX=1` line in
#                       the file: the flag stays set-not-1, canary set
#   Every case also fails if the canary value reaches the wrapper's output.
#
# Usage: crates/trusty-agents/scripts/tagent_wrapper_selftest.sh [template]
#   template defaults to tagent-wrapper.sh beside this script; pass another
#   copy to run the cases against it.
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${1:-$SCRIPT_DIR/tagent-wrapper.sh}"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

CANARY_NAME="TAGENT_9224_WRAPPER_CANARY"
CANARY_VALUE="9224-selftest-canary-value"

pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }

if [ ! -f "$TEMPLATE" ]; then
  echo "tagent_wrapper_selftest: template not found: $TEMPLATE" >&2
  exit 1
fi

# The project dir the rendered wrapper points at, with the stub binary.
PROJECT="$TMP_ROOT/project"
mkdir -p "$PROJECT/target/release" "$TMP_ROOT/bin" "$TMP_ROOT/home"
STUB="$PROJECT/target/release/tagent"
cat > "$STUB" <<'STUB_EOF'
#!/bin/sh
if [ -n "${TAGENT_9224_WRAPPER_CANARY+x}" ]; then echo "canary=set"; else echo "canary=unset"; fi
if [ -z "${TRUSTY_SANDBOX+x}" ]; then
  echo "sandbox=unset"
elif [ "$TRUSTY_SANDBOX" = "1" ]; then
  echo "sandbox=exactly-1"
else
  echo "sandbox=set-not-1"
fi
STUB_EOF
chmod +x "$STUB"

# Render exactly as install-wrapper.sh does, into the temp dir only.
WRAPPER="$TMP_ROOT/bin/tagent"
sed "s|__PROJECT_DIR__|${PROJECT}|g" "$TEMPLATE" > "$WRAPPER"
chmod +x "$WRAPPER"

# run_case <name> <flag: "unset" or "=<value>"> <file sandbox line> \
#          <want canary> <want sandbox>
run_case() {
  local name="$1" flag="$2" file_line="$3" want_canary="$4" want_sandbox="$5"
  local out status got want
  printf '# selftest fixture\n%s\n%s=%s\n' "$file_line" "$CANARY_NAME" "$CANARY_VALUE" \
    > "$PROJECT/.env.local"
  local env_args=(PATH="$PATH" HOME="$TMP_ROOT/home")
  if [ "$flag" != "unset" ]; then env_args+=("TRUSTY_SANDBOX${flag}"); fi
  set +e
  out="$(env -i "${env_args[@]}" "$WRAPPER" 2>&1)"
  status=$?
  set -e
  got="$(printf '%s' "$out" | tr '\n' ' ')"
  want="canary=$want_canary sandbox=$want_sandbox"
  case "$out" in
    *"$CANARY_VALUE"*) fail "$name" "the canary value reached the output"; return ;;
  esac
  if [ "$status" -ne 0 ]; then
    fail "$name" "wrapper exit $status, output [$got]"
  elif [ "$got" != "$want" ]; then
    fail "$name" "stub saw [$got], want [$want]"
  else
    pass "$name"
  fi
}

run_case flag-1            "=1"     "# no flag line"   unset exactly-1
run_case flag-unset        unset    "TRUSTY_SANDBOX=0" set   unset
run_case flag-0            "=0"     "# no flag line"   set   set-not-1
run_case flag-true         "=true"  "# no flag line"   set   set-not-1
run_case flag-empty        "="      "# no flag line"   set   set-not-1
run_case file-cannot-unset "=1"     "TRUSTY_SANDBOX=0" unset exactly-1
run_case file-cannot-set   "=0"     "TRUSTY_SANDBOX=1" set   set-not-1
echo "tagent_wrapper selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
