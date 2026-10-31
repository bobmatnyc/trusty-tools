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
#      It never runs both, so it never starts a second Architect.
#   3. Managed PMs: for each name on the allowlist, a live tmux session means
#      nothing to do; otherwise `tm sessions resume <name>`. Only listed names
#      are resumed. The default list is exactly tm-rentalist-pro.
#   It never runs `tm stop`, `tm sessions ls` (plain `ls` prunes dead
#   records), launchctl, or anything that touches worktrees or Claude Desktop.
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
#   tm sessions resume  exit 0 resumed or "not paused"; non-zero not found or
#                       error (commands/session.rs). stdin is /dev/null, so a
#                       managed resume never attaches this terminal.
#
# Configuration (environment):
#   TM_RECOVER_HEALTH_TIMEOUT   seconds to wait for the daemon (default 120)
#   TM_RECOVER_HEALTH_INTERVAL  seconds between polls (default 2)
#   TM_RECOVER_ARCHITECT_SESSION  default tm-architect
#   TM_RECOVER_ALLOWLIST        space-separated session names (default
#                               tm-rentalist-pro); no wildcards
#   TM_RECOVER_ALLOWLIST_FILE   one name per line, `#` comments; overrides
#                               TM_RECOVER_ALLOWLIST
#   TM_RECOVER_LOG              default ~/.trusty-mpm/logs/login-recover.log
#   TM_RECOVER_PATH             the PATH tm and tmux are found on (default
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
if [[ "$TM" != /* || "$TMUX_BIN" != /* ]]; then
  log "error: tm or tmux not found on PATH=$PATH"
  exit 2
fi
readonly TM TMUX_BIN

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
if ! live "$ARCHITECT"; then
  log "architect: session $ARCHITECT absent; running tm fleet init"
  if "$TM" fleet init < /dev/null >> "$LOG" 2>&1; then
    log "architect: init ok"
  else
    log "error: tm fleet init failed"
    failed=1
  fi
else
  status_rc=0
  status_out="$("$TM" fleet status < /dev/null 2>&1)" || status_rc=$?
  if (( status_rc == 0 )); then
    log "architect: $ARCHITECT running and bound; nothing to do"
  elif [[ "$status_out" == *"the Architect is not bound"* ]]; then
    log "architect: $ARCHITECT running but unbound; running tm fleet relaunch"
    if "$TM" fleet relaunch < /dev/null >> "$LOG" 2>&1; then
      log "architect: relaunch ok"
    else
      log "error: tm fleet relaunch failed"
      failed=1
    fi
  else
    log "error: $ARCHITECT running but setup incomplete for another reason; left alone"
    printf '%s\n' "$status_out" >> "$LOG"
    failed=1
  fi
fi

# 3. Allowlisted managed PMs.
for name in ${allowlist[@]+"${allowlist[@]}"}; do
  if live "$name"; then
    log "pm: $name live; nothing to do"
    continue
  fi
  log "pm: $name not live; running tm sessions resume $name"
  if "$TM" sessions resume "$name" < /dev/null >> "$LOG" 2>&1; then
    log "pm: $name resumed"
  else
    log "error: tm sessions resume $name failed"
    failed=1
  fi
done

if (( failed )); then
  log "done: with errors"
  exit 2
fi
log "done: ok"
