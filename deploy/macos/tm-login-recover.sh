#!/bin/bash
# tm-login-recover.sh — restore the Architect and allowlisted managed PMs after login.
# #10597: slice S2 of login recovery.
#
# Why: a logout kills the tmux server, so the Architect and the managed PM
#   sessions are gone at the next login. Auto-resume is off by design
#   (TRUSTY_MPM_AUTO_RESUME=0), so nothing brings back the few sessions the
#   operator wants. This script brings back exactly those, and nothing else.
#
# What: every run decides from current state, so a second run changes nothing.
#   1. Polls `tm health` until it exits 0, for at most
#      TM_RECOVER_HEALTH_TIMEOUT seconds. On timeout it exits 1 and touches
#      no session.
#   2. Architect (tmux session TM_RECOVER_ARCHITECT_SESSION, default
#      tm-architect):
#        session absent                            -> `tm fleet init`
#        present, `tm fleet status` exits 0        -> nothing
#        present, status says "not bound"          -> `tm fleet relaunch`
#        present, status incomplete for any other reason -> nothing, exit 2
#      It never runs both, so it never starts a second Architect. After an
#      init or relaunch that exits 0 it polls, bounded, until `tm fleet status`
#      exits 0 AND the Architect's tmux session is present; otherwise the step
#      failed. An exit 0 alone proves nothing.
#   3. Managed PMs: for each name on the allowlist, a live tmux session means
#      nothing to do. Otherwise it reads `tm sessions ls --no-prune --json`
#      once, finds the one record whose `name` EXACTLY equals the listed name,
#      and runs `tm sessions resume <that record's id>`. No exact record, or
#      more than one, resumes nothing and fails. Resuming by name would let
#      the resolver's unambiguous-prefix match pick tm-rentalist-pro-2 (#10597).
#      After the resume it polls, bounded, until the tmux session is live.
#      Only listed names are resumed. The default list is exactly
#      tm-rentalist-pro.
#   It never runs `tm stop`, plain `tm sessions ls` (it prunes dead records),
#   launchctl, or anything that touches worktrees or Claude Desktop.
#   It reads no .env, Keychain item or token and exports nothing.
#
# Exit codes: 0 all done or nothing to do; 1 the daemon never became healthy;
#   2 a bad allowlist, or a recovery step that failed or was left undecided.
#
# Command contracts (crates/trusty-mpm/src/bin/tm):
#   tm health           exit 0 healthy; exit 1 socket absent or daemon down
#                       (commands/misc.rs `health`).
#   tm fleet status     exit 0 when `complete`; exit 1 otherwise. An unbound
#                       but set-up Architect prints "incomplete: the Architect
#                       is not bound; run `tm fleet relaunch ...`"
#                       (commands/fleet/status.rs `render`, fleet/mod.rs `run`).
#   tm fleet init       idempotent; refuses a second Architect; exit 1 on a
#                       failed step (fleet/mod.rs).
#   tm fleet relaunch   exit 1 with no live session; refuses inside Claude
#                       Code (#10033) (fleet/relaunch.rs).
#   tm sessions ls --no-prune --json
#                       the daemon's raw body, {"sessions":[{"id","name",
#                       "state",...}]}; --no-prune disables the listing prune
#                       (commands/managed.rs `session_ls`, ManagedListResponse
#                       in client/http_client/types.rs).
#   tm sessions resume  exit 0 resumed or "not paused"; non-zero not found or
#                       error (commands/session.rs). The target resolves exact
#                       id, then exact name, then a name prefix
#                       (client/resolver.rs), so the script passes an id.
#                       stdin is /dev/null, so a managed resume never attaches
#                       this terminal.
#
# Configuration (environment):
#   TM_RECOVER_HEALTH_TIMEOUT   seconds to wait for the daemon (default 120)
#   TM_RECOVER_HEALTH_INTERVAL  seconds between polls (default 2)
#   TM_RECOVER_ARCHITECT_SESSION  default tm-architect
#   TM_RECOVER_ALLOWLIST        space-separated session names (default
#                               tm-rentalist-pro); no wildcards
#   TM_RECOVER_ALLOWLIST_FILE   one name per line, `#` comments; overrides
#                               TM_RECOVER_ALLOWLIST
#   TM_RECOVER_ARCHITECT_VERIFY_TIMEOUT  seconds for init/relaunch to take
#                               effect (default 60)
#   TM_RECOVER_RESUME_VERIFY_TIMEOUT  seconds for a resumed session to go live
#                               (default 30)
#   TM_RECOVER_VERIFY_INTERVAL  seconds between postcondition polls (default 1)
#   TM_RECOVER_LOG              default ~/.trusty-mpm/logs/login-recover.log
#   TM_RECOVER_PATH             the PATH tm, tmux and python3 are found on (default
#                               ~/.cargo/bin, ~/.local/bin, Homebrew, system)
#
# Install as a macOS Login Item that runs in Terminal:
#   1. chmod +x this file and copy it to e.g. ~/bin/tm-login-recover.command
#      (the .command extension makes Finder open it in Terminal).
#   2. System Settings > General > Login Items > Open at Login > "+", pick
#      that file.
#   It must run in Terminal, not from launchd: the tmux server it starts is
#   born in the login session, and Keychain access from a launchd-born tmux
#   is unproven.
#   Depends on the S0 LaunchAgent, which starts the tm daemon at login with
#   TRUSTY_MPM_AUTO_RESUME=0. This script waits for that daemon.
#
# Test: deploy/macos/tests/test_tm_login_recover.py (stub tm and tmux, tmp HOME):
#   python3 -m pytest deploy/macos/tests/test_tm_login_recover.py -q -p no:cacheprovider

