#!/bin/sh
# pending-answers-hook.sh: PostToolUse hook for the Architect session only (the owner D54, 2026-10-09).
# Why: an owner answer on the dashboard must be relayed within one pass (r-183).
# What: pending_hook_dedupe.py injects one additionalContext line when the pending set changed
# (or every 15 min while it stays pending); otherwise prints nothing. Subagent calls (agent_id in
# the hook input) are ignored. Never blocks a tool call.
exec python3 "$(dirname "$0")/pending_hook_dedupe.py" 2>/dev/null || exit 0
