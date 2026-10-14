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
#     DRIFT             a shipped copy differs from its source, or the
#                       content/ copy of the setup skill is missing
#     UNSHIPPED         a source file under a mirrored directory has no
#                       shipped copy (the derived pair rule, #8436 PR C)
#     UNPAIRED          a file under crates/trusty-mpm/src/assets/architect/
#                       has no source, so nothing guards it
#     EMPTY             the sub-project holds no files (a vacuous pass)
#   Runtime state (inbox/, __pycache__/, .pytest_cache/) is not scanned.
#
# Usage: scripts/check_architect_subproject.sh [--root <repo-root>] [--list-pairs]
#   --root points the gate at another tree; the selftest uses it.
#   --list-pairs prints the shipped-copy pairs, one "<source>|<copy>" per
#   line, and exits 0; the selftest plants a drift in each copy it names.
#
# Test: scripts/check_architect_subproject_selftest.sh plants each violation
#   class in a temporary tree and asserts the gate fails on it.
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIST_PAIRS=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ -n "${2:-}" ] || { echo "usage: $0 [--root <repo-root>] [--list-pairs]" >&2; exit 2; }
      ROOT="$(cd "$2" && pwd)"
      shift 2 ;;
    --list-pairs) LIST_PAIRS=1; shift ;;
    *) echo "usage: $0 [--root <repo-root>] [--list-pairs]" >&2; exit 2 ;;
  esac
done

SUB_REL="python/trusty-architect"
SUB="$ROOT/$SUB_REL"

# Shipped copies, derived (#8436 PR C): every file under a MIRRORED directory
# of the sub-project has a byte-equal copy at the same path under ASSET_REL,
# except the EXCEPTIONS below. "<path under the sub-project>|<path under the
# repo root>", one per line.
ASSET_REL="crates/trusty-mpm/src/assets/architect"
MIRRORED="agents docs scripts skills templates"
# Not shipped: the tests beside the scripts, and the setup skill, which ships
# to content/skills/ instead (SPECIAL_PAIRS).
is_exception() { # $1 = path under the sub-project
  case "$1" in
    scripts/test_*.py | scripts/conftest.py | */.DS_Store | *.pyc) return 0 ;;
    skills/tm-architect-setup.md) return 0 ;;
  esac
  return 1
}
SPECIAL_PAIRS="skills/tm-architect-setup.md|content/skills/tm-architect-setup.md"
NL="
"
derived=""
for d in $MIRRORED; do
  [ -d "$SUB/$d" ] || continue
  found="$(find "$SUB/$d" -type f ! -path '*/__pycache__/*' ! -path '*/.pytest_cache/*' \
    | LC_ALL=C sort)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    rel="${f#"$SUB"/}"
    is_exception "$rel" && continue
    derived="$derived$rel|$ASSET_REL/$rel$NL"
  done <<EOF
$found
EOF
done
PAIRS="$SPECIAL_PAIRS$NL$derived"
if [ "$LIST_PAIRS" -eq 1 ]; then
  for pair in $PAIRS; do echo "$pair"; done
  exit 0
fi

fail=0
report() { # $1 = class, $2 = detail
  echo "FAIL $1: $2"
  fail=1
}

if [ ! -d "$SUB" ]; then
  report EMPTY "$SUB_REL does not exist"
  exit 1
fi

scan() { # $1 = class, $2 = extended regex, $3 = optional regex of lines to ignore
  local hits
  hits="$(grep -rInE --exclude-dir=inbox --exclude-dir=__pycache__ \
    --exclude-dir=.pytest_cache -e "$2" "$SUB" 2>/dev/null || true)"
  if [ -n "${3:-}" ] && [ -n "$hits" ]; then
    hits="$(printf '%s\n' "$hits" | grep -vE "$3" || true)"
  fi
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

# #9822: match the sub-project's path, not the bare name: the Rust crate
# crates/trusty-architect is a workspace member and a dependency by design.
MEMBER_RE='python/trusty-architect|"python(/|")'
if [ -f "$ROOT/Cargo.toml" ] &&
   grep -nE "$MEMBER_RE" "$ROOT/Cargo.toml" >/dev/null; then
  report WORKSPACE_MEMBER "Cargo.toml: $(grep -nE "$MEMBER_RE" \
    "$ROOT/Cargo.toml" | head -1)"
fi

scan MAIN_TREE_IMPORT '^[[:space:]]*(from|import)[[:space:]]+trusty_'
# #8436 PR C: a crates.io API URL is not a main-tree path.
scan MAIN_TREE_IMPORT '(^|[^A-Za-z0-9_.-])crates/' 'crates\.io/api/'
scan MAIN_TREE_IMPORT '\.\./\.\.'
scan OPERATOR_PATH '/Users/|/home/|-Users-'
scan OPERATOR_PATH 'trusty-mpm-projects/[^/[:space:]]+/supervisor'
scan SECRET 'ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|(^|[^A-Za-z0-9])sk-[A-Za-z0-9_-]{20,}'

for pair in $PAIRS; do
  src="$SUB/${pair%%|*}"
  dst="$ROOT/${pair#*|}"
  if [ ! -f "$src" ]; then
    report DRIFT "source missing: $SUB_REL/${pair%%|*}"
  elif [ ! -f "$dst" ] && [ "$pair" = "$SPECIAL_PAIRS" ]; then
    report DRIFT "shipped copy missing: ${pair#*|}"
  elif [ ! -f "$dst" ]; then
    report UNSHIPPED "$SUB_REL/${pair%%|*} has no shipped copy at ${pair#*|}; copy it, or list it in is_exception"
  elif ! cmp -s "$src" "$dst"; then
    report DRIFT "${pair#*|} differs from $SUB_REL/${pair%%|*}"
  fi
done

# #8436 P4 fix: a new asset file with no pair would ship unguarded.
if [ -d "$ROOT/$ASSET_REL" ]; then
  shipped="$(find "$ROOT/$ASSET_REL" -type f ! -name .DS_Store \
    ! -path '*/__pycache__/*' | LC_ALL=C sort)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    rel="${f#"$ROOT"/}"
    case "$PAIRS" in
      *"|$rel$NL"*) ;;
      *) report UNPAIRED "$rel is shipped, but has no source under $SUB_REL" ;;
    esac
  done <<EOF
$shipped
EOF
fi

if [ "$fail" -ne 0 ]; then
  echo "check_architect_subproject: FAILED (fix the lines above)" >&2
  exit 1
fi
echo "check_architect_subproject: OK ($files files scanned, shipped copies match)"
