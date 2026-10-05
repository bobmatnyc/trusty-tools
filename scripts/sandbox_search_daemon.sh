#!/usr/bin/env bash
#
# sandbox_search_daemon.sh — start an isolated `trusty-search` daemon for a live
# check or an RSS measurement (issues #8275, #9121).
#
# Why: a hand-built "sandbox" daemon with its own port and HOME still inherits
#   every exported credential, and the Keychain belongs to the OS user, not to
#   HOME (#9121). `scripts/sandbox_daemon.sh` solves this for `tm daemon`; this
#   script does the same for `trusty-search`, which has its own flags and data
#   directory variable. The #8275 RSS measurement and the live checks of #8176,
#   #8149, #8659, #8686, #8777 and #8958 need a daemon that cannot reach the
#   live one (port 7878, `com.trusty.search`) or its data.
# What: validates, then runs in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR=<dir>/data \
#            [FASTEMBED_CACHE_DIR=<model cache>] [<KNOBS the caller set>] \
#            <bin> start --foreground --no-auto-discover \
#                  --data-dir <dir>/data --port <N>
#   No other variable reaches the daemon: no token, no API key, no OPENROUTER*,
#   ANTHROPIC*, GITHUB* or SLACK* name. KNOBS (pass through only when the
#   caller exported them): TRUSTY_WARMBOOT_MAX_INDEXES, TRUSTY_MAX_RESIDENT_INDEXES,
#   TRUSTY_REDB_CACHE_MB, TRUSTY_EMBEDDING_CACHE, TRUSTY_EMBED_INFLIGHT, RUST_LOG,
#   TRUSTY_EMBEDDERD_BIN.
#   Working directory: the daemon runs with cwd <dir>/home. At startup it walks
#   up from its cwd for a `.env.local` and loads it (`load_env_local_once`,
#   crates/trusty-common/src/credentials/dotenv.rs). The script refuses a
#   sandbox dir with a `.env.local` in any ancestor, so a cwd inside a checkout
#   cannot reload the very keys `env -i` removed.
#   TRUSTY_SANDBOX=1 (#9178) also tells that loader to read no `.env.local`,
#   project or $HOME tier, as a second layer behind the refusal above.
#   Embedder sidecar: with PATH /usr/bin:/bin the daemon finds `trusty-embedderd`
#   only as a sibling of the `--bin` executable or through TRUSTY_EMBEDDERD_BIN.
#   `--bin` therefore needs a sibling `trusty-embedderd`, or export
#   TRUSTY_EMBEDDERD_BIN before calling.
#   Port: 7814..7878 is refused, because the daemon walks forward up to 64 ports
#   from the one requested (`bind_with_auto_port`) and could land on the live
#   daemon's 7878. The default is the first free port from 17900. The bound port
#   can still differ from the requested one: callers MUST read
#   <dir>/data/daemon.port. The script waits up to 60 s for that file and prints
#   the actual port, or says it timed out.
#   Model cache: a fresh HOME makes the embedder download its ONNX model into
#   <dir>/home/.cache/fastembed. `--model-cache PATH` (or an exported
#   FASTEMBED_CACHE_DIR) forwards that one directory as FASTEMBED_CACHE_DIR;
#   the resolver is `resolve_fastembed_cache_dir` in
#   crates/trusty-common/src/embedder/types.rs. The mount is not read-only: no
#   env var can make it so, and fastembed may write lock files there. The
#   directory must already exist. The script creates nothing in the real home,
#   with one exception: when `--model-cache` (or FASTEMBED_CACHE_DIR) names a
#   directory in the real home, such as ~/.cache/fastembed, fastembed may write
#   lock files and new model files there. The flag is not refused.
#   Teardown is kill-by-pid only. The child pid is written to <dir>/sandbox.pid.
#   On EXIT, INT or TERM, and under `--stop DIR`, the script signals that pid
#   after checking that the process's argv holds the token
#   `--data-dir <dir>/data` followed by a space or the end of argv. It sends
#   TERM, waits up to 10 s, sends KILL only if the argv check still holds, and
#   waits up to 5 s more. It reports success only when the process is dead; on
#   failure it keeps sandbox.pid and says why. It never uses pkill, killall, a
#   name match or launchctl.
#   Limit: the launcher cannot isolate the OS Keychain or secure-store credential
#   tiers; it relies on trusty-search not reading them (see #9121).
#   Output names variables and the paths this script chose; it prints a value
#   only for the names this script pins or the knobs above.
#
# Usage: scripts/sandbox_search_daemon.sh [--bin PATH] [--port N] [--dir DIR]
#                                         [--model-cache PATH] [--dry-run]
#        scripts/sandbox_search_daemon.sh --stop DIR
#   --bin PATH          the trusty-search binary (default: on PATH)
#   --port N            requested loopback port (default: a free port from 17900;
#                       7814..7878 is refused)
#   --dir DIR           an existing sandbox directory (default: a new `mktemp -d`);
#                       must not be, or resolve to, the real home
#   --model-cache PATH  an existing fastembed cache directory to reuse
#   --dry-run           print the exact env and argv, then exit 0; creates and
#                       starts nothing
#   --stop DIR          stop the daemon recorded in DIR/sandbox.pid
# Exit: the daemon's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_search_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# Passed through from the caller only when exported — tuning knobs, never a credential.
KNOBS="TRUSTY_WARMBOOT_MAX_INDEXES TRUSTY_MAX_RESIDENT_INDEXES TRUSTY_REDB_CACHE_MB \
TRUSTY_EMBEDDING_CACHE TRUSTY_EMBED_INFLIGHT RUST_LOG TRUSTY_EMBEDDERD_BIN"
SANDBOX_PATH="/usr/bin:/bin"
LIVE_PORT=7878
# The daemon walks forward up to 64 ports from the requested one.
LIVE_LOW=$((LIVE_PORT - 64))
PORT_WAIT_SECS="${SANDBOX_PORT_WAIT_SECS:-60}"
FIRST_PORT=17900

