#!/usr/bin/env bash
#
# sandbox_memory_daemon.sh — start an isolated `trusty-memory` daemon for a live
# check (issues #9121, #9161).
#
# Why: a hand-built "sandbox" daemon with its own HOME still inherits every
#   exported credential, and the Keychain belongs to the OS user, not to HOME
#   (#9121). `scripts/sandbox_daemon.sh` covers `tm daemon` and
#   `scripts/sandbox_search_daemon.sh` covers `trusty-search`; this script does
#   the same for `trusty-memory`, whose only isolation lever is the data-dir
#   override. Without it the daemon resolves the real per-user palace store and
#   the live daemon's socket.
# What: validates, then runs in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin:/usr/sbin:/sbin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            [FASTEMBED_CACHE_DIR=<model cache>] [<KNOBS the caller set>] \
#            <dir>/bin/trusty-memory serve --foreground
#   No other variable reaches the daemon: no token, no API key, no OPENROUTER*,
#   ANTHROPIC*, GITHUB* or SLACK* name. KNOBS (pass through only when the
#   caller exported them): RUST_LOG.
#   Data dir and socket: trusty-common's `resolve_data_dir("trusty-memory")`
#   (crates/trusty-common/src/data_dir.rs) returns
#   `$TRUSTY_DATA_DIR_OVERRIDE/trusty-memory`, and the daemon binds
#   `<that>/trusty-memory.sock` (`daemon_socket_path`,
#   crates/trusty-common/src/daemon_addr.rs). The daemon has no TCP port since
#   #6286, so the socket IS its address. The override also makes the daemon skip
#   the startup pin-scan of the real home and the launchd bind guard
#   (`is_data_dir_override_active`, `is_production_socket`). The script refuses
#   a sandbox whose data dir resolves to the real one, and a socket path longer
#   than 100 bytes (sun_path holds 104 on macOS); the default sandbox is
#   therefore made under /tmp, not the long macOS $TMPDIR.
#   Working directory (#9161): the daemon runs with cwd <dir>/home. trusty-common
#   loads the first `.env.local` found walking up from the cwd
#   (`load_env_local_once`, crates/trusty-common/src/credentials/dotenv.rs). The
#   script refuses when the physical <dir>/home or any ancestor holds a
#   `.env.local`, so a cwd inside a checkout cannot reload the keys `env -i`
#   removed. TRUSTY_SANDBOX=1 (#9178) also tells that loader to read no
#   `.env.local`, as a second layer behind the refusal.
#   Ownership: `<dir>/bin/trusty-memory` is a symlink to `--bin`, so the
#   daemon's argv[0] names the sandbox. Teardown is kill-by-pid only. The child
#   pid is written to <dir>/sandbox.pid. On EXIT, INT or TERM, and under
#   `--stop DIR`, the script signals that pid after checking that its argv
#   holds the token `<dir>/bin/trusty-memory serve --foreground`. It sends TERM,
#   waits up to 10 s, sends KILL only if the argv check still holds, and waits
#   up to 5 s more. It reports success only when the process is dead. It never
#   uses pkill, killall, a name match, launchctl or `trusty-memory stop`.
#   Readiness: the script waits up to 60 s (SANDBOX_SOCKET_WAIT_SECS) for the
#   socket and prints its path, or says it timed out.
#   Model cache: `--model-cache PATH` (or an exported FASTEMBED_CACHE_DIR)
#   forwards one existing directory as FASTEMBED_CACHE_DIR; fastembed may write
#   lock files there. Without it any model download lands in <dir>/home.
#   Limit: the launcher cannot isolate the OS Keychain or secure-store credential
#   tiers; it relies on trusty-memory not reading them (see #9121).
#   Output names variables and the paths this script chose; it prints a value
#   only for the names this script pins or the knobs above.
#
# Usage: scripts/sandbox_memory_daemon.sh [--bin PATH] [--dir DIR]
#                                         [--model-cache PATH] [--dry-run]
#        scripts/sandbox_memory_daemon.sh --stop DIR
#   --bin PATH          the trusty-memory binary (default: the first on PATH
#                       outside a `*/shims/*` dir)
#   --dir DIR           an existing sandbox directory (default: a new
#                       `mktemp -d /tmp/tmem-sandbox.XXXXXX`); must not be, or
#                       resolve to, the real home, nor sit under a `.env.local`
#   --model-cache PATH  an existing fastembed cache directory to reuse
#   --dry-run           print the exact env and argv, then exit 0; creates and
#                       starts nothing
#   --stop DIR          stop the daemon recorded in DIR/sandbox.pid
# Exit: the daemon's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_memory_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# Passed through from the caller only when exported — never a credential.
KNOBS="RUST_LOG"
# The sbin dirs hold `sysctl`; without it the daemon misreads RAM and picks
# the Degraded tier. Neither holds a credential.
SANDBOX_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
SOCKET_WAIT_SECS="${SANDBOX_SOCKET_WAIT_SECS:-60}"
# sun_path is 104 bytes on macOS, 108 on Linux; keep a margin.
SOCKET_MAX=100
APP=trusty-memory

