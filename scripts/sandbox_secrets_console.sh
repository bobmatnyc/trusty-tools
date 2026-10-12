#!/usr/bin/env bash
#
# sandbox_secrets_console.sh — start an isolated trusty-secrets server and
# trusty-console for a live check of the Secrets page (issue #9793).
#
# Why: the live checks of #9067, #9712 and #9066 need a secrets server that
#   writes `file` values into a throwaway HOME and never touches the login
#   Keychain, plus a console that bridges to it. A hand-built pair inherits
#   every exported credential (#9121), may load a `.env.local` (#9161), and,
#   before #9793, sent every `set` to the Keychain or refused it. This script
#   is the one sanctioned way to start that pair; `scripts/sandbox_daemon.sh`
#   is the `tm daemon` equivalent and is unchanged.
# What: validates, prepares <dir>, then starts two children in the background
#   and stays in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            <secrets-bin> serve --idle-timeout-secs <N> --socket <socket>
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data TRUSTY_MPM_ROOT=<dir>/mpm \
#            TRUSTY_SECRETS_EXTERNAL=1 \
#            <console-bin> serve --http <bind>
#   <socket> is <dir>/home/.trusty-tools/trusty-secrets/secrets.sock, where
#   the console dials. No other variable reaches either child. Under these
#   variables trusty-secrets reads `file` consent from
#   <dir>/home/.trusty-tools/trusty-common/config.yaml, which this script
#   writes as `secrets: default_backend: file`, and opens no Keychain or CLI
#   backend (DOC-74 §6.1, #9793). TRUSTY_SECRETS_EXTERNAL=1 stops the console
#   spawning a second server through its own binary lookup.
#   Projects: <dir>/projects/sandbox-<i> (i = 1..N), each `git init` with
#   origin git@github.com:trusty-sandbox/sandbox-<i>.git so the server derives
#   a project and an owner scope. <dir>/mpm/project-paths.json lists the ones
#   that are existing directories, as tm's registry does; the console reads it
#   through TRUSTY_MPM_ROOT. No tm daemon runs.
#   Refusals: a relative or non-executable binary; a bind that is not
#   127.0.0.1:<port> or [::1]:<port>; a --dir that is, or resolves to, the
#   real home (password database, never $HOME); a `.env.local` in <dir>/home
#   or any physical ancestor (#9161); a <dir> path holding a quote, a
#   backslash or a control character (it is written into JSON); a live child
#   already recorded for <dir>.
#   Teardown is kill-by-pid only: <dir>/secrets.pid and <dir>/console.pid.
#   A pid is signalled only while its argv still ends in the token this
#   script gave it (`--socket <socket>` or `--http <bind>`): TERM, 10 s, KILL,
#   5 s. On EXIT, INT, TERM, when either child exits, and under `--stop DIR`.
#   Output names variables and paths this script chose; it never prints a
#   value it inherited.
#
# Usage: scripts/sandbox_secrets_console.sh --secrets-bin PATH --console-bin PATH
#            [--dir DIR] [--bind HOST:PORT] [--projects N] [--idle-secs N] [--dry-run]
#        scripts/sandbox_secrets_console.sh --stop DIR
#   --secrets-bin PATH  absolute path of the trusty-secrets binary
#   --console-bin PATH  absolute path of the trusty-console binary
#   --dir DIR           an existing sandbox directory (default: a new mktemp -d)
#   --bind HOST:PORT    console listener, loopback only (default 127.0.0.1:17890)
#   --projects N        sandbox projects to seed, 1-9 (default 2)
#   --idle-secs N       the server's idle exit, in seconds (default 14400)
#   --dry-run           validate and print what would run; create nothing
#   --stop DIR          stop the children recorded in DIR
#   SANDBOX_SOCKET_WAIT_SECS (default 10) bounds the wait for the socket.
# Exit: 0 after a clean stop; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_secrets_console_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SANDBOX_PATH="/usr/bin:/bin"
SOCKET_SUBPATH=".trusty-tools/trusty-secrets/secrets.sock"
CONSENT_SUBPATH=".trusty-tools/trusty-common/config.yaml"
SOCKET_WAIT_SECS="${SANDBOX_SOCKET_WAIT_SECS:-10}"
ME="sandbox_secrets_console"

