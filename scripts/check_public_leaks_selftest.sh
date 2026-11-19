#!/usr/bin/env bash
# check_public_leaks_selftest.sh — proves `check_content.py public-leaks` can fail.
#
# This file is scanned by the guard like any other, so every planted line is
# spliced at run time ("${J}" is empty) and its source text matches nothing.
# The private name is the synthetic word "zorp"; no real name appears here.
#
# Cases, each in a fresh temp git repo holding copies of the scripts under test:
#   clean                 no hit, empty baseline              OK
#   new-hit               a planted hit, not baselined        FAIL
#   new-hit-<ID>          one planted line per pattern id     FAIL naming the id
#   baselined             the same hit, baselined             OK
#   count-grew            a second hit in a baselined file    FAIL
#   count-shrank          one of two baselined hits removed   OK, with a shrink note
#   stale                 a baseline entry with no hit left   FAIL STALE_BASELINE
#   staged-paths(-hit)    path mode scans only the named file
#   private-<form>        the name camel, glued, with digits, plural, hyphen,
#                         underscore, URL and @ forms         FAIL PRIVATE_NAME
#   private-near-miss     a different word                    OK
#   private-baselined     a PRIVATE_NAME row in the baseline  FAIL (never baselined)
#   private-update        --update-baseline writes no PRIVATE_NAME row
#   private-no-plaintext  the name is not in the output or baseline
#   private-allowlisted   the hit's path is allowlisted       OK
#   private-allow-stale   an allowlisted path with no hit     FAIL STALE_ALLOWLIST
#   private-allow-reason  an allowlist row with no #N reason  FAIL
#   private-allow-digest  the hit's path given as sha256:<hex> OK
#   private-allow-digest-stale  a sha256 row matching no hit  FAIL STALE_ALLOWLIST
#   private-absent-local  no names file, outside CI           OK, skipped notice
#   private-absent-ci     no names file, CI=true              FAIL
#   private-empty-ci      empty names file, GITHUB_ACTIONS    FAIL
#   update-baseline       --update-baseline then a plain run  OK
#
# Exit: 0 when every case holds; 1 naming each case that did not.
# Test: this IS the test; wired into .github/workflows/ci.yml (public-leaks job).

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/public-leaks-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT
fail=0
REPO=""
LAST=""
J=""
EH="E${J}VO_HOST"  # spliced, like every planted line
NAMES="$TMP_ROOT/names.txt"
printf '# synthetic\nzorp\n' >"$NAMES"

new_repo() {
  REPO="$TMP_ROOT/$1"
  mkdir -p "$REPO/scripts"
  git -C "$REPO" init -q -b main
  local f
  for f in check_content.py content_probe_lint.py public_leaks.py public_leak_patterns.txt; do
    cp "$SRC/$f" "$REPO/scripts/"
  done
  printf '# baseline\n' >"$REPO/scripts/public_leak_baseline.tsv"
  w doc.md '# a clean public page'
}

# w <rel> <line>... — write a file and track it.
w() {
  local rel="$1"
  shift
  mkdir -p "$(dirname "$REPO/$rel")"
  printf '%s\n' "$@" >"$REPO/$rel"
  git -C "$REPO" add "$rel"
}

base() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$REPO/scripts/public_leak_baseline.tsv"; }

# expect <case> <exit> <needle> [VAR=value...] — run the gate outside CI unless overridden.
expect() {
  local name="$1" want="$2" needle="$3" out rc
  shift 3
  set +e
  out="$(cd "$REPO" && env -u CI -u GITHUB_ACTIONS -u PUBLIC_LEAK_PRIVATE_NAMES_FILE "$@" \
    python3 scripts/check_content.py public-leaks ${ARGS:-} 2>&1)"
  rc=$?
  set -e
  if [[ "$rc" != "$want" || "$out" != *"$needle"* ]]; then
    echo "FAIL $name: exit $rc (want $want), output lacks '$needle':"
    printf '%s\n' "$out" | sed 's/^/    /'
    fail=1
  else
    echo "ok   $name"
  fi
  LAST="$out"
}

new_repo clean
expect clean 0 "1 file(s) scanned, 0 baselined hit(s) — OK"

HOST_LINE="run it on evo${J}-linux first"
new_repo new-hit
w notes.md "$HOST_LINE"
expect new-hit 1 "FAIL ${EH}: notes.md:1: 1 hit(s), baseline allows 0"

while IFS='|' read -r id line; do
  id="$(eval "printf '%s' \"$id\"")"
  new_repo "class-$id"
  w planted.md "$(eval "printf '%s' \"$line\"")"
  expect "new-hit-$id" 1 "FAIL $id: planted.md:1"
done <<'EOF'
OPERATOR_PATH|see /Users${J}/jdoe/src
GH_CONFIG_DIR|GH_CONFIG_DIR=~/.config/gh${J}-work
TAILNET_IP|ssh 100.${J}101.2.3
HOSTNAME|ssh build.${J}lan
RIG_SPEC|benchmarked on a Mac${J} Studio
ARCH_ID|tracked as ar${J}-Q1z today
RULING_DATED|per the owner ruling${J} 2030-01-02
BRIEF_PATH|written to docs/${J}briefs/x.md
E${J}VO_HOST|build on E${J}VO tonight
ORG_ALIAS|git clone git@github${J}-work:org/repo
EMAIL|mail jdoe@${J}corp.io
SLACK_ID|ping U0123${J}ABCD9 now
EOF

