#!/usr/bin/env bash
# install-notify-hook.sh — idempotently add/remove the push-notification
# Notification hook in a watched project's .claude/settings.json.
#
# Usage:
#   scripts/install-notify-hook.sh <project-dir>            # add/update
#   scripts/install-notify-hook.sh <project-dir> --remove   # remove

set -euo pipefail

PROJECT_DIR="${1:?usage: install-notify-hook.sh <project-dir> [--remove]}"
MODE="install"
if [ "${2:-}" = "--remove" ]; then MODE="remove"; fi

SETTINGS="$PROJECT_DIR/.claude/settings.json"
# Absolute path of the receiver in THIS checkout. The hook stores it verbatim,
# so moving the supervisor checkout means re-running this installer.
NOTIFY_CMD="$(cd "$(dirname "$0")" && pwd)/notify-supervisor.sh"

if [ ! -d "$PROJECT_DIR/.claude" ]; then
  echo "error: $PROJECT_DIR/.claude does not exist — not a Claude Code project" >&2
  exit 1
fi

python3 - "$SETTINGS" "$NOTIFY_CMD" "$MODE" <<'PYEOF'
import json, os, sys

settings_path, notify_cmd, mode = sys.argv[1], sys.argv[2], sys.argv[3]
MATCHER = "permission_prompt|idle_prompt|agent_needs_input"

if os.path.exists(settings_path):
    raw = open(settings_path).read().strip()
    data = json.loads(raw) if raw else {}
else:
    data = {}

def is_our_entry(entry):
    return any(
        h.get("type") == "command" and h.get("command") == notify_cmd
        for h in entry.get("hooks", [])
    )

hooks = data.get("hooks", {})
notif = hooks.get("Notification", [])
# Drop any prior entry this installer wrote, regardless of matcher, so a
# re-run always converges instead of duplicating or leaving a stale matcher.
notif = [e for e in notif if not is_our_entry(e)]

if mode == "install":
    notif.append({
        "matcher": MATCHER,
        "hooks": [{"type": "command", "command": notify_cmd, "timeout": 5}],
    })
    hooks["Notification"] = notif
    data["hooks"] = hooks
elif mode == "remove":
    if notif:
        hooks["Notification"] = notif
        data["hooks"] = hooks
    elif "Notification" in hooks:
        del hooks["Notification"]
        if hooks:
            data["hooks"] = hooks
        elif "hooks" in data:
            del data["hooks"]
else:
    print(f"error: unknown mode {mode}", file=sys.stderr)
    sys.exit(2)

os.makedirs(os.path.dirname(settings_path), exist_ok=True)
tmp = settings_path + ".tmp"
with open(tmp, "w") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")
os.replace(tmp, settings_path)
print(f"{'installed' if mode == 'install' else 'removed'}: {settings_path}")
PYEOF
