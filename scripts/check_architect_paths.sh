#!/usr/bin/env bash
#
# check_architect_paths.sh — every path and skill the shipped Architect
# manual and skills name exists after `tm fleet init` (issue #9997, AC1).
#
# Why: the Architect acts on what its instructions name. A manual or skill
#   that points at a script, record, doc or skill `tm fleet init` does not
#   write sends a fresh Architect after a file that is not there. The old
#   unit test read backticked words in two skills only.
#
# What: runs `tm fleet init --dir <tmp>/architect --no-launch` under a scratch
#   HOME, a private tmux directory and an environment of PATH, HOME, LANG,
#   TMUX_TMPDIR and TRUSTY_CONTENT_OFFLINE=1 only, from the repo root so the
#   checkout's content is the one seeded. Then scripts/lib/architect_paths.py
#   parses the manual, the seeded CLAUDE.md and every seeded SKILL.md
#   (backticks, fenced blocks and `python3|bash <path>` commands) and fails on
#   each reference that does not resolve. A missing `tm`, a failed init or a
#   read error fails the check (FAIL INIT, FAIL READ); nothing is skipped.
#
# Usage: scripts/check_architect_paths.sh [--root <repo>] [--init-dir <dir>]
#   TM_BIN names the tm binary (default: `tm` on PATH). --init-dir checks an
#   already initialised directory instead of running init; the selftest uses
#   it. Exit 0: all resolve. Exit 1: findings. Exit 2: usage error.
#
# Test: scripts/check_architect_paths_selftest.sh.
#
# Portability: POSIX tools plus python3; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INIT_DIR=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ -n "${2:-}" ] || { echo "usage: $0 [--root <repo>] [--init-dir <dir>]" >&2; exit 2; }
      ROOT="$(cd "$2" && pwd)"; shift 2 ;;
    --init-dir)
      [ -n "${2:-}" ] || { echo "usage: $0 [--root <repo>] [--init-dir <dir>]" >&2; exit 2; }
      INIT_DIR="$2"; shift 2 ;;
    *) echo "usage: $0 [--root <repo>] [--init-dir <dir>]" >&2; exit 2 ;;
  esac
done

if [ -z "$INIT_DIR" ]; then
  TM="${TM_BIN:-$(command -v tm || true)}"
  if [ -z "$TM" ] || [ ! -x "$TM" ]; then
    echo "FAIL INIT: no tm binary (set TM_BIN or put tm on PATH): ${TM:-none}"
    exit 1
  fi
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/architect-paths.XXXXXX")"
  trap 'rm -rf "$WORK"' EXIT
  mkdir -p "$WORK/home" "$WORK/tmux"
  INIT_DIR="$WORK/architect"
  rc=0
  out="$(cd "$ROOT" && env -i PATH="$PATH" HOME="$WORK/home" LANG="${LANG:-C.UTF-8}" \
    TMUX_TMPDIR="$WORK/tmux" TRUSTY_CONTENT_OFFLINE=1 \
    "$TM" fleet init --dir "$INIT_DIR" --no-launch 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ] || [ ! -d "$INIT_DIR/.claude/skills" ]; then
    echo "FAIL INIT: tm fleet init exited $rc"
    printf '%s\n' "$out" | tail -n 20 | sed 's/^/  /'
    exit 1
  fi
fi

# Not exec: the EXIT trap removes the scratch init directory.
rc=0
python3 "$SCRIPT_DIR/lib/architect_paths.py" --root "$ROOT" --init-dir "$INIT_DIR" || rc=$?
exit "$rc"