die() {
  echo "sandbox_memory_daemon: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_memory_daemon.sh [--bin PATH] [--dir DIR]" >&2
  echo "                                        [--model-cache PATH] [--dry-run]" >&2
  echo "       scripts/sandbox_memory_daemon.sh --stop DIR" >&2
  exit 2
}

# Path audit (#9121): a `$(...)` strips trailing newlines, so a path feeding a
# security decision never goes through one; `resolve` sets $RESOLVED instead.

# real_home: the password-database home for this uid; empty when unknown.
real_home() {
  local user
  user="$(id -un)"
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
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

# owned_pid PID DIR: succeeds only when PID is a plain pid above 1 and its argv
# holds the token `DIR/bin/trusty-memory serve --foreground` at its start or
# after a space (an interpreter stub shows `/bin/sh <path> ...`). A reused pid,
# a sibling dir such as `DIR-other`, and the live daemon all fail.
owned_pid() {
  local pid="$1" dir="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -n "$args" ] || return 1
  case " $args " in *" $dir/bin/$APP serve --foreground "*) return 0 ;; *) return 1 ;; esac
}

# is_alive PID: a running process; a zombie awaiting its parent is dead.
is_alive() {
  local st
  kill -0 "$1" 2>/dev/null || return 1
  st="$(ps -p "$1" -o stat= 2>/dev/null || true)"
  case "$st" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

# wait_dead PID TENTHS: poll for up to TENTHS tenths of a second; succeeds when dead.
wait_dead() {
  local i=0
  while [ "$i" -lt "$2" ]; do
    is_alive "$1" || return 0
    sleep 0.1
    i=$((i + 1))
  done
  ! is_alive "$1"
}

# terminate_owned PID DIR: TERM, wait 10 s, KILL only while owned_pid still
# holds, wait 5 s. Returns 0 only when PID is dead.
terminate_owned() {
  local pid="$1" dir="$2"
  is_alive "$pid" || return 0
  if ! owned_pid "$pid" "$dir"; then
    echo "sandbox_memory_daemon: refused: pid '$pid' is not a process whose argv holds $dir/bin/$APP serve --foreground; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned_pid "$pid" "$dir"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_dead "$pid" 50 && return 0
  fi
  echo "sandbox_memory_daemon: pid $pid is still alive after TERM and KILL; $dir/sandbox.pid kept" >&2
  return 1
}

# stop_recorded DIR: terminate the pid in DIR/sandbox.pid. The pidfile is
# removed, and 0 returned, only once that process is dead.
stop_recorded() {
  local dir="$1" pidfile pid
  pidfile="$dir/sandbox.pid"
  [ -f "$pidfile" ] || { echo "sandbox_memory_daemon: no $pidfile" >&2; return 1; }
  pid=""
  read -r pid < "$pidfile" || true
  case "$pid" in ''|*[!0-9]*) echo "sandbox_memory_daemon: refused: pidfile holds '$pid', not a pid; nothing signalled" >&2; return 1 ;; esac
  [ "$pid" -gt 1 ] || { echo "sandbox_memory_daemon: refused: pid '$pid'; nothing signalled" >&2; return 1; }
  if ! is_alive "$pid"; then
    rm -f "$pidfile"
    echo "sandbox_memory_daemon: pid $pid already dead; removed sandbox.pid"
    return 0
  fi
  terminate_owned "$pid" "$dir" || return 1
  rm -f "$pidfile"
  echo "sandbox_memory_daemon: stopped pid $pid"
}

BIN=""
DIR=""
MODEL_CACHE=""
STOP_DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --model-cache) [ "$#" -ge 2 ] || usage; MODEL_CACHE="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "sandbox_memory_daemon: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  STOP_RESOLVED="$RESOLVED"
  [ -n "$STOP_RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$STOP_RESOLVED" || exit 1
  exit 0
fi

if [ -z "$BIN" ]; then
  # The first match on PATH that is not a version-manager shim: a shim needs
  # its manager's env, which env -i removes.
  while IFS= read -r cand; do
    case "$cand" in */shims/*) continue ;; esac
    BIN="$cand"
    break
  done < <(type -ap "$APP" || true)
  [ -n "$BIN" ] || die "no $APP binary on PATH outside a shims dir; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
# Absolute, so the symlink below works from any cwd.
case "$BIN" in
  */*) BIN_DIR="${BIN%/*}"; [ -n "$BIN_DIR" ] || BIN_DIR=/ ;;
  *) BIN_DIR=. ;;
esac
resolve "$BIN_DIR"
BIN_DIR="$RESOLVED"
[ -n "$BIN_DIR" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$BIN_DIR/${BIN##*/}"

REAL_HOME="$(real_home)"
[ -n "$REAL_HOME" ] || die "the password database names no home for this user"
resolve "$REAL_HOME"
REAL_HOME_RESOLVED="$RESOLVED"
[ -n "$REAL_HOME_RESOLVED" ] || REAL_HOME_RESOLVED="$REAL_HOME"
# The data dir the live daemon resolves with no override (`dirs::data_dir()`).
case "$(uname -s)" in
  Darwin) REAL_DATA="$REAL_HOME_RESOLVED/Library/Application Support/$APP" ;;
  *) REAL_DATA="$REAL_HOME_RESOLVED/.local/share/$APP" ;;