die() {
  echo "sandbox_search_daemon: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_search_daemon.sh [--bin PATH] [--port N] [--dir DIR]" >&2
  echo "                                        [--model-cache PATH] [--dry-run]" >&2
  echo "       scripts/sandbox_search_daemon.sh --stop DIR" >&2
  exit 2
}

# Path audit (#9121): a `$(...)` strips trailing newlines, so a path feeding a
# security decision never goes through one. Paths are produced by `resolve`
# into $RESOLVED, and the ancestor walk uses parameter expansion. The pid and
# port files hold digits only; `ps` argv and the mktemp name (random suffix)
# cannot end in a newline that matters.
# CDPATH is deliberately not unset: `resolve` runs `cd` on caller-supplied
# values, and a CDPATH hit could only redirect a relative one (below the bar).

# real_home: the password-database home for this uid; empty when unknown. A
# home whose name holds a newline cannot be told apart in dscl/getent's
# line-oriented output; the real-home refusal is defence in depth only.
real_home() {
  local user
  user="$(id -un)"
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# resolve DIR: sets $RESOLVED to the physical path of an existing directory,
# empty when absent. A sentinel keeps a trailing newline in the name; the
# result is never returned through `$(...)`.
RESOLVED=""
resolve() {
  local out
  RESOLVED=""
  out="$( (cd "$1" 2>/dev/null && pwd -P && printf x) || true)"
  [ -n "$out" ] || return 0
  out="${out%x}"
  RESOLVED="${out%$'\n'}"
}

# port_busy N: succeeds when something already listens on 127.0.0.1:N.
port_busy() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

# free_port: the first unused port from FIRST_PORT; empty when none of 100 is.
free_port() {
  local p="$FIRST_PORT"
  while [ "$p" -lt $((FIRST_PORT + 100)) ]; do
    if ! port_busy "$p"; then echo "$p"; return 0; fi
    p=$((p + 1))
  done
}

# owned_pid PID DIR: succeeds only when PID is a plain pid above 1 and its argv
# holds the token `--data-dir DIR/data` followed by a space or the end of argv.
# A negative, zero, group-shaped or reused pid, and a sibling dir such as
# `DIR-other`, fail.
owned_pid() {
  local pid="$1" dir="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -n "$args" ] || return 1
  case "$args " in *"--data-dir $dir/data "*) return 0 ;; *) return 1 ;; esac
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
# holds, wait 5 s. Returns 0 only when PID is dead. Returns 1 and prints why
# when PID is alive but not owned (nothing signalled) or survives KILL.
terminate_owned() {
  local pid="$1" dir="$2"
  is_alive "$pid" || return 0
  if ! owned_pid "$pid" "$dir"; then
    echo "sandbox_search_daemon: refused: pid '$pid' is not a process whose argv holds --data-dir $dir/data; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned_pid "$pid" "$dir"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_dead "$pid" 50 && return 0
  fi
  echo "sandbox_search_daemon: pid $pid is still alive after TERM and KILL; $dir/sandbox.pid kept" >&2
  return 1
}

# stop_recorded DIR: terminate the pid in DIR/sandbox.pid. The pidfile is
# removed, and 0 returned, only once that process is dead.
stop_recorded() {
  local dir="$1" pidfile pid
  pidfile="$dir/sandbox.pid"
  [ -f "$pidfile" ] || { echo "sandbox_search_daemon: no $pidfile" >&2; return 1; }
  pid=""
  read -r pid < "$pidfile" || true
  case "$pid" in ''|*[!0-9]*) echo "sandbox_search_daemon: refused: pidfile holds '$pid', not a pid; nothing signalled" >&2; return 1 ;; esac
  [ "$pid" -gt 1 ] || { echo "sandbox_search_daemon: refused: pid '$pid'; nothing signalled" >&2; return 1; }
  if ! is_alive "$pid"; then
    rm -f "$pidfile"
    echo "sandbox_search_daemon: pid $pid already dead; removed sandbox.pid"
    return 0
  fi
  terminate_owned "$pid" "$dir" || return 1
  rm -f "$pidfile"
  echo "sandbox_search_daemon: stopped pid $pid"
}

