#!/usr/bin/env bash
# init.sh — prepare an Architect directory for first use. Safe to re-run.
#
# Why: the manual path beside `tm fleet init` (#8436): from the shipped files
# alone it must leave the same tree `tm fleet init` seeds, and it must never
# lift the PM guard unless the owner asks (ruling Dca A: default off).
# What: creates inbox/ and the local/ skeleton (templates copied without
# overwriting), the improvement and usage ledgers and local/.gitignore, links
# inbox/briefs to local/briefs, and merges into .claude/settings.json only
# when absent: statusLine -> scripts/statusline-tee.sh, and with --guard-lift
# (or ARCHITECT_GUARD_LIFT=1) env.TRUSTY_MPM_PM_UNRESTRICTED=1. A settings file
# that is not a JSON object fails the run and is left as it was. Installs the
# notify hook in each watch-set project it can resolve, prints (never writes)
# the tm supervisor grant line, then runs scripts/doctor.sh. With --start it
# also runs scripts/architect-start.sh.
# Test: scripts/test_init.py; `init_sh_and_tm_fleet_init_produce_the_same_tree`
# in trusty-mpm compares this tree with the one `tm fleet init` seeds.
# Usage: scripts/init.sh [--start] [--guard-lift]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
START=0
LIFT="${ARCHITECT_GUARD_LIFT:-0}"
for arg in "$@"; do
  case "$arg" in
    --start) START=1 ;;
    --guard-lift) LIFT=1 ;;
    *) echo "usage: scripts/init.sh [--start] [--guard-lift]" >&2; exit 2 ;;
  esac
done

mkdir -p inbox local/briefs local/archive local/mandates local/improvements/runs local/usage
# Briefs are durable records in local/; inbox/briefs stays as a link because
# PM sessions hold absolute paths into it (SPEC-ARCHITECT-01 D).
if [ ! -e inbox/briefs ] && [ ! -L inbox/briefs ]; then
  ln -s ../local/briefs inbox/briefs; echo "linked   inbox/briefs -> ../local/briefs"
else
  echo "kept     inbox/briefs"
fi

copy() {  # copy <template> <dest>, never overwriting
  if [ -e "$2" ]; then echo "kept     $2"; else cp "$1" "$2"; echo "created  $2"; fi
}
# AGENTS.md and CLAUDE.md ship with the files; this script does not copy them.
# The operating manual is composed into the instructions by tm, not a file here.
copy templates/state.md local/state.md
copy templates/actions.md local/actions.md
copy templates/goals.md local/goals.md
copy templates/local-gitignore local/.gitignore
for f in local/improvements/ledger.jsonl local/usage/ledger.jsonl; do
  if [ -e "$f" ]; then echo "kept     $f"; else : > "$f"; echo "created  $f"; fi
done

mkdir -p .claude
python3 - .claude/settings.json "$ROOT/scripts/statusline-tee.sh" "$LIFT" <<'PYEOF'
import json, os, sys
path, tee, lift = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
data = {}
if os.path.lexists(path):
    if os.path.islink(path):
        sys.exit(f"init.sh: {path} is a symlink; not written")
    raw = open(path).read().strip()
    try:
        data = json.loads(raw) if raw else {}
    except ValueError as err:
        sys.exit(f"init.sh: {path} is not valid JSON ({err}); not written")
    if not isinstance(data, dict):
        sys.exit(f"init.sh: {path} is not a JSON object; not written")
changed = []
if lift:
    env = data.setdefault("env", {})
    if not isinstance(env, dict):
        sys.exit(f"init.sh: {path} has an env that is not an object; not written")
    if env.get("TRUSTY_MPM_PM_UNRESTRICTED") != "1":
        env["TRUSTY_MPM_PM_UNRESTRICTED"] = "1"
        changed.append("TRUSTY_MPM_PM_UNRESTRICTED=1 (guard lift, owner's choice)")
want = {"type": "command", "command": tee, "padding": 0}
cur = data.get("statusLine")
if cur is None:
    data["statusLine"] = want
    changed.append("statusLine")
elif cur != want:
    print(f"kept     {path} statusLine (existing value; would set {json.dumps(want)})")
if changed:
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")
    os.replace(tmp, path)
    print(f"updated  {path} ({', '.join(changed)})")
else:
    print(f"kept     {path}")
PYEOF

# Install the notify hook in each watch-set project whose directory tmux can show.
watch=$(awk '/^## Watch set/{w=1;next} /^## /{w=0} w && /^- /{print $2}' local/state.md)
for s in $watch; do
  case "$s" in "<"*) continue;; esac
  d=$(tmux list-panes -t "=$s" -F '#{pane_current_path}' 2>/dev/null | head -1 || true)
  if [ -n "$d" ] && [ -d "$d/.claude" ]; then
    scripts/install-notify-hook.sh "$d" | sed "s|^|hook     $s: |"
  else
    echo "skipped  hook for $s (session or project dir not found; run scripts/install-notify-hook.sh <dir>)"
  fi
done

# The supervisor grant lives in the user's tm config; print it, never edit it.
if ! grep -qs "$ROOT" "$HOME/.trusty-mpm/config.toml"; then
  cat <<EOF

Grant (add to ~/.trusty-mpm/config.toml, or run: tm fleet init --no-launch):
  [supervisor]
  projects = ["$ROOT"]
  session = "tm-architect"
EOF
fi

scripts/doctor.sh || true
[ "$START" = 1 ] && scripts/architect-start.sh

cat <<EOF

Next (full checklist: templates/README.md, "Post-launch"):
  1. Fill in AGENTS.md, then edit local/state.md: the watch set; re-run scripts/init.sh.
  2. Launch: tm fleet init (or tm sessions resume tm-architect), then scripts/architect-start.sh [--rc].
  3. In the Architect session: /remote-control tm-architect; create the daily cron; arm one inbox Monitor.
  4. Palaces: memory_palace_create "architect" and "owner" (MCP call, not shell).
  5. Logins you complete yourself: Claude, gh, Full Disk Access for the claude binary.
EOF
