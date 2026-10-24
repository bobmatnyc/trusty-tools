#!/usr/bin/env bash
#
# check_architect_paths_selftest.sh — planted cases for
# scripts/check_architect_paths.sh (issue #9997, AC1).
#
# Why: a path checker only ever seen passing cannot be told apart from one
#   that reads nothing. Each reference form the checker parses gets a planted
#   dangling reference that must turn it red, and each fail-closed arm (a read
#   error, no skills, no `tm`) must fail rather than skip.
#
# What: builds a fake repo root (the manual under content/) and a fake
#   initialised Architect directory in a temp dir, asserts the checker passes
#   on that clean pair, then for each case copies the pair, plants one fault,
#   and asserts exit 1 plus the expected "FAIL <CLASS>" text and the planted
#   name. Near misses that must stay clean are a second list.
#
# Test: this IS the test. Run: scripts/check_architect_paths_selftest.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_architect_paths.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/architect-paths-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

clean="$WORK/clean"
manual="$clean/root/content/instructions/architect/architect.md"
mkdir -p "$(dirname "$manual")" \
  "$clean/init/scripts" "$clean/init/local/projects" "$clean/init/docs/architect" \
  "$clean/init/.claude/skills/architect-tmux-ops" "$clean/init/.claude/skills/tm-fleet-check" \
  "$clean/init/.claude/agents"
for f in scripts/relay.sh scripts/posted.sh scripts/input-state.py local/state.md \
         docs/architect/voice.md CLAUDE.md; do
  printf 'seeded\n' >"$clean/init/$f"
done
printf '%s\n' \
  '# Manual' \
  'Send with `scripts/relay.sh <session> <text>`; records in `local/projects/<project>.md`.' \
  'Classify with python3 scripts/input-state.py first. Voice: `docs/architect/voice.md`.' \
  '- **Sends (r-073).** Check first. (details: architect-tmux-ops, tm-fleet-check)' \
  '' \
  '```sh' \
  "scripts/posted.sh tm-api 13:00Z" \
  '```' \
  '' \
  '| Skill | Kind | Load it when |' \
  '| --- | --- | --- |' \
  '| `architect-tmux-ops` | framework | before a send |' \
  '| `tm-fleet-check` | framework | every pass |' >"$manual"
printf '%s\n' '---' 'name: architect-tmux-ops' '---' \
  'Read `local/state.md` and every `local/projects/*.md`; see `tm-fleet-check`.' \
  'Runtime files such as `inbox/alerts.jsonl` and the session `tm-architect` are not paths.' \
  >"$clean/init/.claude/skills/architect-tmux-ops/SKILL.md"
printf '%s\n' '---' 'name: tm-fleet-check' '---' 'See `scripts/relay.sh` and `CLAUDE.md`.' \
  >"$clean/init/.claude/skills/tm-fleet-check/SKILL.md"

# #10161 D3c: one seeded agent; agents name paths and skills like the skills do.
printf '%s\n' '---' 'name: runner' '---' \
  'Run `scripts/relay.sh` read-only; see `architect-tmux-ops` and `local/projects/*.md`.' \
  >"$clean/init/.claude/agents/runner.md"

SKILL_FILE=".claude/skills/architect-tmux-ops/SKILL.md"
AGENT_FILE=".claude/agents/runner.md"
plant() { # $1 = case name, $2 = case dir (a copy of the clean pair)
  local d="$2" m="$2/root/content/instructions/architect/architect.md"
  case "$1" in
    backtick) printf 'See `scripts/planted-tick.sh`.\n' >>"$m" ;;
    fence) printf '```sh\nscripts/planted-fence.sh --now\n```\n' >>"$m" ;;
    command-python) printf 'Run python3 scripts/planted-cmd.py first.\n' >>"$m" ;;
    command-bash) printf 'Run bash scripts/planted-cmd.sh first.\n' >>"$d/init/$SKILL_FILE" ;;
    placeholder) printf 'Profiles: `local/planted-users/<name>.md`.\n' >>"$m" ;;
    template-dir) printf 'Templates: `templates/`.\n' >>"$m" ;;
    details-skill) printf -- '- **Hosts.** (details: architect-planted-skill)\n' >>"$m" ;;
    table-skill) printf '| `tm-planted-skill` | framework | never |\n' >>"$m" ;;
    tick-skill) printf 'Load `architect-planted-skill` first.\n' >>"$d/init/$SKILL_FILE" ;;
    deleted-script) rm "$d/init/scripts/relay.sh" ;;
    unreadable-skill)
      rm "$d/init/$SKILL_FILE" && mkdir "$d/init/$SKILL_FILE" ;;
    undecodable-skill) printf '\377\376 planted-bytes\n' >"$d/init/$SKILL_FILE" ;;
    missing-manual) rm "$m" ;;
    no-skills) rm -rf "$d/init/.claude/skills" && mkdir -p "$d/init/.claude/skills" ;;
    agent-backtick) printf 'Write `local/planted-agent/run.md`.\n' >>"$d/init/$AGENT_FILE" ;;
    agent-command) printf 'Run python3 scripts/planted-agent.py -.\n' >>"$d/init/$AGENT_FILE" ;;
    agent-skill) printf 'Load `architect-planted-agent-skill` first.\n' >>"$d/init/$AGENT_FILE" ;;
    undecodable-agent) printf '\377\376 planted-bytes\n' >"$d/init/$AGENT_FILE" ;;
    no-agents) rm -rf "$d/init/.claude/agents" && mkdir -p "$d/init/.claude/agents" ;;
    missing-agents) rm -rf "$d/init/.claude/agents" ;;
  esac
}

