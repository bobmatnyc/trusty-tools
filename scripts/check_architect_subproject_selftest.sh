#!/usr/bin/env bash
#
# check_architect_subproject_selftest.sh — planted-violation cases for
# scripts/check_architect_subproject.sh (issue #8436).
#
# Why: a gate that has only been seen passing cannot be told apart from one
#   that returns 0 unconditionally. Each violation class gets one planted case
#   that must turn the gate red with that class named in its output.
#
# What: builds a minimal clean repo tree in a temp dir (root Cargo.toml with
#   `members = ["crates/*"]`, a sub-project file, and a skill whose shipped
#   copy matches), asserts the gate passes on it, then for each case copies the
#   clean tree, plants one violation, and asserts exit 1 plus the expected
#   "FAIL <CLASS>" text. Token-shaped strings are assembled at run time so this
#   file carries no literal credential shape.
#
# Test: this IS the test. Run: scripts/check_architect_subproject_selftest.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_architect_subproject.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/architect-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

SKILL_SRC="python/trusty-architect/skills/tm-supervisor-setup.md"
SKILL_DST="crates/trusty-mpm/src/assets/skills/tm-supervisor-setup.md"

clean="$WORK/clean"
mkdir -p "$clean/python/trusty-architect/skills" "$clean/python/trusty-architect/scripts" \
  "$clean/crates/trusty-mpm/src/assets/skills"
printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >"$clean/Cargo.toml"
printf 'import os\nROOT = os.path.dirname(__file__)\n' >"$clean/python/trusty-architect/scripts/ok.py"
printf -- '---\nname: tm-supervisor-setup\n---\n# Set up the Architect\n' >"$clean/$SKILL_SRC"
cp "$clean/$SKILL_SRC" "$clean/$SKILL_DST"

repeat() { # $1 = char, $2 = count
  local s="" i=0
  while [ "$i" -lt "$2" ]; do s="$s$1"; i=$((i + 1)); done
  printf '%s' "$s"
}
TOKEN="ghp""_$(repeat a 36)"

plant() { # $1 = case name; mutates $WORK/$1, a copy of the clean tree
  local t="$WORK/$1" sub="$WORK/$1/python/trusty-architect"
  case "$1" in
    cargo-toml) printf '[package]\nname = "x"\n' >"$sub/Cargo.toml" ;;
    workspace-member)
      printf '[workspace]\nmembers = ["crates/*", "python/trusty-architect"]\n' >"$t/Cargo.toml" ;;
    python-import) printf 'from trusty_voice import config\n' >"$sub/scripts/bad.py" ;;
    crates-path) printf 'P = "../crates/trusty-mpm/src"\n' >"$sub/scripts/bad.py" ;;
    climb-out) printf 'sys.path.insert(0, "../../lib")\n' >"$sub/scripts/bad.py" ;;
    users-path) printf 'ROOT = "/Users/someone/work"\n' >"$sub/scripts/bad.py" ;;
    home-path) printf 'ROOT = "/home/someone/work"\n' >"$sub/scripts/bad.py" ;;
    claude-slug) printf 'D = "projects/-Users-someone-work"\n' >"$sub/scripts/bad.py" ;;
    private-checkout) printf 'see ~/trusty-mpm-projects/acme/supervisor\n' >"$sub/notes.md" ;;
    secret) printf 'TOKEN = "%s"\n' "$TOKEN" >"$sub/scripts/bad.py" ;;
    drift) printf 'edited\n' >>"$t/$SKILL_DST" ;;
    shipped-missing) rm "$t/$SKILL_DST" ;;
    empty) rm -rf "$sub" && mkdir -p "$sub" ;;
  esac
}

# case<TAB>expected FAIL class
TAB="$(printf '\t')"
CASES="cargo-toml${TAB}CARGO_TOML
workspace-member${TAB}WORKSPACE_MEMBER
python-import${TAB}MAIN_TREE_IMPORT
crates-path${TAB}MAIN_TREE_IMPORT
climb-out${TAB}MAIN_TREE_IMPORT
users-path${TAB}OPERATOR_PATH
home-path${TAB}OPERATOR_PATH
claude-slug${TAB}OPERATOR_PATH
private-checkout${TAB}OPERATOR_PATH
secret${TAB}SECRET
drift${TAB}DRIFT
shipped-missing${TAB}DRIFT
empty${TAB}EMPTY"

fail=0
pass=0

rc=0
out="$("$GATE" --root "$clean" 2>&1)" || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "FAIL: clean tree -> exit $rc (expected 0)" >&2
  printf '%s\n' "$out" | sed 's/^/       /' >&2
  fail=1
else
  pass=$((pass + 1))
fi

while IFS="$TAB" read -r name class; do
  [ -n "$name" ] || continue
  cp -R "$clean" "$WORK/$name"
  plant "$name"
  rc=0
  out="$("$GATE" --root "$WORK/$name" 2>&1)" || rc=$?
  if [ "$rc" -ne 1 ]; then
    echo "FAIL: $name -> exit $rc (expected 1)" >&2
    fail=1
  elif ! printf '%s\n' "$out" | grep -q "FAIL $class"; then
    echo "FAIL: $name -> output lacks 'FAIL $class'" >&2
    fail=1
  else
    pass=$((pass + 1))
    continue
  fi
  printf '%s\n' "$out" | sed 's/^/       /' >&2
done <<EOF
$CASES
EOF

if [ "$fail" -ne 0 ]; then
  echo "check_architect_subproject selftest: FAILED ($pass cases passed)" >&2
  exit 1
fi
echo "check_architect_subproject selftest: OK ($pass cases passed)"
