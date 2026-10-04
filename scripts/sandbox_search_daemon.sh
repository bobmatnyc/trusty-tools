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
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_DATA_DIR=<dir>/data \
#            [FASTEMBED_CACHE_DIR=<model cache>] [<KNOBS the caller set>] \
#            <bin> start --foreground --no-auto-discover \
#                  --data-dir <dir>/data --port <N>
#   No other variable reaches the daemon: no token, no API key, no OPENROUTER*,
#   ANTHROPIC*, GITHUB* or SLACK* name. KNOBS (pass through only when the
#   caller exported them): TRUSTY_WARMBOOT_MAX_INDEXES, TRUSTY_MAX_RESIDENT_INDEXES,
#   TRUSTY_REDB_CACHE_MB, TRUSTY_EMBEDDING_CACHE, TRUSTY_EMBED_INFLIGHT, RUST_LOG.
#   Port 7878 is refused. The default port is the first free one from 17900.
#   Model cache: a fresh HOME makes the embedder download its ONNX model into
#   <dir>/home/.cache/fastembed. `--model-cache PATH` (or an exported
#   FASTEMBED_CACHE_DIR) forwards that one directory as FASTEMBED_CACHE_DIR;
#   the resolver is `resolve_fastembed_cache_dir` in
#   crates/trusty-common/src/embedder/types.rs. The mount is not read-only: no
#   env var can make it so, and fastembed may write lock files there. The
#   directory must already exist, so the script never creates anything in the
#   real home.
#   Teardown is kill-by-pid only. The child pid is written to <dir>/sandbox.pid.
#   On EXIT, INT or TERM, and under `--stop DIR`, the script signals that pid
#   after checking that the process's argv contains <dir>. It never uses pkill,
#   killall, a name match or launchctl.
#   Output names variables and the paths this script chose; it prints a value
#   only for the names this script pins or the knobs above.
#
# Usage: scripts/sandbox_search_daemon.sh [--bin PATH] [--port N] [--dir DIR]
#                                         [--model-cache PATH] [--dry-run]
#        scripts/sandbox_search_daemon.sh --stop DIR
#   --bin PATH          the trusty-search binary (default: on PATH)
#   --port N            loopback port (default: a free port from 17900; not 7878)
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
TRUSTY_EMBEDDING_CACHE TRUSTY_EMBED_INFLIGHT RUST_LOG"
SANDBOX_PATH="/usr/bin:/bin"
LIVE_PORT=7878
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

# port_busy N: succeeds when something already listens on 127.0.0.1:N.
port_busy() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

# free_port: the first unused port from FIRST_PORT; empty when none of 100 is.
free_port() {
  local p="$FIRST_PORT"
  while [ "$p" -lt $((FIRST_PORT + 100)) ]; do
    if [ "$p" -ne "$LIVE_PORT" ] && ! port_busy "$p"; then echo "$p"; return 0; fi
    p=$((p + 1))
  done
}

# owned_pid PID DIR: succeeds only when PID is a plain pid above 1 and its argv
# contains DIR. A negative, zero, group-shaped or reused pid fails.
owned_pid() {
  local pid="$1" dir="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -n "$args" ] || return 1
  case "$args" in *"$dir"*) return 0 ;; *) return 1 ;; esac
}

# stop_recorded DIR: signal TERM to the pid in DIR/sandbox.pid when it still
# owns DIR; returns 1 and signals nothing otherwise. Re-checks before KILL.
stop_recorded() {
  local dir="$1" pidfile pid i
  pidfile="$dir/sandbox.pid"
  [ -f "$pidfile" ] || { echo "sandbox_search_daemon: no $pidfile" >&2; return 1; }
  pid="$(tr -d ' \n' < "$pidfile")"
  if ! owned_pid "$pid" "$dir"; then
    echo "sandbox_search_daemon: refused: pid '$pid' is not a process whose argv contains $dir; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  i=0
  while [ "$i" -lt 50 ] && kill -0 "$pid" 2>/dev/null; do sleep 0.2; i=$((i + 1)); done
  if kill -0 "$pid" 2>/dev/null && owned_pid "$pid" "$dir"; then
    kill -KILL "$pid" 2>/dev/null || true
  fi
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
  STOP_RESOLVED="$(resolve "$STOP_DIR")"
  [ -n "$STOP_RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$STOP_RESOLVED" || exit 1
  exit 0
fi

if [ -n "$PORT" ]; then
  case "$PORT" in
    *[!0-9]*) echo "sandbox_search_daemon: --port must be a number" >&2; usage ;;
  esac
  [ "$PORT" -ne "$LIVE_PORT" ] || die "port $LIVE_PORT is the live daemon's port"
  [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "--port is out of range: $PORT"
else
  PORT="$(free_port)"
  [ -n "$PORT" ] || die "no free port from $FIRST_PORT; pass --port"
fi

if [ -z "$BIN" ]; then
  BIN="$(command -v trusty-search || true)"
  [ -n "$BIN" ] || die "no trusty-search binary on PATH; pass --bin"
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
  SANDBOX="$(resolve "$(mktemp -d "${TMPDIR:-/tmp}/ts-sandbox.XXXXXX")")"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# The model cache: --model-cache wins, then an exported FASTEMBED_CACHE_DIR.
[ -n "$MODEL_CACHE" ] || MODEL_CACHE="${FASTEMBED_CACHE_DIR:-}"
if [ -n "$MODEL_CACHE" ]; then
  MODEL_CACHE_RESOLVED="$(resolve "$MODEL_CACHE")"
  [ -n "$MODEL_CACHE_RESOLVED" ] || die "model cache is not an existing directory: $MODEL_CACHE"
  MODEL_CACHE="$MODEL_CACHE_RESOLVED"
fi

# The KNOBS the caller exported, as NAME=value words for env -i. `compgen -e`
# lists exported names only. Values are read by indirect expansion.
EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_DATA_DIR=$SANDBOX/data")
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
# teardown: signal only the pid this script spawned, and only while its argv
# still contains the sandbox dir.
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  trap - EXIT INT TERM
  if [ -n "$CHILD" ] && owned_pid "$CHILD" "$SANDBOX"; then
    kill -TERM "$CHILD" 2>/dev/null || true
    wait "$CHILD" 2>/dev/null || true
  fi
  rm -f "$SANDBOX/sandbox.pid"
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

env -i "${ENV_WORDS[@]}" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "sandbox_search_daemon: started pid $CHILD (stop with: scripts/sandbox_search_daemon.sh --stop $SANDBOX)"
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
