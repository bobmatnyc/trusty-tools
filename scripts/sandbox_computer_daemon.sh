#!/usr/bin/env bash
#
# sandbox_computer_daemon.sh — start an isolated `trusty-computer serve` broker
# for a live check (issues #11460, #9121).
#
# Why: a hand-built broker inherits every exported credential (#9121), loads
#   the `.env.local` at or above its cwd (#9161), and, with the real HOME,
#   resolves `~/.cua-driver` and `~/.trusty/computer` to Bob's live cua-driver
#   config, socket and grants. The trusty-computer plan (section 7) requires
#   every live run to use this script instead.
# What: validates, then runs in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            CUA_DRIVER_RS_TELEMETRY_ENABLED=false CUA_DRIVER_RS_UPDATE_CHECK=false \
#            [<FORWARDED names the caller exported>] \
#            <bin> serve --state <dir>/state
#   No other variable reaches the broker: no token, no CUA_* name besides the
#   two pinned off, no SSH_AUTH_SOCK. FORWARDED: RUST_LOG, LANG, LC_ALL.
#   Live cua-driver isolation: HOME=<dir>/home, so `~/.cua-driver` resolves to
#   <dir>/home/.cua-driver, never the real one. The daemon gets that HOME
#   too: the broker starts it with `/usr/bin/open -n -g -a CuaDriver`, and
#   `open` hands the launched app the caller's environment ("Opened
#   applications inherit environment variables", open(1)); a probe app opened
#   from `env -i HOME=<x>` saw HOME=<x> with and without `--env HOME`
#   (#11460 fix round). The script refuses a --dir
#   that is, or lies inside, the real home's `.cua-driver` or `.trusty`
#   directory, or that resolves to the real home. The broker's private driver
#   socket is <dir>/state/driver/cua-driver.sock (plan section 3); a path over
#   103 bytes (sun_path) refuses. The script never opens, lists or removes any
#   socket itself.
#   Working directory: <dir>/home; a `.env.local` at or above it refuses, and
#   TRUSTY_SANDBOX=1 tells trusty loaders to read none.
#   Teardown is kill-by-pid only: the child pid goes to <dir>/sandbox.pid; on
#   EXIT, INT, TERM and under `--stop DIR` the script signals it only while its
#   argv ends `serve --state <dir>/state`. TERM, 10 s, then KILL, 5 s. A
#   KILLed broker cannot stop its daemon, so the script then runs
#   `cua-driver stop --socket <dir>/state/driver/cua-driver.sock` under the
#   same `env -i` (cua-driver path overridable by SANDBOX_CUA_DRIVER for the
#   selftest). Never pkill, killall, a name match or launchctl.
#   Binary freshness (#10755): a cargo artifact older than the trusty-computer
#   sources refuses (scripts/lib/bin_fresh.sh) unless --allow-stale.
#   Phase 0 note: `serve` is a stub that prints usage and exits 2 until Phase 3.
#
# Usage: scripts/sandbox_computer_daemon.sh [--bin PATH] [--dir DIR] [--allow-stale] [--dry-run]
#        scripts/sandbox_computer_daemon.sh --stop DIR
# Exit: the broker's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_computer_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bin_fresh.sh
. "$SCRIPT_DIR/lib/bin_fresh.sh"

FORWARDED="RUST_LOG LANG LC_ALL"
SANDBOX_PATH="/usr/bin:/bin"
NAME=sandbox_computer_daemon

die() {
  echo "$NAME: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_computer_daemon.sh [--bin PATH] [--dir DIR] [--allow-stale] [--dry-run]" >&2
  echo "       scripts/sandbox_computer_daemon.sh --stop DIR" >&2
  exit 2
}

# real_home: the password-database home for this uid; empty when unknown.
real_home() {
  local user
  user="$(id -un)"
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# resolve DIR: sets $RESOLVED to the physical path of an existing directory.
RESOLVED=""
resolve() {
  local out
  RESOLVED=""
  out="$( (cd "$1" 2>/dev/null && pwd -P && printf x) || true)"
  [ -n "$out" ] || return 0
  out="${out%x}"
  RESOLVED="${out%$'\n'}"
}

# inside PATH ROOT: PATH is ROOT or below it.
inside() {
  [ "$1" = "$2" ] || [ "${1#"$2"/}" != "$1" ]
}

owned_pid() {
  local pid="$1" state="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  case "$args" in *" serve --state $state") return 0 ;; *) return 1 ;; esac
}

is_alive() {
  local st
  kill -0 "$1" 2>/dev/null || return 1
  st="$(ps -p "$1" -o stat= 2>/dev/null || true)"
  case "$st" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

wait_dead() {
  local i=0
  while [ "$i" -lt "$2" ]; do
    is_alive "$1" || return 0
    sleep 0.1
    i=$((i + 1))
  done
  ! is_alive "$1"
}

# stop_driver STATE: ask the daemon on STATE's private socket to stop.
stop_driver() {
  local state="$1" cua="${SANDBOX_CUA_DRIVER:-/Applications/CuaDriver.app/Contents/MacOS/cua-driver}"
  [ -x "$cua" ] || { echo "$NAME: no $cua; the daemon on $state/driver/cua-driver.sock is not stopped" >&2; return 0; }
  env -i "HOME=${state%/state}/home" "PATH=$SANDBOX_PATH" CUA_DRIVER_RS_TELEMETRY_ENABLED=false \
    CUA_DRIVER_RS_UPDATE_CHECK=false "$cua" stop --socket "$state/driver/cua-driver.sock" || true
  echo "$NAME: asked the daemon on $state/driver/cua-driver.sock to stop"
}

terminate_owned() {
  local pid="$1" state="$2"
  is_alive "$pid" || return 0
  if ! owned_pid "$pid" "$state"; then
    echo "$NAME: refused: pid '$pid' argv does not end 'serve --state $state'; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned_pid "$pid" "$state"; then
    kill -KILL "$pid" 2>/dev/null || true
    # #11460: a KILLed broker leaves its daemon running; stop it by socket.
    wait_dead "$pid" 50 && { stop_driver "$state"; return 0; }
  fi
  echo "$NAME: pid $pid is still alive after TERM and KILL; sandbox.pid kept" >&2
  return 1
}

BIN=""
DIR=""
STOP_DIR=""
DRY_RUN=0
ALLOW_STALE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --allow-stale) ALLOW_STALE=1; shift ;;
    -h|--help) usage ;;
    *) echo "$NAME: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  [ -n "$RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  [ -f "$RESOLVED/sandbox.pid" ] || die "no $RESOLVED/sandbox.pid"
  pid=""
  read -r pid < "$RESOLVED/sandbox.pid" || true
  if ! is_alive "$pid"; then
    rm -f "$RESOLVED/sandbox.pid"
    echo "$NAME: pid $pid already dead; removed sandbox.pid"
    exit 0
  fi
  terminate_owned "$pid" "$RESOLVED/state" || exit 1
  rm -f "$RESOLVED/sandbox.pid"
  echo "$NAME: stopped pid $pid"
  exit 0
