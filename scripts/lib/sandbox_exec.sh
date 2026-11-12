#!/usr/bin/env bash
#
# sandbox_exec.sh — the credential launch wrapper for scripts/sandbox_daemon.sh
# (issue #10721). Never run by hand.
#
# Why: `scripts/sandbox_daemon.sh --credential NAME=<ref>` hands the sandbox
#   daemon one provider key by vault reference. `tm secrets exec` resolves the
#   reference and sets NAME in this wrapper's environment, so the value never
#   reaches an argv or a file. That exec runs with the caller's environment
#   (it needs the caller's HOME and project checkout to reach the vault), so
#   this wrapper must clear everything else before the daemon starts.
# What: usage `sandbox_exec.sh SANDBOX BIN NAME`. Refuses unless NAME is
#   ANTHROPIC_API_KEY or OPENROUTER_API_KEY and is set. Then, in order: cd to
#   SANDBOX/home (#9161), unset every exported variable except PATH, NAME and
#   the FORWARDED names below (Architect ruling Q1: an unset loop in place of
#   `env -i`, since `env -i NAME=...` would put the value on argv), unset every
#   exported function, set the four pinned paths, and exec
#     env -u _ -u SHLVL BIN daemon --sandbox --sandbox-credential NAME
#   `env -u _ -u SHLVL` drops the `_` and SHLVL bash re-adds on exec. The
#   daemon's own closed allowlist (`ENV_ALLOWLIST` in daemon_sandbox.rs)
#   refuses anything this loop missed; it is the backstop, not the filter.
#   Residual risk (Architect ruling Q3, accepted): from `tm secrets exec`
#   until the daemon reads and removes NAME, a process of the same user can
#   read the value from this process's environment (`ps eww`). Output names
#   variables only, never a value.
# Exit: 1 on a refusal; otherwise the exec'd daemon's status.
#
# Test: scripts/sandbox_daemon_selftest.sh (the `cred-*` cases).
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

set -euo pipefail

refuse() {
  echo "sandbox_exec: refused: $*" >&2
  exit 1
}

[ "$#" -eq 3 ] || refuse "usage: sandbox_exec.sh SANDBOX BIN NAME"
SANDBOX="$1"
BIN="$2"
NAME="$3"
# #10721: keep in step with SANDBOX_CREDENTIAL_NAMES in secret_source.rs.
case "$NAME" in
  ANTHROPIC_API_KEY|OPENROUTER_API_KEY) ;;
  *) refuse "credential name not allowed: $NAME (allowed: ANTHROPIC_API_KEY, OPENROUTER_API_KEY)" ;;
esac
# Fail closed: no fallback to any other source of the value.
[ -n "${!NAME:-}" ] || refuse "$NAME is not set; tm secrets exec did not inject it"
[ -x "$BIN" ] || refuse "not an executable file: $BIN"

# Keep in step with FORWARDED in scripts/sandbox_daemon.sh (the selftest
# compares the two lists).
FORWARDED="LANG LC_ALL LC_COLLATE LC_CTYPE LC_MESSAGES LC_MONETARY LC_NUMERIC LC_TIME \
LC_ADDRESS LC_IDENTIFICATION LC_MEASUREMENT LC_NAME LC_PAPER LC_TELEPHONE \
LOGNAME RUST_LOG SHELL TERM TMPDIR USER"
KEEP=" PATH $NAME $FORWARDED "
# This script's own variables: a caller that exported one of these names must
# not leak it past the loop, nor have the loop unset the script's copy.
export -n SANDBOX BIN NAME FORWARDED KEEP var fn

# #9161: before the unset loop, so the PWD and OLDPWD it sets are cleared too.
cd "$SANDBOX/home" || refuse "cannot enter <dir>/home: $SANDBOX/home"

for var in $(compgen -e); do
  case "$KEEP" in
    *" $var "*) ;;
    *) unset "$var" 2>/dev/null || refuse "cannot clear $var" ;;
  esac
done
# Exported functions travel as BASH_FUNC_* entries that `compgen -e` omits.
for fn in $(declare -F -x | awk '{print $3}'); do
  unset -f "$fn"
done

export HOME="$SANDBOX/home"
export TRUSTY_DATA_DIR_OVERRIDE="$SANDBOX/data"
export TRUSTY_SANDBOX=1
export TMUX_TMPDIR="$SANDBOX/data/tmux"

exec env -u _ -u SHLVL "$BIN" daemon --sandbox --sandbox-credential "$NAME"
