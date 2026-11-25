#!/usr/bin/env bash
#
# sandbox_assistants_console.sh — start an isolated trusty-console and one
# seeded assistant's ask daemon for a live check of the Trusty Assistants page
# (issue #11749, epic #11746).
#
# Why: the page lists assistant instances, reads their turn logs and asks a
#   running ask daemon. A hand-built pair would inherit every exported
#   credential (#9121), load any dot-env-local file at or above its cwd (#9161), and
#   read the real assistant homes. This script is the one sanctioned way to
#   start that pair. It clones scripts/sandbox_secrets_console.sh (the console
#   half) and scripts/sandbox_assistants_ask.sh (the `ta` half).
# What: validates, seeds <dir>, then starts two children in the background
#   and stays in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            TRUSTY_MEMORY_SOCKET=<dir>/no-memory-daemon.sock \
#            TCODE_MOCK_LLM=echo \
#            <ta-bin> ask-daemon <instance>   < <dir>/daemon.stdin (a FIFO)
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data TRUSTY_MPM_ROOT=<dir>/mpm \
#            TRUSTY_SECRETS_EXTERNAL=1 \
#            <console-bin> serve --http <bind>
#   Both run with cwd <dir>/home. No other variable reaches either child.
#   `TCODE_MOCK_LLM=echo` answers offline with no provider key; its answer is
#   `engineer: echoed hello-from-mock-engineer`. The daemon's stdin is a FIFO
#   this script holds open: that is the lease its idle exit waits for, so it
#   serves until teardown or `--stop-daemon`.
#   Seed: the assistants root is trusty-agents/ under <dir>/home (what `ta`
#   and the console derive from HOME). <instance>/ holds instructions.md and
#   stores/ask-turns.jsonl with two turns. Decoys `ta list` must hide: OKG/
#   (an uppercase name, with an instructions.md), bob-kb/ (no
#   instructions.md) and README.md (a file).
#   Sockets: <dir>/data/trusty-agents/trusty-agents-<instance>.sock and the
#   daemon's health socket beside it. <dir> defaults to a new mktemp -d under
#   /tmp, so the socket path fits sun_path.
#   Refusals: a relative or non-executable binary; a binary older than its
#   sources (scripts/lib/bin_fresh.sh, #10755; `--allow-stale` warns); a bind
#   that is not 127.0.0.1:<port> or [::1]:<port>; an invalid instance id; a
#   --dir that is, or resolves to, the real home (password database, never
#   $HOME); a dot-env-local file in <dir>/home or any physical ancestor
#   (#9161); a
#   <dir> path holding a quote, backslash or control character; a socket path
#   over 103 bytes; a live child already recorded for <dir>.
#   Teardown is kill-by-pid only: <dir>/console.pid and <dir>/daemon.pid. A
#   pid is signalled only while its argv still ends in the token this script
#   gave it (`--http <bind>` or `ask-daemon <instance>`): TERM, 10 s, KILL,
#   5 s. Then the FIFO goes, and so does any socket a killed daemon left. On
#   EXIT, INT, TERM, when the console exits, and under `--stop DIR`.
#   `--stop-daemon DIR` stops only the daemon, so the page shows the 503 hint
#   while the console keeps serving.
#   Output names variables and paths this script chose; it never prints a
#   value it inherited.
#
# Usage: scripts/sandbox_assistants_console.sh --console-bin PATH --ta-bin PATH
#            [--dir DIR] [--bind HOST:PORT] [--instance ID] [--allow-stale]
#            [--dry-run]
#        scripts/sandbox_assistants_console.sh --stop DIR
#        scripts/sandbox_assistants_console.sh --stop-daemon DIR
#   --console-bin PATH  absolute path of the trusty-console binary
#   --ta-bin PATH       absolute path of the ta binary
#   --dir DIR           an existing sandbox directory (default: a new mktemp -d)
#   --bind HOST:PORT    console listener, loopback only (default 127.0.0.1:17891)
#   --instance ID       the seeded instance (default sandbox)
#   --allow-stale       run a build artifact older than its sources, warning
#   --dry-run           validate and print what would run; create nothing
#   SANDBOX_SOCKET_WAIT_SECS (default 20) bounds the wait for the ask socket.
# Exit: 0 after a clean stop; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_assistants_console_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail
# #11821 P6a: --projects-root / --config-dir, shared by every sandbox launcher.
# Builtins only: a selftest runs this launcher on a PATH with no dirname.
case "${BASH_SOURCE[0]}" in */*) SIP_DIR="${BASH_SOURCE[0]%/*}" ;; *) SIP_DIR=. ;; esac
# shellcheck source=scripts/lib/sandbox_install_paths.sh
. "$SIP_DIR/lib/sandbox_install_paths.sh"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bin_fresh.sh
. "$SCRIPT_DIR/lib/bin_fresh.sh"

SANDBOX_PATH="/usr/bin:/bin"
SOCKET_WAIT_SECS="${SANDBOX_SOCKET_WAIT_SECS:-20}"
ME="sandbox_assistants_console"

die() {
  echo "$ME: refused: $*" >&2
  exit 1
}

usage() {
  echo "  install paths (#11821): [--projects-root PATH] [--config-dir PATH], inside --dir" >&2
  echo "usage: scripts/sandbox_assistants_console.sh --console-bin PATH --ta-bin PATH" >&2
  echo "           [--dir DIR] [--bind HOST:PORT] [--instance ID] [--allow-stale] [--dry-run]" >&2
  echo "       scripts/sandbox_assistants_console.sh --stop DIR" >&2
  echo "       scripts/sandbox_assistants_console.sh --stop-daemon DIR" >&2
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

# read_word FILE: the first line of FILE, empty when absent.
read_word() {
  local v=""
  [ -f "$1" ] && read -r v < "$1"
  echo "$v"
}

# stop_daemon DIR: stop the recorded ask daemon, then remove what a killed
# one leaves: its sockets and the stdin FIFO. 0 when dead.
stop_daemon() {
  local dir="$1" pid id
  id="$(read_word "$dir/instance")"
  if [ -f "$dir/daemon.pid" ]; then
    pid="$(read_word "$dir/daemon.pid")"
    stop_owned "$pid" "ask-daemon $id" || return 1
    rm -f "$dir/daemon.pid"
  fi
  [ -z "$id" ] || rm -f "$dir/data/trusty-agents/trusty-agents-$id.sock"
  rm -f "$dir/data/trusty-agents/trusty-agents.sock" "$dir/daemon.stdin"
}

# stop_recorded DIR: stop the console, then the daemon. 0 when both are dead.
stop_recorded() {
  local dir="$1" status=0 pid
  if [ -f "$dir/console.pid" ]; then
    pid="$(read_word "$dir/console.pid")"
    if stop_owned "$pid" "--http $(read_word "$dir/console.addr")"; then
      rm -f "$dir/console.pid"
    else
      status=1
    fi
  fi
  stop_daemon "$dir" || status=1
  return "$status"
}

CONSOLE_BIN=""
TA_BIN=""
DIR=""
BIND="127.0.0.1:17891"
INSTANCE="sandbox"
STOP_DIR=""
STOP_DAEMON_DIR=""
DRY_RUN=0
ALLOW_STALE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --console-bin) [ "$#" -ge 2 ] || usage; CONSOLE_BIN="$2"; shift 2 ;;
    --ta-bin) [ "$#" -ge 2 ] || usage; TA_BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --bind) [ "$#" -ge 2 ] || usage; BIND="$2"; shift 2 ;;
    --instance) [ "$#" -ge 2 ] || usage; INSTANCE="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --stop-daemon) [ "$#" -ge 2 ] || usage; STOP_DAEMON_DIR="$2"; shift 2 ;;
    --allow-stale) ALLOW_STALE=1; shift ;;
    --projects-root) [ "$#" -ge 2 ] || usage; SIP_PROJECTS="$2"; shift 2 ;;
    --config-dir) [ "$#" -ge 2 ] || usage; SIP_CONFIG="$2"; shift 2 ;;
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
if [ -n "$STOP_DAEMON_DIR" ]; then
  resolve "$STOP_DAEMON_DIR"
  [ -n "$RESOLVED" ] || die "--stop-daemon is not an existing directory: $STOP_DAEMON_DIR"
  stop_daemon "$RESOLVED" || exit 1
  echo "$ME: stopped the ask daemon recorded in $RESOLVED; the console keeps serving"
  exit 0
fi

# The `ta` instance-id rule: 1-64 of [a-z0-9._-], a letter or digit, not . or ..
case "$INSTANCE" in
  ''|.|..|*[!a-z0-9._-]*) die "--instance must be [a-z0-9._-] and not . or ..: $INSTANCE" ;;
esac
case "$INSTANCE" in *[a-z0-9]*) ;; *) die "--instance needs a letter or digit: $INSTANCE" ;; esac
[ "${#INSTANCE}" -le 64 ] || die "--instance is over 64 characters"

for bin in "$CONSOLE_BIN" "$TA_BIN"; do
  [ -n "$bin" ] || { echo "$ME: --console-bin and --ta-bin are required" >&2; usage; }
  case "$bin" in /*) ;; *) die "binary path is not absolute: $bin" ;; esac
  [ -f "$bin" ] && [ -x "$bin" ] || die "binary is not an executable file: $bin"
done
# #10755. The bundle is not a source here: a debug console reads it from disk
# at run time, and a byte-identical `pnpm build` only moves its mtime. The
# selftest's `page assets are the committed bundle's` case covers #11749 AC5.
SRC_ROOT="${SANDBOX_SRC_ROOT:-$SCRIPT_DIR/..}"
bin_fresh_check "$ME" "$CONSOLE_BIN" "$ALLOW_STALE" "$SRC_ROOT" trusty-console || exit 1
bin_fresh_check "$ME" "$TA_BIN" "$ALLOW_STALE" "$SRC_ROOT" trusty-assistants || exit 1

# Loopback only: this sandbox is for one machine; the tailnet check uses the
# real console (crates/trusty-console/README.md, "Assistants").
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
  # /tmp, not $TMPDIR: the socket path must fit sun_path.
  resolve "$(mktemp -d /tmp/ta-console.XXXXXX)"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
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
  envf=".env"
  while :; do
    [ ! -f "$anc/${envf}.local" ] || die "$anc/${envf}.local would be loaded from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

SANDBOX_HOME="$SANDBOX/home"
ROOT="$SANDBOX_HOME/trusty-agents"
INSTANCE_HOME="$ROOT/$INSTANCE"
SOCKET="$SANDBOX/data/trusty-agents/trusty-agents-$INSTANCE.sock"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCKET}" -gt 103 ]; then
  die "the socket path $SOCKET is ${#SOCKET} bytes, over sun_path's 103; pick a shorter --dir"
fi
COMMON=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1" "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data")
DAEMON_ENV=("${COMMON[@]}" "TRUSTY_MEMORY_SOCKET=$SANDBOX/no-memory-daemon.sock" "TCODE_MOCK_LLM=echo")
CONSOLE_ENV=("${COMMON[@]}" "TRUSTY_MPM_ROOT=$SANDBOX/mpm" "TRUSTY_SECRETS_EXTERNAL=1")
DAEMON_ARGV=("$TA_BIN" ask-daemon "$INSTANCE")
CONSOLE_ARGV=("$CONSOLE_BIN" serve --http "$BIND")

echo "$ME: sandbox dir: $SANDBOX"
echo "$ME: instance home: $INSTANCE_HOME"
echo "$ME: ask socket: $SOCKET"
echo "$ME: daemon env -i (names): HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MEMORY_SOCKET TCODE_MOCK_LLM"
echo "$ME: console env -i (names): HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ROOT TRUSTY_SECRETS_EXTERNAL"
echo "$ME: daemon argv: ${DAEMON_ARGV[*]}"
echo "$ME: console argv: ${CONSOLE_ARGV[*]}"
echo "$ME: page: http://$BIND/tools/assistants/"

# #11821 P6a: admit only install paths inside the sandbox dir.
sip_check sandbox_assistants_console "$SANDBOX" || exit 1
if [ "$DRY_RUN" -eq 1 ]; then
  echo "$ME: dry run; nothing created or started"
  exit 0
fi
sip_write sandbox_assistants_console "$SANDBOX" || exit 1

for role in console daemon; do
  prev="$(read_word "$SANDBOX/$role.pid")"
  ! is_alive "$prev" || die "pid $prev ($role) is recorded live for this dir; stop it with --stop $SANDBOX"
done

# Seed the instance and the decoys `ta list` hides.
mkdir -p "$INSTANCE_HOME/stores" "$ROOT/OKG" "$ROOT/bob-kb" "$SANDBOX/data" "$SANDBOX/mpm"
[ -f "$INSTANCE_HOME/instructions.md" ] || printf 'You are the sandbox assistant (#11749).\n' > "$INSTANCE_HOME/instructions.md"
if [ ! -f "$INSTANCE_HOME/stores/ask-turns.jsonl" ]; then
  {
    printf '%s\n' '{"v":1,"at":"2026-11-20T10:00:00Z","question":"seeded question one","answer":"seeded answer one"}'
    printf '%s\n' '{"v":1,"at":"2026-11-21T10:00:00Z","question":"seeded question two","answer":"seeded answer two"}'
  } > "$INSTANCE_HOME/stores/ask-turns.jsonl"
fi
printf 'Uppercase: not an instance id, so ta list hides it.\n' > "$ROOT/OKG/instructions.md"
printf 'No instructions.md here, so ta list hides it.\n' > "$ROOT/bob-kb/notes.md"
printf 'A file, not an instance home.\n' > "$ROOT/README.md"
[ -f "$SANDBOX/mpm/project-paths.json" ] || printf '[]\n' > "$SANDBOX/mpm/project-paths.json"
echo "$BIND" > "$SANDBOX/console.addr"
echo "$INSTANCE" > "$SANDBOX/instance"

CONSOLE_PID=""
DAEMON_PID=""
FIFO="$SANDBOX/daemon.stdin"
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  trap - EXIT INT TERM
  # Release the daemon's stdin lease first; TERM below covers the rest.
  exec 9>&-
  stop_recorded "$SANDBOX" || echo "$ME: teardown left a child running; see the pidfiles in $SANDBOX" >&2
  # Reap our own children, so none outlives this script as a zombie.
  local pid
  for pid in $CONSOLE_PID $DAEMON_PID; do wait "$pid" 2>/dev/null || true; done
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

cd "$SANDBOX/home"
rm -f "$FIFO" "$SOCKET"
mkfifo -m 600 "$FIFO"
# The child blocks opening the FIFO until fd 9 below opens its write end.
env -i "${DAEMON_ENV[@]}" "${DAEMON_ARGV[@]}" < "$FIFO" > "$SANDBOX/daemon.out" 2> "$SANDBOX/daemon.log" &
DAEMON_PID=$!
echo "$DAEMON_PID" > "$SANDBOX/daemon.pid"
exec 9> "$FIFO"

waited=0
while [ "$waited" -lt $((SOCKET_WAIT_SECS * 5)) ] && [ ! -S "$SOCKET" ] && is_alive "$DAEMON_PID"; do
  sleep 0.2
  waited=$((waited + 1))
done
is_alive "$DAEMON_PID" || die "the ask daemon exited before serving $SOCKET; see $SANDBOX/daemon.log"
[ -S "$SOCKET" ] || die "no ask socket at $SOCKET after ${SOCKET_WAIT_SECS}s; see $SANDBOX/daemon.log"

env -i "${CONSOLE_ENV[@]}" "${CONSOLE_ARGV[@]}" > "$SANDBOX/console.log" 2>&1 &
CONSOLE_PID=$!
echo "$CONSOLE_PID" > "$SANDBOX/console.pid"
echo "$ME: started ask daemon pid $DAEMON_PID and trusty-console pid $CONSOLE_PID"
echo "$ME: open http://$BIND/tools/assistants/"
echo "$ME: stop the daemon only (the page then shows the 503 hint): scripts/sandbox_assistants_console.sh --stop-daemon $SANDBOX"
echo "$ME: stop everything: scripts/sandbox_assistants_console.sh --stop $SANDBOX (or Ctrl-C here)"

# bash 3.2 has no `wait -n`: poll until the console exits, then tear down.
# The daemon may stop first (--stop-daemon); the console keeps serving.
while is_alive "$CONSOLE_PID"; do
  sleep 1
done
echo "$ME: the console exited; stopping the daemon" >&2
exit 0