new_repo baselined
w notes.md "$HOST_LINE"
base notes.md ${EH} 1
expect baselined 0 "1 baselined hit(s) — OK"

new_repo count-grew
w notes.md "$HOST_LINE" "$HOST_LINE"
base notes.md ${EH} 1
expect count-grew 1 "FAIL ${EH}: notes.md:1: 2 hit(s), baseline allows 1"

new_repo count-shrank
w notes.md "$HOST_LINE"
base notes.md ${EH} 2
expect count-shrank 0 "1 baseline count(s) can shrink"

new_repo stale
base doc.md ${EH} 1
expect stale 1 "FAIL STALE_BASELINE: doc.md ${EH}: no hit left — delete the entry"

new_repo staged-paths
w notes.md "$HOST_LINE"
ARGS="doc.md" expect staged-paths 0 "1 file(s) scanned"
ARGS="notes.md" expect staged-paths-hit 1 "FAIL ${EH}: notes.md:1"

# The private-name class: every glued form of the synthetic name.
while IFS='|' read -r form line; do
  new_repo "private-$form"
  w notes.md "$line"
  expect "private-$form" 1 "FAIL PRIVATE_NAME: notes.md:1" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
  if [[ "$LAST" == *orp* ]]; then echo "FAIL private-$form-no-plaintext"; fail=1; fi
done <<'EOF'
camel|built for ZorpCorp clients
glued|the zorpinc account
digits|cloned zorp123 last week
plural|both Zorps agreed
hyphen|host mac-zorp-2
underscore|ZORP_TOKEN is set
url|see https://wiki.zorp.io/page
at|ask the team at zorp
EOF

new_repo private-near-miss
w notes.md 'a zor p split and a zoro word'
expect private-near-miss 0 "OK" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"

new_repo private-baselined
w notes.md 'built for ZorpCorp clients'
base notes.md PRIVATE_NAME 1
expect private-baselined 1 "FAIL BASELINE: scripts/public_leak_baseline.tsv:2: PRIVATE_NAME is never baselined" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
ARGS="--update-baseline" expect private-update 0 "baseline rewritten, 0 entr" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
if grep -qi orp "$REPO/scripts/public_leak_baseline.tsv"; then echo "FAIL private-no-plaintext: name in baseline"; fail=1; else echo "ok   private-no-plaintext"; fi
expect private-unlisted 1 "FAIL PRIVATE_NAME: notes.md:1" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
printf 'notes.md\tlegacy placeholder (#1)\n' >"$REPO/scripts/public_leak_private_allowlist.tsv"
expect private-allowlisted 0 "OK" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
printf 'doc.md\tlegacy placeholder (#1)\n' >"$REPO/scripts/public_leak_private_allowlist.tsv"
expect private-allow-stale 1 "FAIL STALE_ALLOWLIST: doc.md" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
NOTES_DIGEST="sha256:$(printf %s notes.md | shasum -a 256 | cut -d' ' -f1)"
printf '%s\tlegacy placeholder (#1)\n' "$NOTES_DIGEST" >"$REPO/scripts/public_leak_private_allowlist.tsv"
expect private-allow-digest 0 "OK" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
DOC_DIGEST="sha256:$(printf %s doc.md | shasum -a 256 | cut -d' ' -f1)"
printf '%s\tlegacy placeholder (#1)\n' "$DOC_DIGEST" >"$REPO/scripts/public_leak_private_allowlist.tsv"
expect private-allow-digest-stale 1 "FAIL STALE_ALLOWLIST: $DOC_DIGEST" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"
printf 'notes.md\tno issue pointer\n' >"$REPO/scripts/public_leak_private_allowlist.tsv"
expect private-allow-reason 1 "FAIL ALLOWLIST: scripts/public_leak_private_allowlist.tsv:1" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$NAMES"

new_repo private-absent
w notes.md 'built for ZorpCorp clients'
expect private-absent-local 0 "PUBLIC_LEAK_PRIVATE_NAMES_FILE absent or empty, private names skipped" PUBLIC_LEAK_PRIVATE_NAMES_FILE="$TMP_ROOT/none.txt"
expect private-absent-ci 1 "FAIL PATTERNS: PUBLIC_LEAK_PRIVATE_NAMES_FILE is absent or empty in CI" CI=true
printf '\n' >"$TMP_ROOT/empty.txt"
expect private-empty-ci 1 "is absent or empty in CI" GITHUB_ACTIONS=true PUBLIC_LEAK_PRIVATE_NAMES_FILE="$TMP_ROOT/empty.txt"

new_repo update-baseline
w notes.md "$HOST_LINE" "mail jdoe@${J}corp.io"
ARGS="--update-baseline" expect update-baseline-write 0 "baseline rewritten, 2 entr"
expect update-baseline-run 0 "2 baselined hit(s) — OK"

exit "$fail"
