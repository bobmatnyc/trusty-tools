#!/usr/bin/env bash
# notify-supervisor.sh — Claude Code Notification hook receiver.
#
# Installed (via install-notify-hook.sh) as the command of a `Notification`
# hook entry in a WATCHED project's .claude/settings.json. Reads the hook's
# stdin JSON, appends one line to this supervisor project's inbox, and always
# exits 0 fast. Must never block or fail the watched session: no network
# calls, no long-running commands, best-effort only, fail silent.
#
# Run as `/bin/bash notify-supervisor.sh`; do not exec (D73, #10263). The
# shebang stays for manual use only.

set -u
# Deliberately no `set -e` — every failure path below must still reach the
# final `exit 0`.

# The inbox lives in this supervisor checkout, next to scripts/. Resolved from
# this script's own location so the repo works from any path; override with
# ARCHITECT_INBOX_DIR, then SUPERVISOR_INBOX_DIR (used by tests to avoid writing the live inbox).
SCRIPT_DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || exit 0
INBOX_DIR="${ARCHITECT_INBOX_DIR:-${SUPERVISOR_INBOX_DIR:-$(dirname "$SCRIPT_DIR")/inbox}}"
INBOX_FILE="$INBOX_DIR/events.jsonl"

# Self-timeout: macOS ships neither GNU `timeout` nor `gtimeout` by default,
# so this is best-effort — if either is on PATH (e.g. via `brew install
# coreutils`), run the real work under it as a child so a hang anywhere below
# (tmux, jq, python3) cannot outlive 2s. The real backstop is this hook's own
# "timeout": 5 in settings.json, which Claude Code enforces regardless of
# what this script does.
if [ -z "${NOTIFY_SUPERVISOR_REEXEC:-}" ]; then
  export NOTIFY_SUPERVISOR_REEXEC=1
  # Deliberately NOT `exec` here: `exec timeout 4 "$0"` would replace this
  # process, so a kill-on-timeout makes timeout's own 124 THIS script's exit
  # code — breaking the "always exit 0" contract. Run the re-invocation as a
  # child instead and discard its exit status so this wrapper always exits 0.
  # #10263: re-run through /bin/bash, never a direct #! exec of "$0" (D73).
  if command -v timeout >/dev/null 2>&1; then
    timeout 4 /bin/bash "$0" "$@"
    exit 0
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout 4 /bin/bash "$0" "$@"
    exit 0
  fi
fi

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

mkdir -p "$INBOX_DIR" 2>/dev/null || exit 0

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

tmux_session=""
tmux_pane=""
if { [ -n "${TMUX_PANE:-}" ] || [ -n "${TMUX:-}" ]; } && command -v tmux >/dev/null 2>&1; then
  # Target this hook's own pane explicitly. Without -t, display-message
  # answers for the client's current pane, which can be a different session.
  if [ -n "${TMUX_PANE:-}" ]; then
    tmux_session="$(tmux display-message -p -t "$TMUX_PANE" '#S' 2>/dev/null || true)"
    tmux_pane="$TMUX_PANE"
  else
    tmux_session="$(tmux display-message -p '#S' 2>/dev/null || true)"
    tmux_pane="$(tmux display-message -p '#{pane_id}' 2>/dev/null || true)"
  fi
fi

# Project name: the Claude Code project root, not the shell's cwd, which can
# be a subdirectory (e.g. my-app/services reported as "services").
project_root="${CLAUDE_PROJECT_DIR:-}"
project_name=""
[ -n "$project_root" ] && project_name="$(basename "$project_root")"

if command -v jq >/dev/null 2>&1; then
  line="$(printf '%s' "$payload" | jq -c \
    --arg ts "$ts" \
    --arg tmux_session "$tmux_session" \
    --arg tmux_pane "$tmux_pane" \
    --arg project_name "$project_name" \
    '{
      ts: $ts,
      session_id: (.session_id // null),
      cwd: (.cwd // null),
      project: (if $project_name != "" then $project_name else ((.cwd // "") | split("/") | last) end),
      tmux_session: (if $tmux_session == "" then null else $tmux_session end),
      tmux_pane: (if $tmux_pane == "" then null else $tmux_pane end),
      type: (.notification_type // null),
      message: (.message // null)
    } as $base
    | $base + {
        display: ("[" +
          (if $tmux_session != "" then $tmux_session
           elif ($base.project // "") != "" then $base.project
           else "unknown-session" end)
          + "]: " + ($base.message // ""))
      }' 2>/dev/null)"
else
  line="$(NOTIFY_PROJECT="$project_name" NOTIFY_TS="$ts" NOTIFY_TMUX_SESSION="$tmux_session" NOTIFY_TMUX_PANE="$tmux_pane" python3 -c '
import json, os, sys
try:
    data = json.loads(sys.stdin.read() or "{}")
except Exception:
    data = {}
cwd = data.get("cwd") or ""
project = os.environ.get("NOTIFY_PROJECT") or (cwd.rstrip("/").split("/")[-1] if cwd else None)
tmux_session = os.environ.get("NOTIFY_TMUX_SESSION") or None
message = data.get("message")
display_name = tmux_session or project or "unknown-session"
out = {
    "ts": os.environ.get("NOTIFY_TS"),
    "session_id": data.get("session_id"),
    "cwd": data.get("cwd"),
    "project": project,
    "tmux_session": tmux_session,
    "tmux_pane": os.environ.get("NOTIFY_TMUX_PANE") or None,
    "type": data.get("notification_type"),
    "message": message,
    "display": "[{}]: {}".format(display_name, message or ""),
}
print(json.dumps(out))
' <<<"$payload" 2>/dev/null)"
fi

[ -n "$line" ] || exit 0
printf '%s\n' "$line" >> "$INBOX_FILE" 2>/dev/null
exit 0
