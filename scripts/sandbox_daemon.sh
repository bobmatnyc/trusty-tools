#!/usr/bin/env bash
#
# sandbox_daemon.sh — start an isolated `tm daemon --sandbox` for a live check
# (issue #9121).
#
# Why: a qa live-check hand-built a "sandbox" daemon with its own HOME and
#   port. It inherited TELEGRAM_BOT_TOKEN and polled the real Telegram bot
#   (TerminatedByOtherGetUpdates). A fresh HOME is not isolation: every
#   exported credential is inherited, and the Keychain belongs to the OS user,
#   not to HOME. This script is the one sanctioned way to start an isolated
#   daemon; hand-built `tm daemon` sandboxes are forbidden.
# What: validates, then execs, in the foreground:
#     env -i HOME=<dir>/home PATH=<caller's PATH> \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data TRUSTY_MPM_ADDR=127.0.0.1:<port> \
#            <tm> daemon --sandbox
#   No other variable reaches the daemon. The daemon refuses on its own too:
#   any *_TOKEN / *_KEY variable, a missing data-dir override, or HOME equal to
#   the password-database home. The real home is read from the password
#   database (dscl on macOS, getent elsewhere), never from $HOME.
#   Output names variables and the paths this script chose; it never prints a
#   value it inherited.
#
# Usage: scripts/sandbox_daemon.sh [--bin PATH] [--port N] [--dir DIR] [--dry-run]
#   --bin PATH  the tm binary (default: `tm` on PATH)
#   --port N    loopback port (default 17881; the daemon falls back to an
#               ephemeral port when it is busy and records the real one in
#               <dir>/home/.trusty-mpm/daemon.lock)
#   --dir DIR   an existing sandbox directory (default: a new `mktemp -d`);
#               must not be, or resolve to, the real home
#   --dry-run   validate and print what would run; create and start nothing
# Exit: the daemon's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# The only variables the daemon receives. Keep in step with the exec below.
ALLOWLIST="HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ADDR"

die() {
  echo "sandbox_daemon: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_daemon.sh [--bin PATH] [--port N] [--dir DIR] [--dry-run]" >&2
  exit 2
}

# real_home: the password-database home for this uid; empty when unknown.
real_home() {
  local user
  user="$(id -un)"
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | awk '{print $2}' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# resolve DIR: the physical path of an existing directory; empty when absent.
resolve() {
  (cd "$1" 2>/dev/null && pwd -P) || true
}

BIN=""
PORT="17881"
DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --port) [ "$#" -ge 2 ] || usage; PORT="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "sandbox_daemon: unknown argument: $1" >&2; usage ;;
  esac
done

case "$PORT" in
  ''|*[!0-9]*) echo "sandbox_daemon: --port must be a number" >&2; usage ;;
esac

if [ -z "$BIN" ]; then
  BIN="$(command -v tm || true)"
  [ -n "$BIN" ] || die "no tm binary on PATH; pass --bin"
fi
[ -x "$BIN" ] || die "--bin is not an executable file: $BIN"

REAL_HOME="$(real_home)"
[ -n "$REAL_HOME" ] || die "the password database names no home for this user"
REAL_HOME_RESOLVED="$(resolve "$REAL_HOME")"
[ -n "$REAL_HOME_RESOLVED" ] || REAL_HOME_RESOLVED="$REAL_HOME"

if [ -n "$DIR" ]; then
  SANDBOX="$(resolve "$DIR")"
  [ -n "$SANDBOX" ] || die "--dir is not an existing directory: $DIR"
  [ "$SANDBOX" != "$REAL_HOME_RESOLVED" ] || die "--dir resolves to the real home"
  if [ -d "$SANDBOX/home" ] && [ "$(resolve "$SANDBOX/home")" = "$REAL_HOME_RESOLVED" ]; then
    die "<dir>/home resolves to the real home"
  fi
elif [ "$DRY_RUN" -eq 1 ]; then
  SANDBOX="<new mktemp -d>"
else
  SANDBOX="$(resolve "$(mktemp -d "${TMPDIR:-/tmp}/tm-sandbox.XXXXXX")")"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

ADDR="127.0.0.1:$PORT"
echo "sandbox_daemon: sandbox dir: $SANDBOX"
echo "sandbox_daemon: daemon address: $ADDR"
echo "sandbox_daemon: variables passed (names only): $ALLOWLIST"
echo "sandbox_daemon: command: env -i $ALLOWLIST $BIN daemon --sandbox"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_daemon: dry run; nothing started"
  exit 0
fi

mkdir -p "$SANDBOX/home" "$SANDBOX/data"
exec env -i \
  HOME="$SANDBOX/home" \
  PATH="$PATH" \
  TRUSTY_DATA_DIR_OVERRIDE="$SANDBOX/data" \
  TRUSTY_MPM_ADDR="$ADDR" \
  "$BIN" daemon --sandbox