die() {
  echo "$ME: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_secrets_console.sh --secrets-bin PATH --console-bin PATH" >&2
  echo "           [--dir DIR] [--bind HOST:PORT] [--projects N] [--idle-secs N] [--dry-run]" >&2
  echo "       scripts/sandbox_secrets_console.sh --stop DIR" >&2
  exit 2
}

# real_home: the password-database home for this uid; empty when unknown.
real_home() {
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$(id -un)" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# resolve DIR: sets $RESOLVED to the physical path of an existing directory,
# empty when absent. A sentinel keeps a trailing newline in the name.
RESOLVED=""
resolve() {
  local out
  RESOLVED=""
  out="$( (cd "$1" 2>/dev/null && pwd -P && printf x) || true)"
  [ -n "$out" ] || return 0
  out="${out%x}"
  RESOLVED="${out%$'\n'}"
}

# is_alive PID: a running process; a zombie awaiting its parent is dead.
is_alive() {
  local st
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  kill -0 "$1" 2>/dev/null || return 1
  st="$(ps -p "$1" -o stat= 2>/dev/null || true)"
  case "$st" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

# owned PID TOKEN: PID is a plain pid above 1 whose argv ends in TOKEN.
owned() {
  local pid="$1" token="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -ww -p "$pid" -o command= 2>/dev/null || true)"
  case "$args" in *" $token") return 0 ;; *) return 1 ;; esac
}

# wait_dead PID TENTHS: poll up to TENTHS tenths of a second; 0 when dead.
wait_dead() {
  local i=0
  while [ "$i" -lt "$2" ]; do
    is_alive "$1" || return 0
    sleep 0.1
    i=$((i + 1))
  done
  ! is_alive "$1"
}

# stop_owned PID TOKEN: TERM, 10 s, KILL while still owned, 5 s. 0 when dead.
stop_owned() {
  local pid="$1" token="$2"
  is_alive "$pid" || return 0
  if ! owned "$pid" "$token"; then
    echo "$ME: refused: pid '$pid' does not end its argv in '$token'; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned "$pid" "$token"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_dead "$pid" 50 && return 0
  fi
  echo "$ME: pid $pid is still alive after TERM and KILL" >&2
  return 1
}

# The token each child's argv ends in, for DIR.
secrets_token() { echo "--socket $1/home/$SOCKET_SUBPATH"; }
console_token() {
  local addr=""
  [ -f "$1/console.addr" ] && read -r addr < "$1/console.addr"
  echo "--http $addr"
}

# stop_recorded DIR: stop both recorded children; a pidfile goes only once
# its process is dead. 0 when both are dead.
stop_recorded() {
  local dir="$1" status=0 role pid token
  for role in console secrets; do
    [ -f "$dir/$role.pid" ] || continue
    pid=""
    read -r pid < "$dir/$role.pid" || true
    if [ "$role" = console ]; then token="$(console_token "$dir")"; else token="$(secrets_token "$dir")"; fi
    if stop_owned "$pid" "$token"; then
      rm -f "$dir/$role.pid"
    else
      status=1
    fi
  done
  return "$status"
}

SECRETS_BIN=""
CONSOLE_BIN=""
DIR=""
BIND="127.0.0.1:17890"
PROJECTS=2
IDLE=14400
STOP_DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --secrets-bin) [ "$#" -ge 2 ] || usage; SECRETS_BIN="$2"; shift 2 ;;
    --console-bin) [ "$#" -ge 2 ] || usage; CONSOLE_BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --bind) [ "$#" -ge 2 ] || usage; BIND="$2"; shift 2 ;;
    --projects) [ "$#" -ge 2 ] || usage; PROJECTS="$2"; shift 2 ;;
    --idle-secs) [ "$#" -ge 2 ] || usage; IDLE="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "$ME: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  [ -n "$RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$RESOLVED" || exit 1
  echo "$ME: stopped the children recorded in $RESOLVED"
  exit 0
fi

case "$PROJECTS" in [1-9]) ;; *) echo "$ME: --projects must be 1-9" >&2; usage ;; esac
case "$IDLE" in ''|*[!0-9]*|0) echo "$ME: --idle-secs must be a positive number" >&2; usage ;; esac