set -euo pipefail

readonly DATE=/bin/date
readonly MKDIR=/bin/mkdir
readonly SLEEP=/bin/sleep

HEALTH_TIMEOUT="${TM_RECOVER_HEALTH_TIMEOUT:-120}"
HEALTH_INTERVAL="${TM_RECOVER_HEALTH_INTERVAL:-2}"
ARCHITECT_VERIFY_TIMEOUT="${TM_RECOVER_ARCHITECT_VERIFY_TIMEOUT:-60}"
RESUME_VERIFY_TIMEOUT="${TM_RECOVER_RESUME_VERIFY_TIMEOUT:-30}"
VERIFY_INTERVAL="${TM_RECOVER_VERIFY_INTERVAL:-1}"
ARCHITECT="${TM_RECOVER_ARCHITECT_SESSION:-tm-architect}"
LOG="${TM_RECOVER_LOG:-$HOME/.trusty-mpm/logs/login-recover.log}"
PATH="${TM_RECOVER_PATH:-$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin}"

$MKDIR -p "${LOG%/*}"

log() {
  local line
  line="$($DATE -u +%Y-%m-%dT%H:%M:%SZ) $*"
  printf '%s\n' "$line" >> "$LOG"
  printf '%s\n' "$line"
}

# Resolve each tool once, to an absolute path.
TM="$(command -v tm || true)"
TMUX_BIN="$(command -v tmux || true)"
PYTHON="$(command -v python3 || true)"
if [[ "$TM" != /* || "$TMUX_BIN" != /* || "$PYTHON" != /* ]]; then
  log "error: tm, tmux or python3 not found on PATH=$PATH"
  exit 2
fi
readonly TM TMUX_BIN PYTHON

# The allowlist: exact names only.
allowlist=()
if [[ -n "${TM_RECOVER_ALLOWLIST_FILE:-}" ]]; then
  while IFS= read -r entry || [[ -n "$entry" ]]; do
    entry="${entry%%#*}"
    entry="${entry//[[:space:]]/}"
    [[ -n "$entry" ]] && allowlist+=("$entry")
  done < "$TM_RECOVER_ALLOWLIST_FILE"
else
  read -r -a allowlist <<< "${TM_RECOVER_ALLOWLIST:-tm-rentalist-pro}"
fi
# bash 3.2 (/bin/bash) treats an empty "${a[@]}" as unbound under set -u.
for name in "$ARCHITECT" ${allowlist[@]+"${allowlist[@]}"}; do
  if [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    log "error: session name '$name' is not an exact name; nothing done"
    exit 2
  fi
done

live() {
  "$TMUX_BIN" has-session -t "=$1" 2>/dev/null
}

# True once "$@" succeeds; polls every VERIFY_INTERVAL for at most $1 seconds.
poll_until() {
  local timeout="$1" begin=$SECONDS
  shift
  until "$@"; do
    if (( SECONDS - begin >= timeout )); then
      return 1
    fi
    $SLEEP "$VERIFY_INTERVAL"
  done
}

architect_up() {
  "$TM" fleet status < /dev/null > /dev/null 2>&1 && live "$ARCHITECT"
}

# Prints the id of every record whose name EXACTLY equals argv[1], one per line.
readonly PICK_IDS='import json, sys
body = json.load(sys.stdin)
rows = body.get("sessions") if isinstance(body, dict) else None
for row in rows or []:
    if isinstance(row, dict) and row.get("name") == sys.argv[1]:
        print(row.get("id") or "")'

failed=0

# 1. Wait for the daemon.
log "start: waiting up to ${HEALTH_TIMEOUT}s for tm health"
start=$SECONDS
until "$TM" health < /dev/null > /dev/null 2>&1; do
  if (( SECONDS - start >= HEALTH_TIMEOUT )); then
    log "error: tm health not ok after ${HEALTH_TIMEOUT}s; no session touched"
    exit 1
  fi
  $SLEEP "$HEALTH_INTERVAL"
done
log "daemon: healthy"

# 2. The Architect.
architect_action=""
if ! live "$ARCHITECT"; then
  log "architect: session $ARCHITECT absent; running tm fleet init"
  architect_action=init
else
  status_rc=0
  status_out="$("$TM" fleet status < /dev/null 2>&1)" || status_rc=$?
  if (( status_rc == 0 )); then
    log "architect: $ARCHITECT running and bound; nothing to do"
  elif [[ "$status_out" == *"the Architect is not bound"* ]]; then
    log "architect: $ARCHITECT running but unbound; running tm fleet relaunch"
    architect_action=relaunch
  else
    log "error: $ARCHITECT running but setup incomplete for another reason; left alone"
    printf '%s\n' "$status_out" >> "$LOG"
    failed=1
  fi
fi
if [[ -n "$architect_action" ]]; then
  if ! "$TM" fleet "$architect_action" < /dev/null >> "$LOG" 2>&1; then
    log "error: tm fleet $architect_action failed"
    failed=1
  elif poll_until "$ARCHITECT_VERIFY_TIMEOUT" architect_up; then
    log "architect: verified; tm fleet status complete and $ARCHITECT present"
  else
    log "error: postcondition failed: tm fleet $architect_action exited 0 but within" \
      "${ARCHITECT_VERIFY_TIMEOUT}s tm fleet status was not complete or $ARCHITECT was absent"
    failed=1
  fi
fi

# 3. Allowlisted managed PMs.
listing=""
listed=0
for name in ${allowlist[@]+"${allowlist[@]}"}; do
  if live "$name"; then
    log "pm: $name live; nothing to do"
    continue
  fi
  if (( listed == 0 )); then
    listed=1
    if ! listing="$("$TM" sessions ls --no-prune --json < /dev/null 2>> "$LOG")"; then
      log "error: tm sessions ls --no-prune --json failed; no session resumed"
      listing=""
    fi
  fi
  if [[ -z "$listing" ]]; then
    log "error: no session listing; $name not resumed"
    failed=1
    continue
  fi
  ids=()
  if ! picked="$(printf '%s' "$listing" | "$PYTHON" -I -c "$PICK_IDS" "$name" 2>> "$LOG")"; then
    log "error: session listing is not the expected JSON; $name not resumed"
    failed=1
    continue
  fi
  while IFS= read -r id; do
    [[ -n "$id" ]] && ids+=("$id")
  done <<< "$picked"
  if (( ${#ids[@]} == 0 )); then
    log "error: no exact record for $name; nothing resumed"
    failed=1
    continue
  fi
  if (( ${#ids[@]} > 1 )); then
    log "error: ambiguous: ${#ids[@]} records named exactly $name; nothing resumed"
    failed=1
    continue
  fi
  id="${ids[0]}"
  if [[ ! "$id" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]*$ ]]; then
    log "error: record id '$id' for $name is malformed; nothing resumed"
    failed=1
    continue
  fi
  log "pm: $name not live; running tm sessions resume $id (exact record)"
  if ! "$TM" sessions resume "$id" < /dev/null >> "$LOG" 2>&1; then
    log "error: tm sessions resume $id ($name) failed"
    failed=1
  elif poll_until "$RESUME_VERIFY_TIMEOUT" live "$name"; then
    log "pm: $name verified live"
  else
    log "error: resume exited 0 but $name is not live after ${RESUME_VERIFY_TIMEOUT}s"
    failed=1
  fi
done

if (( failed )); then
  log "done: with errors"
  exit 2
fi
log "done: ok"
