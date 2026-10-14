#!/bin/sh
# datg-hook.sh: SessionStart and PreToolUse(Agent|Task) hook for a DATG pilot PM (the owner D93, 2026-10-09).
# Why: the PM must see its goal rules at session start and be warned (later blocked) when a
# dispatch names no goal id. Read-only: never writes state.
# What: passes the hook JSON on stdin to `datg.py hook`; prints its JSON or nothing. Every
# failure path exits 0 with no output (fail open); the block is a documented permissionDecision.
R="$(cd "$(dirname "$0")/.." && pwd)"  # this checkout (#8436)
python3 "$R/scripts/datg.py" hook 2>/dev/null
exit 0