fi

if [ -z "$BIN" ]; then
  if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "$CARGO_TARGET_DIR/debug/trusty-computer" ]; then
    BIN="$CARGO_TARGET_DIR/debug/trusty-computer"
  else
    BIN="$(command -v trusty-computer || true)"
  fi
  [ -n "$BIN" ] || die "no debug trusty-computer under \$CARGO_TARGET_DIR and none on PATH; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
case "$BIN" in */*) BIN_DIR="${BIN%/*}"; [ -n "$BIN_DIR" ] || BIN_DIR=/ ;; *) BIN_DIR=. ;; esac
resolve "$BIN_DIR"
[ -n "$RESOLVED" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$RESOLVED/${BIN##*/}"
bin_fresh_check "$NAME" "$BIN" "$ALLOW_STALE" "${SANDBOX_SRC_ROOT:-$SCRIPT_DIR/..}" trusty-computer || exit 1

REAL_HOME="$(real_home)"
[ -n "$REAL_HOME" ] || die "the password database names no home for this user"
resolve "$REAL_HOME"
[ -n "$RESOLVED" ] && REAL_HOME="$RESOLVED"

if [ -n "$DIR" ]; then
  resolve "$DIR"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "--dir is not an existing directory: $DIR"
elif [ "$DRY_RUN" -eq 1 ]; then
  SANDBOX="<new mktemp -d>"
else
  # /tmp, not $TMPDIR: the socket path must fit sun_path (macOS $TMPDIR is long).
  SANDBOX="$(mktemp -d /tmp/tcomp-sandbox.XXXXXX)"
  resolve "$SANDBOX"
  SANDBOX="$RESOLVED"
fi

if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
  [ "$SANDBOX" != "$REAL_HOME" ] || die "--dir resolves to the real home"
  for live in "$REAL_HOME/.cua-driver" "$REAL_HOME/.trusty"; do
    ! inside "$SANDBOX" "$live" || die "--dir lies inside the live $live"
  done
  if [ -e "$SANDBOX/home" ]; then
    resolve "$SANDBOX/home"
    [ -n "$RESOLVED" ] || die "<dir>/home exists but is not a directory"
    [ "$RESOLVED" != "$REAL_HOME" ] || die "<dir>/home resolves to the real home"
    anc="$RESOLVED"
  else
    anc="$SANDBOX"
  fi
  # #9161: no `.env.local` at or above the broker's cwd.
  while :; do
    [ ! -f "$anc/.env.local" ] || die "$anc/.env.local would be loaded from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

STATE="$SANDBOX/state"
SOCKET="$STATE/driver/cua-driver.sock"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCKET}" -gt 103 ]; then
  die "the driver socket path $SOCKET is ${#SOCKET} bytes, over sun_path's 103; pick a shorter --dir"
fi

EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1"
  "CUA_DRIVER_RS_TELEMETRY_ENABLED=false" "CUA_DRIVER_RS_UPDATE_CHECK=false")
for name in $FORWARDED; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    ENV_WORDS+=("$name=${!name}")
  fi
done
ARGV=("$BIN" serve --state "$STATE")

echo "$NAME: sandbox dir: $SANDBOX"
echo "$NAME: driver socket: $SOCKET"
echo "$NAME: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "$NAME: argv: ${ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "$NAME: dry run; nothing started"
  exit 0
fi

if [ -f "$SANDBOX/sandbox.pid" ]; then
  prev=""
  read -r prev < "$SANDBOX/sandbox.pid" || true
  if is_alive "$prev" && owned_pid "$prev" "$STATE"; then
    die "pid $prev is a running sandbox broker of this dir; stop it with --stop $SANDBOX first"
  fi
fi
mkdir -p "$SANDBOX/home"
mkdir -p "$STATE"
chmod 0700 "$STATE"

CHILD=""
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  trap - EXIT INT TERM
  if [ -n "$CHILD" ] && terminate_owned "$CHILD" "$STATE"; then
    wait "$CHILD" 2>/dev/null || true
    rm -f "$SANDBOX/sandbox.pid"
  fi
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

cd "$SANDBOX/home"
env -i "${ENV_WORDS[@]}" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "$NAME: started pid $CHILD (stop with: scripts/sandbox_computer_daemon.sh --stop $SANDBOX)"
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
