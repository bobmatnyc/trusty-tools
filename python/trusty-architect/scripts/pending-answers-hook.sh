#!/bin/sh
# pending-answers-hook.sh: PostToolUse and UserPromptSubmit hook for the Architect session only.
# Why: an owner answer on the dashboard must be relayed within one pass.
# What: pending_hook_dedupe.py injects one additionalContext line when the pending set changed
# (or every 15 min while it stays pending), plus unread PM wake messages (pm_cursor.py); otherwise
# prints nothing. Subagent calls (agent_id in the hook input) are ignored. Never blocks a tool call.
# #10571: the script runs from the directory it lives in, so no checkout path ships in this file.
exec python3 "$(dirname "$0")/pending_hook_dedupe.py" 2>/dev/null || exit 0
