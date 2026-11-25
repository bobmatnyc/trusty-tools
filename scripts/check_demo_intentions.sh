#!/usr/bin/env bash
#
# check_demo_intentions.sh — an `intent(demo):` test intention may appear only
# under a declared demo path (#11489).
#
# Why: a test intention defers coverage. Outside declared demo/POC work it
#   would hide owed or failing tests, so the demo scope is enforced here, not
#   left to the reviewer.
# What: `git grep`s tracked source files (.rs .ts .tsx .js .mjs .cjs .py
#   .svelte) for the literal `intent(demo):`. Markdown and shell are not
#   scanned, so the rule's own docs never match. Each hit must sit under a path
#   prefix listed in scripts/demo-intention-paths.txt (one per line, `#`
#   comments; a trailing `/` names a directory). A hit outside is printed as
#   `file:line:` and fails. An allowlist row that names no tracked path fails,
#   so a finished or promoted demo has to delete its row.
#   DEMO_INTENTION_PATHS overrides the allowlist path (selftest only).
#
# Exit: 0 = every intention is inside a demo path; 1 = a violation or a stale
#   row; 3 = the allowlist is missing.
#
# Test: scripts/check_demo_intentions_selftest.sh

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
ALLOW="${DEMO_INTENTION_PATHS:-scripts/demo-intention-paths.txt}"

[ -f "$ALLOW" ] || { echo "[FAIL] demo-intentions: allowlist missing: $ALLOW" >&2; exit 3; }

PREFIXES=()
while IFS= read -r line || [ -n "$line" ]; do
  line="${line%%#*}"
  line="$(printf '%s' "$line" | tr -d '[:space:]')"
  [ -n "$line" ] && PREFIXES+=("$line")
done < "$ALLOW"

FAIL=0
for p in ${PREFIXES[@]+"${PREFIXES[@]}"}; do
  if [ -z "$(git ls-files -- "$p" | head -n 1)" ]; then
    echo "[FAIL] demo-intentions: allowlist row '$p' names no tracked path — remove it" >&2
    FAIL=1
  fi
done

HITS="$(git grep -n -F 'intent(demo):' -- '*.rs' '*.ts' '*.tsx' '*.js' '*.mjs' \
  '*.cjs' '*.py' '*.svelte' || true)"

COUNT=0
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  COUNT=$((COUNT + 1))
  file="${hit%%:*}"
  ok=0
  for p in ${PREFIXES[@]+"${PREFIXES[@]}"}; do
    case "$file" in "$p"*) ok=1; break ;; esac
  done
  if [ "$ok" -eq 0 ]; then
    rest="${hit#*:}"
    echo "[FAIL] demo-intentions: ${file}:${rest%%:*}: intent(demo): outside a declared demo path ($ALLOW)" >&2
    FAIL=1
  fi
done <<<"$HITS"

if [ "$FAIL" -ne 0 ]; then
  echo "[FAIL] demo-intentions: a test intention is allowed only in declared demo/POC work." >&2
  echo "       Write the real test, or record the demo declaration and add its path to $ALLOW." >&2
  exit 1
fi
echo "[PASS] demo-intentions: ${COUNT} intention(s), all inside declared demo paths."