# case<TAB>expected class<TAB>text the failure must name
TAB="$(printf '\t')"
CASES="backtick${TAB}PATH${TAB}scripts/planted-tick.sh
fence${TAB}PATH${TAB}scripts/planted-fence.sh
command-python${TAB}PATH${TAB}scripts/planted-cmd.py
command-bash${TAB}PATH${TAB}scripts/planted-cmd.sh
placeholder${TAB}PATH${TAB}local/planted-users/
template-dir${TAB}PATH${TAB}templates/
details-skill${TAB}SKILL${TAB}architect-planted-skill
table-skill${TAB}SKILL${TAB}tm-planted-skill
tick-skill${TAB}SKILL${TAB}architect-planted-skill
deleted-script${TAB}PATH${TAB}scripts/relay.sh
unreadable-skill${TAB}READ${TAB}architect-tmux-ops
undecodable-skill${TAB}READ${TAB}architect-tmux-ops
missing-manual${TAB}READ${TAB}architect.md
no-skills${TAB}EMPTY${TAB}.claude/skills
agent-backtick${TAB}PATH${TAB}local/planted-agent/run.md
agent-command${TAB}PATH${TAB}scripts/planted-agent.py
agent-skill${TAB}SKILL${TAB}architect-planted-agent-skill
undecodable-agent${TAB}READ${TAB}runner.md
no-agents${TAB}EMPTY${TAB}.claude/agents
missing-agents${TAB}READ${TAB}.claude/agents"

fail=0
pass=0
run_check() { # $1 = case dir; prints the output; returns the exit code
  local rc=0
  "$CHECK" --root "$1/root" --init-dir "$1/init" 2>&1 || rc=$?
  return "$rc"
}

rc=0
out="$(run_check "$clean")" || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "FAIL: clean tree -> exit $rc (expected 0)" >&2
  printf '%s\n' "$out" | sed 's/^/       /' >&2
  fail=1
else
  pass=$((pass + 1))
fi

while IFS="$TAB" read -r name class needle; do
  [ -n "$name" ] || continue
  cp -R "$clean" "$WORK/$name"
  plant "$name" "$WORK/$name"
  rc=0
  out="$(run_check "$WORK/$name")" || rc=$?
  if [ "$rc" -ne 1 ]; then
    echo "FAIL: $name -> exit $rc (expected 1)" >&2
  elif ! printf '%s\n' "$out" | grep -q "^FAIL $class"; then
    echo "FAIL: $name -> output lacks 'FAIL $class'" >&2
  elif ! printf '%s\n' "$out" | grep -qF "$needle"; then
    echo "FAIL: $name -> output does not name '$needle'" >&2
  else
    pass=$((pass + 1))
    continue
  fi
  fail=1
  printf '%s\n' "$out" | sed 's/^/       /' >&2
done <<EOF
$CASES
EOF

# Fail closed without an initialised directory: no `tm`, or a `tm` whose init
# fails, is a FAIL INIT, never a skip.
for name in no-tm failing-tm; do
  bin="$WORK/$name-bin"
  mkdir -p "$bin"
  if [ "$name" = failing-tm ]; then
    printf '#!/bin/sh\necho "init refused" >&2\nexit 1\n' >"$bin/tm"
  else
    printf '#!/bin/sh\nexit 0\n' >"$bin/not-tm"
  fi
  chmod 755 "$bin"/*
  rc=0
  out="$(TM_BIN="$bin/tm" "$CHECK" --root "$clean/root" 2>&1)" || rc=$?
  if [ "$rc" -eq 1 ] && printf '%s\n' "$out" | grep -q '^FAIL INIT'; then
    pass=$((pass + 1))
  else
    echo "FAIL: $name -> exit $rc, expected 1 with FAIL INIT" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "check_architect_paths_selftest: FAILED ($pass passed)" >&2
  exit 1
fi
echo "check_architect_paths_selftest: OK ($pass cases)"