BIN=""
PORT=""
DIR=""
MODEL_CACHE=""
STOP_DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --port) [ "$#" -ge 2 ] || usage; PORT="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --model-cache) [ "$#" -ge 2 ] || usage; MODEL_CACHE="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "sandbox_search_daemon: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  STOP_RESOLVED="$RESOLVED"
  [ -n "$STOP_RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$STOP_RESOLVED" || exit 1
  exit 0
fi

if [ -n "$PORT" ]; then
  case "$PORT" in
    *[!0-9]*) echo "sandbox_search_daemon: --port must be a number" >&2; usage ;;
  esac
  if [ "$PORT" -ge "$LIVE_LOW" ] && [ "$PORT" -le "$LIVE_PORT" ]; then
    die "port $PORT is within 64 of the live daemon's port $LIVE_PORT; the daemon walks forward on a busy port"
  fi
  [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "--port is out of range: $PORT"
else
  PORT="$(free_port)"
  [ -n "$PORT" ] || die "no free port from $FIRST_PORT; pass --port"
fi

if [ -z "$BIN" ]; then
  BIN="$(command -v trusty-search || true)"
  [ -n "$BIN" ] || die "no trusty-search binary on PATH; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
# Absolute, so the launch works from the sandbox cwd and a sibling
# trusty-embedderd is found next to it.
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
  MADE="$(mktemp -d "${TMPDIR:-/tmp}/ts-sandbox.XXXXXX")"
  resolve "$MADE"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# #9121: the daemon loads the first `.env.local` found walking up from its cwd.
# Its cwd is <dir>/home, and `current_dir()` is the physical path, so the walk
# starts at the physical <dir>/home (a symlinked home must not escape it) and
# covers <dir>/home/.env.local itself.
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

# The model cache: --model-cache wins, then an exported FASTEMBED_CACHE_DIR.
[ -n "$MODEL_CACHE" ] || MODEL_CACHE="${FASTEMBED_CACHE_DIR:-}"
if [ -n "$MODEL_CACHE" ]; then
  resolve "$MODEL_CACHE"
  MODEL_CACHE_RESOLVED="$RESOLVED"
  [ -n "$MODEL_CACHE_RESOLVED" ] || die "model cache is not an existing directory: $MODEL_CACHE"
  MODEL_CACHE="$MODEL_CACHE_RESOLVED"
fi

# The KNOBS the caller exported, as NAME=value words for env -i. `compgen -e`
# lists exported names only. Values are read by indirect expansion.
EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_DATA_DIR=$SANDBOX/data" "TRUSTY_SANDBOX=1")
if [ -n "$MODEL_CACHE" ]; then
  ENV_WORDS+=("FASTEMBED_CACHE_DIR=$MODEL_CACHE")
fi
for name in $KNOBS; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    ENV_WORDS+=("$name=${!name}")
  fi
done
ARGV=("$BIN" start --foreground --no-auto-discover --data-dir "$SANDBOX/data" --port "$PORT")

echo "sandbox_search_daemon: sandbox dir: $SANDBOX"
echo "sandbox_search_daemon: port: $PORT"
if [ -z "$MODEL_CACHE" ]; then
  echo "sandbox_search_daemon: no model cache given; the embedder downloads into $SANDBOX/home/.cache/fastembed" \
    "(pass --model-cache PATH to reuse one)"
fi
echo "sandbox_search_daemon: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "sandbox_search_daemon: argv: ${ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_search_daemon: dry run; nothing started"
  exit 0
fi

mkdir -p "$SANDBOX/home" "$SANDBOX/data"

CHILD=""
# teardown: end only the pid this script spawned, and only while its argv still
# holds the sandbox's --data-dir. The pidfile goes only once the child is dead.
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

# A stale port file from an earlier run in this dir must not pass for ours.
rm -f "$SANDBOX/data/daemon.port"
# cwd <dir>/home: see the Working directory note in the header.
cd "$SANDBOX/home"
env -i "${ENV_WORDS[@]}" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "sandbox_search_daemon: started pid $CHILD (stop with: scripts/sandbox_search_daemon.sh --stop $SANDBOX)"

# The daemon walks forward from the requested port; the port file is the truth.
PORT_FILE="$SANDBOX/data/daemon.port"
waited=0
while [ "$waited" -lt $((PORT_WAIT_SECS * 5)) ] && [ ! -s "$PORT_FILE" ] && is_alive "$CHILD"; do
  sleep 0.2
  waited=$((waited + 1))
done
if [ -s "$PORT_FILE" ]; then
  echo "sandbox_search_daemon: bound port: $(tr -d ' \n' < "$PORT_FILE") (from $PORT_FILE; requested $PORT)"
elif is_alive "$CHILD"; then
  echo "sandbox_search_daemon: no $PORT_FILE after ${PORT_WAIT_SECS}s; the bound port is unknown (requested $PORT); read that file once it appears" >&2
else
  echo "sandbox_search_daemon: the daemon exited before writing $PORT_FILE" >&2
fi
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
