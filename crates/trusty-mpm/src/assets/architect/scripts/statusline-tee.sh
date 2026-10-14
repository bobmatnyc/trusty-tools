#!/usr/bin/env bash
# statusline-tee.sh — Claude Code statusLine command: cache the rate limits, then render.
#
# Why: Claude Code passes `rate_limits` (five_hour / seven_day used_percentage and
# resets_at) in the statusLine stdin JSON; `tm statusline` renders only part of
# it, and the status line is cut off in narrow panes. The Architect needs the
# figures without typing /usage (the owner, 2026-10-07: track usage to plan cutovers).
# What: reads stdin once; when it carries rate_limits, writes
# inbox/statusline-usage.json (line 1: the logged-in account from .claude.json,
# the rest: the payload) atomically; then feeds the same stdin to
# `tm statusline` and returns its exit code. Any cache failure is ignored, so
# the status line never breaks. Used as the Architect's own `statusLine.command`
# (.claude/settings.json), because the payload is account-wide.
# Test: scripts/test_usage.py (statusline_tee_*)
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INBOX="${SUPERVISOR_INBOX_DIR:-$HERE/../inbox}"
CJ="${USAGE_CLAUDE_JSON:-${CLAUDE_CONFIG_DIR:-$HOME/.trusty-tools/trusty-mpm/claude-config}/.claude.json}"
TM="${TM_BIN:-$HOME/.cargo/bin/tm}"
payload=$(cat)
case "$payload" in
  *'"rate_limits"'*)
    {
      acct=$(grep -o '"emailAddress": *"[^"]*"' "$CJ" 2>/dev/null | head -1 | sed 's/.*: *"\(.*\)"/\1/')
      tmp="$INBOX/.statusline-usage.$$"
      printf '%s\n%s\n' "$acct" "$payload" > "$tmp" && mv "$tmp" "$INBOX/statusline-usage.json"
    } 2>/dev/null || true
    ;;
esac
printf '%s' "$payload" | "$TM" statusline "$@"