for bin in "$SECRETS_BIN" "$CONSOLE_BIN"; do
  [ -n "$bin" ] || { echo "$ME: --secrets-bin and --console-bin are required" >&2; usage; }
  case "$bin" in /*) ;; *) die "binary path is not absolute: $bin" ;; esac
  [ -f "$bin" ] && [ -x "$bin" ] || die "binary is not an executable file: $bin"
done

# Loopback only: the page carries secret names and confirmations.
case "$BIND" in
  127.0.0.1:*) PORT="${BIND#127.0.0.1:}" ;;
  '[::1]':*) PORT="${BIND#\[::1\]:}" ;;
  *) die "--bind must be 127.0.0.1:<port> or [::1]:<port>, not $BIND" ;;
esac
case "$PORT" in ''|*[!0-9]*) die "--bind port is not a number: $BIND" ;; esac
[ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "--bind port out of range: $BIND"

REAL_HOME="$(real_home)"
[ -n "$REAL_HOME" ] || die "the password database names no home for this user"
resolve "$REAL_HOME"
REAL_HOME_RESOLVED="${RESOLVED:-$REAL_HOME}"

if [ -n "$DIR" ]; then
  resolve "$DIR"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "--dir is not an existing directory: $DIR"
  [ "$SANDBOX" != "$REAL_HOME_RESOLVED" ] || die "--dir resolves to the real home"
  if [ -d "$SANDBOX/home" ]; then
    resolve "$SANDBOX/home"
    [ "$RESOLVED" != "$REAL_HOME_RESOLVED" ] || die "<dir>/home resolves to the real home"
  fi
elif [ "$DRY_RUN" -eq 1 ]; then
  SANDBOX="<new mktemp -d>"
else
  MADE="$(mktemp -d "${TMPDIR:-/tmp}/secrets-sandbox.XXXXXX")"
  resolve "$MADE"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
  # The registry is JSON written by hand; refuse what would need escaping.
  case "$SANDBOX" in
    *'"'*|*\\*|*[[:cntrl:]]*) die "the sandbox path holds a quote, backslash or control character" ;;
  esac
  # #9161: both children run with cwd <dir>/home; walk its physical path up to /.
  if [ -e "$SANDBOX/home" ] || [ -L "$SANDBOX/home" ]; then
    resolve "$SANDBOX/home"
    anc="$RESOLVED"
    [ -n "$anc" ] || die "<dir>/home exists but cannot be resolved: $SANDBOX/home"
  else
    anc="$SANDBOX"
  fi
  while :; do
    [ ! -f "$anc/.env.local" ] || die "$anc/.env.local would be loaded from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

SOCKET="$SANDBOX/home/$SOCKET_SUBPATH"
COMMON=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1" "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data")
SECRETS_ENV=("${COMMON[@]}")
CONSOLE_ENV=("${COMMON[@]}" "TRUSTY_MPM_ROOT=$SANDBOX/mpm" "TRUSTY_SECRETS_EXTERNAL=1")
SECRETS_ARGV=("$SECRETS_BIN" serve --idle-timeout-secs "$IDLE" --socket "$SOCKET")
CONSOLE_ARGV=("$CONSOLE_BIN" serve --http "$BIND")

echo "$ME: sandbox dir: $SANDBOX"
echo "$ME: secrets socket: $SOCKET"
echo "$ME: console: http://$BIND/"
echo "$ME: secrets env -i (names): HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE"
echo "$ME: console env -i (names): HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ROOT TRUSTY_SECRETS_EXTERNAL"
echo "$ME: secrets argv: ${SECRETS_ARGV[*]}"
echo "$ME: console argv: ${CONSOLE_ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "$ME: dry run; nothing created or started"
  exit 0
fi

for role in secrets console; do
  if [ -f "$SANDBOX/$role.pid" ]; then
    prev=""
    read -r prev < "$SANDBOX/$role.pid" || true
    is_alive "$prev" && die "pid $prev ($role) is recorded live for this dir; stop it with --stop $SANDBOX"
  fi
done

mkdir -p "$SANDBOX/home/${CONSENT_SUBPATH%/*}" "$SANDBOX/data" "$SANDBOX/mpm" "$SANDBOX/projects"
printf 'secrets:\n  default_backend: file\n' > "$SANDBOX/home/$CONSENT_SUBPATH"

# Seed the projects; git runs with the sandbox's own empty environment.
ROWS=""
i=1
while [ "$i" -le "$PROJECTS" ]; do
  name="sandbox-$i"
  path="$SANDBOX/projects/$name"
  i=$((i + 1))
  if ! mkdir -p "$path" 2>/dev/null || [ ! -d "$path" ]; then
    echo "$ME: $path is not a directory; left out of the registry" >&2
    continue
  fi
  if [ ! -d "$path/.git" ]; then
    env -i HOME="$SANDBOX/home" PATH="$SANDBOX_PATH" git -c init.defaultBranch=main -C "$path" init -q
    env -i HOME="$SANDBOX/home" PATH="$SANDBOX_PATH" \
      git -C "$path" remote add origin "git@github.com:trusty-sandbox/$name.git"
  fi
  ROWS="$ROWS${ROWS:+,}
  {\"alias\": \"$name\", \"path\": \"$path\"}"
done
printf '[%s\n]\n' "$ROWS" > "$SANDBOX/mpm/project-paths.json"
echo "$BIND" > "$SANDBOX/console.addr"

SECRETS_PID=""
CONSOLE_PID=""
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  trap - EXIT INT TERM
  stop_recorded "$SANDBOX" || echo "$ME: teardown left a child running; see the pidfiles in $SANDBOX" >&2
  # Reap our own children, so none outlives this script as a zombie.
  local pid
  for pid in $SECRETS_PID $CONSOLE_PID; do wait "$pid" 2>/dev/null || true; done
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

cd "$SANDBOX/home"
env -i "${SECRETS_ENV[@]}" "${SECRETS_ARGV[@]}" &
SECRETS_PID=$!
echo "$SECRETS_PID" > "$SANDBOX/secrets.pid"
waited=0
while [ "$waited" -lt $((SOCKET_WAIT_SECS * 5)) ] && [ ! -S "$SOCKET" ] && is_alive "$SECRETS_PID"; do
  sleep 0.2
  waited=$((waited + 1))
done
is_alive "$SECRETS_PID" || die "trusty-secrets exited before serving $SOCKET"
[ -S "$SOCKET" ] || echo "$ME: no socket at $SOCKET after ${SOCKET_WAIT_SECS}s; starting the console anyway" >&2

env -i "${CONSOLE_ENV[@]}" "${CONSOLE_ARGV[@]}" &
CONSOLE_PID=$!
echo "$CONSOLE_PID" > "$SANDBOX/console.pid"
echo "$ME: started trusty-secrets pid $SECRETS_PID and trusty-console pid $CONSOLE_PID"
echo "$ME: TRUSTY_SANDBOX=1 is set for both; the Keychain and CLI backends stay closed"
echo "$ME: stop with: scripts/sandbox_secrets_console.sh --stop $SANDBOX (or Ctrl-C here)"

# bash 3.2 has no `wait -n`: poll until either child exits, then tear down.
while is_alive "$SECRETS_PID" && is_alive "$CONSOLE_PID"; do
  sleep 1
done
echo "$ME: a child exited; stopping the other" >&2
exit 0
