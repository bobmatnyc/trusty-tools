#!/usr/bin/env bash
#
# release-gate-selftest.sh — fixtures for scripts/release-gate.sh (#11470).
#
# Why: the release gate's verdict is its whole value; each arm is driven here
#   with a stub tm and a synthetic table, so no network, no daemon and no
#   real binary are needed.
# What: the stub prints the text after `say=` and exits with the number
#   after `exit=` in its arguments (else 0), and logs each call. Cases:
#   matching exits and output pass; an exit other than the expected one fails;
#   the expected exit with the wrong text fails; a non-zero-exit row with no
#   expected output refuses to run; a lane with no row of its class fails; a table with a `?` exit refuses to run; a relative
#   --bin refuses; a SKIP or NETWORK row is never executed; the stub sees an
#   empty HOME under the sandbox and TRUSTY_SANDBOX=1, and its cwd is that HOME.
# Usage: bash scripts/release-gate-selftest.sh
# Test: this IS the test; ci.yml runs it.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${REPO_ROOT}/scripts/release-gate.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/release-gate-selftest.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT
FAIL=0
LOG="${WORK}/calls.log"

cat > "${WORK}/tm" <<EOF
#!/bin/bash
echo "\$* | home=\$HOME | pwd=\$(pwd -P) | sandbox=\${TRUSTY_SANDBOX:-}" >> "${LOG}"
for a in "\$@"; do case "\$a" in say=*) echo "\${a#say=}" >&2 ;; esac; done
for a in "\$@"; do case "\$a" in exit=*) exit "\${a#exit=}" ;; esac; done
exit 0
EOF
chmod +x "${WORK}/tm"

table() {
  printf '# fixture\n' > "${WORK}/t.tsv"
  local row
  for row in "$@"; do printf '%s\n' "${row}" >> "${WORK}/t.tsv"; done
}
TAB=$'\t'
# row <path> <class> <invocation> <exit> [want-output]
row() { printf '%s' "$1${TAB}$2${TAB}\$H${TAB}$3${TAB}-${TAB}$4${TAB}${5:--}${TAB}r"; }

expect() {
  local name="$1" want="$2" rc=0; shift 2
  : > "${LOG}"
  bash "${GATE}" "$@" > "${WORK}/out" 2>&1 || rc=$?
  if [ "${rc}" = "${want}" ]; then echo "ok   ${name} (exit ${rc})"
  else echo "FAIL ${name}: exit ${rc}, want ${want}"; sed 's/^/     /' "${WORK}/out"; FAIL=1; fi
}

table "$(row a SAFE "a exit=0" 0)" "$(row b SAFE "b say=no-such-thing exit=3" 3 no-such)" \
  "$(row skipped SKIP "skipped-ran" -)" "$(row net NETWORK "net-ran" 0)"
expect "matching exits pass" 0 --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"
if grep -qE "skipped-ran|net-ran" "${LOG}"; then echo "FAIL a SKIP or NETWORK row ran"; FAIL=1
else echo "ok   SKIP and NETWORK rows never run"; fi
if grep -q "^a exit=0 | home=/private/tmp/tmrg\.[^/]*/home | pwd=/private/tmp/tmrg\.[^/]*/home | sandbox=1$" "${LOG}" \
  || grep -q "^a exit=0 | home=/tmp/tmrg\.[^/]*/home | pwd=/tmp/tmrg\.[^/]*/home | sandbox=1$" "${LOG}"; then
  echo "ok   rows run in the sandbox HOME, cwd there, TRUSTY_SANDBOX=1"
else echo "FAIL row environment:"; sed 's/^/     /' "${LOG}"; FAIL=1; fi

table "$(row a SAFE "a exit=1" 0)"
expect "an unexpected exit fails" 1 --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"

# #11470 F3: an error row passes only on its own error text.
table "$(row b SAFE "b say=some-other-error exit=3" 3 no-such)"
expect "the expected exit with the wrong output fails" 1 \
  --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"
grep -q 'lacks "no-such"' "${WORK}/out" || { echo "FAIL the output mismatch is not named"; FAIL=1; }

table "$(row b SAFE "b exit=3" 3)"
expect "a non-zero-exit row with no expected output refuses to run" 2 \
  --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"

table "$(row a SANDBOX "a" 0)"
expect "a lane with no row of its class fails" 1 \
  --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"
grep -q "lane safe ran 0 rows" "${WORK}/out" || { echo "FAIL the empty lane is not named"; FAIL=1; }

table "$(row a SAFE "a" "0?")"
expect "an unpinned exit refuses to run" 2 --bin "${WORK}/tm" --lane safe --table "${WORK}/t.tsv"

table "$(row a SAFE "a" 0)"
expect "a relative --bin refuses" 2 --bin tm --lane safe --table "${WORK}/t.tsv"

table "$(row c CONTENT "content update" 0)" "$(row f CONTENT "fleet init exit=1" 0)"
expect "a failing CONTENT row fails the content lane" 1 \
  --bin "${WORK}/tm" --lane content --table "${WORK}/t.tsv"
expect "--newest-published pins content update" 1 --bin "${WORK}/tm" --lane content \
  --table "${WORK}/t.tsv" --newest-published content-v2.4.0
grep -q "^content update --content-ref content-v2.4.0 " "${LOG}" \
  || { echo "FAIL --newest-published did not reach content update"; FAIL=1; }

[ "${FAIL}" -eq 0 ] && echo "release-gate-selftest: all cases pass"
exit "${FAIL}"
