#!/usr/bin/env bash
# usage-probe.sh <session> <pane> <account> — read /usage in one idle PM pane into the ledger.
#
# Why: the status-line cache is the no-typing source, but it only exists while the
# Architect session renders; /usage is the verified fallback (architect-host-admin "Resource
# reporting"), and it is needed right after an account change.
# What: refuses unless input-state.py reports `empty` or `suggestion` for the pane
# and no turn is running; types `/usage` (rc_send_cmd: echo-verified, then Enter),
# waits for the "Resets" lines, captures, sends Escape, and pipes the capture to
# `usage.py ingest --source usage-cmd --account <account>`. Never touches a draft.
# Test: scripts/test_usage.py (usage_probe_*) against a fake tmux.
set -u
s="${1:?session}"; pane="${2:?pane}"; account="${3:?account}"
SUPERVISOR_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$SUPERVISOR_DIR/scripts/rc-lib.sh"
t="$pane"
log() { echo "$(date -u +%FT%TZ) usage-probe $s $pane: $1"; }
state=$(python3 "$RC_IS" "$t" | cut -d' ' -f2)
case "$state" in empty|suggestion) ;; *) log "skip: input box is $state"; exit 2 ;; esac
_rc_cap | grep -q 'esc to interrupt' && { log "skip: turn running"; exit 2; }
rc_send_cmd "/usage" || { log "fail: /usage not echoed"; exit 1; }
cap=""
for _ in $(seq 1 "${USAGE_WAIT_TRIES:-10}"); do
  sleep "${USAGE_WAIT_SLEEP:-1}"
  cap=$(_rc_cap)
  printf '%s\n' "$cap" | grep -q 'Resets' && break
done
_rc_tmux send-keys -t "$t" Escape
if printf '%s\n' "$cap" | python3 "$SUPERVISOR_DIR/scripts/usage.py" ingest --source usage-cmd --account "$account"; then
  log "ok"
else
  log "fail: no /usage figures in the capture"; exit 1
fi