esac

if [ -n "$DIR" ]; then
  resolve "$DIR"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "--dir is not an existing directory: $DIR"
  [ "$SANDBOX" != "$REAL_HOME_RESOLVED" ] || die "--dir resolves to the real home"
  if [ -d "$SANDBOX/home" ]; then
    resolve "$SANDBOX/home"
    [ "$RESOLVED" != "$REAL_HOME_RESOLVED" ] || die "<dir>/home resolves to the real home"
  fi
  if [ -d "$SANDBOX/data/$APP" ]; then
    resolve "$SANDBOX/data/$APP"
    [ "$RESOLVED" != "$REAL_DATA" ] || die "<dir>/data/$APP resolves to the real data dir $REAL_DATA"
  fi
elif [ "$DRY_RUN" -eq 1 ]; then
  SANDBOX="<new mktemp -d>"
else
  # /tmp, not $TMPDIR: the macOS $TMPDIR pushes the socket past sun_path.
  MADE="$(mktemp -d /tmp/tmem-sandbox.XXXXXX)"
  resolve "$MADE"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# #9161: the daemon loads the first `.env.local` found walking up from its cwd.
# Its cwd is the physical <dir>/home, so the walk starts there (a symlinked home
# must not escape it), covers <dir>/home/.env.local itself, and climbs to /.
if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
  if [ -e "$SANDBOX/home" ] || [ -L "$SANDBOX/home" ]; then
    resolve "$SANDBOX/home"
    anc="$RESOLVED"
    [ -n "$anc" ] || die "<dir>/home exists but cannot be resolved to a directory: $SANDBOX/home"
  else
    anc="$SANDBOX"
  fi
  while :; do
    [ ! -f "$anc/.env.local" ] || die "$anc/.env.local would be loaded by the daemon from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

SOCKET="$SANDBOX/data/$APP/$APP.sock"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCKET}" -gt "$SOCKET_MAX" ]; then
  die "socket path is ${#SOCKET} bytes, over $SOCKET_MAX: $SOCKET; pick a shorter --dir"
fi

[ -n "$MODEL_CACHE" ] || MODEL_CACHE="${FASTEMBED_CACHE_DIR:-}"
if [ -n "$MODEL_CACHE" ]; then
  resolve "$MODEL_CACHE"
  [ -n "$RESOLVED" ] || die "model cache is not an existing directory: $MODEL_CACHE"
  MODEL_CACHE="$RESOLVED"
fi

# The KNOBS the caller exported, as NAME=value words for env -i. `compgen -e`
# lists exported names only. Values are read by indirect expansion.
EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data" "TRUSTY_SANDBOX=1")
if [ -n "$MODEL_CACHE" ]; then
  ENV_WORDS+=("FASTEMBED_CACHE_DIR=$MODEL_CACHE")
fi
for name in $KNOBS; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    ENV_WORDS+=("$name=${!name}")
  fi
done
LINK="$SANDBOX/bin/$APP"
ARGV=("$LINK" serve --foreground)

echo "sandbox_memory_daemon: sandbox dir: $SANDBOX"
echo "sandbox_memory_daemon: data dir: $SANDBOX/data/$APP"
echo "sandbox_memory_daemon: socket: $SOCKET"
echo "sandbox_memory_daemon: binary: $BIN (via $LINK)"
echo "sandbox_memory_daemon: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "sandbox_memory_daemon: argv: ${ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_memory_daemon: dry run; nothing started"
  exit 0
fi

mkdir -p "$SANDBOX/home" "$SANDBOX/data" "$SANDBOX/bin"
ln -sfn "$BIN" "$LINK"

CHILD=""
# teardown: end only the pid this script spawned, and only while its argv still
# names the sandbox. The pidfile goes only once the child is dead.
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  local dead=1
  trap - EXIT INT TERM
  if [ -n "$CHILD" ] && terminate_owned "$CHILD" "$SANDBOX"; then
    dead=0
    wait "$CHILD" 2>/dev/null || true
  fi
  if [ "$dead" -eq 0 ]; then rm -f "$SANDBOX/sandbox.pid"; fi
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

# cwd <dir>/home: see the Working directory note in the header.
cd "$SANDBOX/home"
env -i "${ENV_WORDS[@]}" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "sandbox_memory_daemon: started pid $CHILD (stop with: scripts/sandbox_memory_daemon.sh --stop $SANDBOX)"

waited=0
while [ "$waited" -lt $((SOCKET_WAIT_SECS * 5)) ] && [ ! -S "$SOCKET" ] && is_alive "$CHILD"; do
  sleep 0.2
  waited=$((waited + 1))
done
if [ -S "$SOCKET" ]; then
  echo "sandbox_memory_daemon: listening on $SOCKET"
elif is_alive "$CHILD"; then
  echo "sandbox_memory_daemon: no socket at $SOCKET after ${SOCKET_WAIT_SECS}s; the daemon is still starting" >&2
else
  echo "sandbox_memory_daemon: the daemon exited before binding $SOCKET" >&2
fi
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
