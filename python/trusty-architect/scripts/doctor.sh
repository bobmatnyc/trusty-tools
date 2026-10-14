#!/usr/bin/env bash
# doctor.sh — post-install check of one Architect directory. Read-only.
#
# Why: a fresh install is only complete when every piece the Architect was built
# with is in place (owner ruling 2026-10-07); this names the pieces that are not.
# What: prints OK/FAIL per check and exits 1 on any FAIL. Repo checks always run;
# host checks (tm grant, poller session) are skipped with --no-host. The guard
# lift is the owner's choice (#8436, ruling Dca A), so its state is reported as
# INFO, never as a failure. The operating manual is composed by tm, so no
# manual file is required here.
# Test: scripts/test_init.py runs it after init.sh.
# Usage: scripts/doctor.sh [--no-host]
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
HOST=1; [ "${1:-}" = "--no-host" ] && HOST=0
bad=0
check() {  # check <label> <command...>
  local label="$1"; shift
  if "$@" >/dev/null 2>&1; then echo "OK       $label"; else echo "FAIL     $label"; bad=1; fi
}
setting() {  # setting <python expr over d>
  python3 -c "import json,sys; d=json.load(open('.claude/settings.json')); sys.exit(0 if ($1) else 1)"
}
for f in local/state.md local/actions.md local/goals.md \
  local/improvements/ledger.jsonl local/usage/ledger.jsonl \
  local/.gitignore AGENTS.md CLAUDE.md; do
  check "file $f" test -e "$f"
done
check "settings statusLine -> statusline-tee.sh" setting "'statusline-tee.sh' in (d.get('statusLine') or {}).get('command','')"
if setting "d.get('env',{}).get('TRUSTY_MPM_PM_UNRESTRICTED')=='1'" >/dev/null 2>&1; then
  echo "INFO     guard lift: lifted (owner choice; scripts/init.sh --guard-lift)"
else
  echo "INFO     guard lift: off"
fi
for f in .claude/agents/architect.md .claude/agents/improvement-curator.md .claude/agents/token-optimizer.md \
  .claude/skills/architect-improvements/SKILL.md .claude/skills/architect-token-optimizer/SKILL.md; do
  check "tracked $f" test -f "$f"
done
if [ "$HOST" = 1 ]; then
  check "tm supervisor grant for $ROOT" grep -qs "$ROOT" "$HOME/.trusty-mpm/config.toml"
  check "tmux session tm-architect-poll" tmux has-session -t '=tm-architect-poll'
fi
exit $bad
