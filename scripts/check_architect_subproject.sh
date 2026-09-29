#!/usr/bin/env bash
#
# check_architect_subproject.sh — independence and drift gate for the
# Architect sub-project, python/trusty-architect/ (issue #8436).
#
# Why: #8436 makes the Architect a self-contained sub-project inside the
#   monorepo: no Cargo workspace membership, no imports from the main tree,
#   and no dependency on the private supervisor checkout it was imported from.
#   Its content also ships inside trusty-mpm, and a copy that drifts from the
#   sub-project ships stale text. Acceptance criteria 6 and 7 ask for a check
#   that fails on each of these.
#
# What: exits non-zero, printing one "FAIL <CLASS>: <detail>" line per
#   finding, when any of these holds:
#     CARGO_TOML        a Cargo.toml exists anywhere under the sub-project
#     WORKSPACE_MEMBER  the root Cargo.toml names the sub-project or python/
#     MAIN_TREE_IMPORT  a sub-project file imports a trusty_* Python module,
#                       cites a crates/ path, or climbs out with ../..
#     OPERATOR_PATH     a sub-project file names a home-directory path
#                       (/Users/, /home/, or Claude Code's -Users- slug form)
#                       or a <projects>/<owner>/supervisor checkout path
#     SECRET            a sub-project file carries a GitHub or API token shape
#     DRIFT             a shipped copy is missing or differs from its source
#     EMPTY             the sub-project holds no files (a vacuous pass)
#   Runtime state (inbox/, __pycache__/, .pytest_cache/) is not scanned.
#
# Usage: scripts/check_architect_subproject.sh [--root <repo-root>]
#   --root points the gate at another tree; the selftest uses it.
#
# Test: scripts/check_architect_subproject_selftest.sh plants each violation
#   class in a temporary tree and asserts the gate fails on it.
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${1:-}" = "--root" ]; then
  [ -n "${2:-}" ] || { echo "usage: $0 [--root <repo-root>]" >&2; exit 2; }
  ROOT="$(cd "$2" && pwd)"
elif [ -n "${1:-}" ]; then
  echo "usage: $0 [--root <repo-root>]" >&2
  exit 2
fi

SUB_REL="python/trusty-architect"
SUB="$ROOT/$SUB_REL"

# Shipped copies: "<path under the sub-project>|<path under the repo root>".
# P4 adds the deployed poller files here.
PAIRS="skills/tm-supervisor-setup.md|crates/trusty-mpm/src/assets/skills/tm-supervisor-setup.md"

fail=0
report() { # $1 = class, $2 = detail
  echo "FAIL $1: $2"
  fail=1
}

if [ ! -d "$SUB" ]; then
  report EMPTY "$SUB_REL does not exist"
  exit 1
fi

scan() { # $1 = class, $2 = extended regex; reports each matching line
  local hits
  hits="$(grep -rInE --exclude-dir=inbox --exclude-dir=__pycache__ \
    --exclude-dir=.pytest_cache -e "$2" "$SUB" 2>/dev/null || true)"
  [ -n "$hits" ] || return 0
  while IFS= read -r line; do
    report "$1" "${line#"$ROOT"/}"
  done <<EOF
$hits
EOF
}

files="$(find "$SUB" -type f ! -path '*/inbox/*' ! -path '*/__pycache__/*' \
  ! -path '*/.pytest_cache/*' | wc -l | tr -d ' ')"
[ "$files" -gt 0 ] || report EMPTY "$SUB_REL holds no files"

cargo="$(find "$SUB" -name Cargo.toml -type f)"
if [ -n "$cargo" ]; then
  while IFS= read -r f; do report CARGO_TOML "${f#"$ROOT"/}"; done <<EOF
$cargo
EOF
fi

if [ -f "$ROOT/Cargo.toml" ] &&
   grep -nE 'trusty-architect|"python(/|")' "$ROOT/Cargo.toml" >/dev/null; then
  report WORKSPACE_MEMBER "Cargo.toml: $(grep -nE 'trusty-architect|"python(/|")' \
    "$ROOT/Cargo.toml" | head -1)"
fi

scan MAIN_TREE_IMPORT '^[[:space:]]*(from|import)[[:space:]]+trusty_'
scan MAIN_TREE_IMPORT '(^|[^A-Za-z0-9_.-])crates/'
scan MAIN_TREE_IMPORT '\.\./\.\.'
scan OPERATOR_PATH '/Users/|/home/|-Users-'
scan OPERATOR_PATH 'trusty-mpm-projects/[^/[:space:]]+/supervisor'
scan SECRET 'ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|(^|[^A-Za-z0-9])sk-[A-Za-z0-9_-]{20,}'

for pair in $PAIRS; do
  src="$SUB/${pair%%|*}"
  dst="$ROOT/${pair#*|}"
  if [ ! -f "$src" ]; then
    report DRIFT "source missing: $SUB_REL/${pair%%|*}"
  elif [ ! -f "$dst" ]; then
    report DRIFT "shipped copy missing: ${pair#*|}"
  elif ! cmp -s "$src" "$dst"; then
    report DRIFT "${pair#*|} differs from $SUB_REL/${pair%%|*}"
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "check_architect_subproject: FAILED (fix the lines above)" >&2
  exit 1
fi
echo "check_architect_subproject: OK ($files files scanned, shipped copies match)"
