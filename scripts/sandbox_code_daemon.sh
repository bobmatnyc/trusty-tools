#!/usr/bin/env bash
#
# sandbox_code_daemon.sh — start an isolated `tcode serve` daemon for a live
# check (issues #6637, #9121).
#
# Why: a hand-built "sandbox" daemon with its own data dir and HOME still
#   inherits every exported credential (#9121), loads the `.env.local` at or
#   above its cwd (#9161), and reaches the per-user stores HOME does not
#   isolate. `scripts/sandbox_daemon.sh` solves this for `tm daemon` and
#   `scripts/sandbox_search_daemon.sh` for `trusty-search`; this script does
#   the same for `tcode serve`, which since #6637 slice 2c serves its Unix
#   socket only.
# What: validates, then runs in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            TRUSTY_MEMORY_SOCKET=<dir>/no-memory-daemon.sock \
#            [<FORWARDED names the caller exported>] \
#            <bin> serve --project <project>
#   No other variable reaches the daemon: no token, no API key, no
#   OPENROUTER*, ANTHROPIC*, AWS*, GITHUB*, TELEGRAM* or SLACK* name.
#   FORWARDED (pass through only when exported): RUST_LOG, TCODE_MOCK_LLM,
#   TCODE_PERMISSION_MODE, LANG, LC_ALL, TMPDIR. TCODE_MOCK_LLM lets a live
#   check run `task.run` with the scripted offline client and no provider key.
#   Data: TRUSTY_DATA_DIR_OVERRIDE puts the socket, logs and log-drain state
#   under <dir>/data; HOME puts `~/.trusty-code` (workstreams, private state)
#   under <dir>/home. TRUSTY_MEMORY_SOCKET names a path nothing serves, so the
#   turn recorder never writes into the live trusty-memory palace, and the
#   trusty-search socket resolves under <dir>/data, where nothing serves.
#   Secure store: `tcode` is built without trusty-common's `keyring-store`
#   feature, so its credential store is the file store under HOME, never the
#   Keychain. The launcher cannot enforce that for an arbitrary `--bin`.
#   Working directory: the daemon runs with cwd <dir>/home. trusty-common loads
#   the first `.env.local` found walking up from the cwd (`load_env_local_once`,
#   crates/trusty-common/src/credentials/dotenv.rs). The script refuses a
#   sandbox dir with a `.env.local` at or above <dir>/home, and TRUSTY_SANDBOX=1
#   (#9178) tells that loader to read none, as a second layer.
#   Project: `--project PATH` binds the daemon to PATH (default <dir>/project,
#   created empty). `tcode serve` writes its agent roster into
#   <project>/.trusty-code/agents/. The project path is the argv token the
#   teardown checks, so it is recorded in <dir>/sandbox.project.
#   Socket (#6637): the daemon binds no TCP port; it serves
#   <dir>/data/trusty-code/trusty-code.sock only, so the script passes no
#   `--port` and takes none. A socket path over 103 bytes (sun_path) refuses.
#   A stale socket file needs no handling: the daemon takes over a dead one
#   and refuses a live one. The script waits up to 60 s
#   (SANDBOX_SOCKET_WAIT_SECS) for the socket and prints its path. Clients
#   reach the sandbox with `TRUSTY_DATA_DIR_OVERRIDE=<dir>/data`.
#   Teardown is kill-by-pid only. The child pid is written to <dir>/sandbox.pid.
#   On EXIT, INT or TERM, and under `--stop DIR`, the script signals that pid
#   after checking that its argv ends `serve --project <project>`. It sends
#   TERM, waits up to 10 s, sends KILL only if the argv check still holds, and
#   waits up to 5 s more. It reports success only when the process is dead. It
#   never uses pkill, killall, a name match or launchctl.
#   Output names variables and the paths this script chose; it prints a value
#   only for the names it pins or forwards.
#
# Usage: scripts/sandbox_code_daemon.sh [--bin PATH] [--dir DIR]
#                                       [--project PATH] [--dry-run]
#        scripts/sandbox_code_daemon.sh --stop DIR
#   --bin PATH      the tcode binary (default: target/debug/tcode under
#                   $CARGO_TARGET_DIR, else `tcode` on PATH)
#   --dir DIR       an existing sandbox directory (default: a new `mktemp -d`);
#                   must not be, or resolve to, the real home
#   --project PATH  an existing project directory (default: <dir>/project)
#   --dry-run       print the exact env and argv, then exit 0; starts nothing
#   --stop DIR      stop the daemon recorded in DIR/sandbox.pid
# Exit: the daemon's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_code_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# Passed through from the caller only when exported — never a credential.
FORWARDED="RUST_LOG TCODE_MOCK_LLM TCODE_PERMISSION_MODE LANG LC_ALL TMPDIR"
SANDBOX_PATH="/usr/bin:/bin"
SOCKET_WAIT_SECS="${SANDBOX_SOCKET_WAIT_SECS:-60}"

die() {
  echo "sandbox_code_daemon: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_code_daemon.sh [--bin PATH] [--dir DIR] [--project PATH] [--dry-run]" >&2
  echo "       scripts/sandbox_code_daemon.sh --stop DIR" >&2
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

# owned_pid PID PROJECT: succeeds only when PID is a plain pid above 1 and its
# argv ends with `serve --project PROJECT`.
owned_pid() {
  local pid="$1" project="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -n "$args" ] || return 1
  case "$args" in *" serve --project $project") return 0 ;; *) return 1 ;; esac
}

# is_alive PID: a running process; a zombie awaiting its parent is dead.
is_alive() {
  local st
  kill -0 "$1" 2>/dev/null || return 1
  st="$(ps -p "$1" -o stat= 2>/dev/null || true)"
  case "$st" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

# wait_dead PID TENTHS: poll for up to TENTHS tenths of a second.
wait_dead() {
  local i=0
  while [ "$i" -lt "$2" ]; do
    is_alive "$1" || return 0
    sleep 0.1
    i=$((i + 1))
  done
  ! is_alive "$1"
}

# terminate_owned PID PROJECT: TERM, wait 10 s, KILL only while owned_pid still
# holds, wait 5 s. Returns 0 only when PID is dead.
terminate_owned() {
  local pid="$1" project="$2"
  is_alive "$pid" || return 0
  if ! owned_pid "$pid" "$project"; then
    echo "sandbox_code_daemon: refused: pid '$pid' is not a process whose argv ends 'serve --project $project'; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned_pid "$pid" "$project"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_dead "$pid" 50 && return 0
  fi
  echo "sandbox_code_daemon: pid $pid is still alive after TERM and KILL; sandbox.pid kept" >&2
  return 1
}

# stop_recorded DIR: terminate the pid in DIR/sandbox.pid.
stop_recorded() {
  local dir="$1" pid project
  [ -f "$dir/sandbox.pid" ] || { echo "sandbox_code_daemon: no $dir/sandbox.pid" >&2; return 1; }
  [ -f "$dir/sandbox.project" ] || { echo "sandbox_code_daemon: no $dir/sandbox.project; nothing signalled" >&2; return 1; }
  pid=""
  project=""
  read -r pid < "$dir/sandbox.pid" || true
  IFS= read -r project < "$dir/sandbox.project" || true
  case "$pid" in ''|*[!0-9]*) echo "sandbox_code_daemon: refused: pidfile holds '$pid', not a pid; nothing signalled" >&2; return 1 ;; esac
  [ "$pid" -gt 1 ] || { echo "sandbox_code_daemon: refused: pid '$pid'; nothing signalled" >&2; return 1; }
  if ! is_alive "$pid"; then
    rm -f "$dir/sandbox.pid"
    echo "sandbox_code_daemon: pid $pid already dead; removed sandbox.pid"
    return 0
  fi
  terminate_owned "$pid" "$project" || return 1
  rm -f "$dir/sandbox.pid"
  echo "sandbox_code_daemon: stopped pid $pid"
}

BIN=""
DIR=""
PROJECT=""
STOP_DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --project) [ "$#" -ge 2 ] || usage; PROJECT="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "sandbox_code_daemon: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  [ -n "$RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$RESOLVED" || exit 1
  exit 0
fi

if [ -z "$BIN" ]; then
  if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "$CARGO_TARGET_DIR/debug/tcode" ]; then
    BIN="$CARGO_TARGET_DIR/debug/tcode"
  else
    BIN="$(command -v tcode || true)"
  fi
  [ -n "$BIN" ] || die "no debug tcode under \$CARGO_TARGET_DIR and none on PATH; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
case "$BIN" in
  */*) BIN_DIR="${BIN%/*}"; [ -n "$BIN_DIR" ] || BIN_DIR=/ ;;
  *) BIN_DIR=. ;;
esac
resolve "$BIN_DIR"
[ -n "$RESOLVED" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$RESOLVED/${BIN##*/}"

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
  # /tmp, not $TMPDIR: the socket path must fit sun_path (macOS $TMPDIR is long).
  MADE="$(mktemp -d /tmp/tc-sandbox.XXXXXX)"
  resolve "$MADE"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# #9161: the daemon loads the first `.env.local` found walking up from its cwd,
# <dir>/home. Walk the physical path, and cover <dir>/home/.env.local itself.
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

if [ -n "$PROJECT" ]; then
  resolve "$PROJECT"
  [ -n "$RESOLVED" ] || die "--project is not an existing directory: $PROJECT"
  PROJECT="$RESOLVED"
else
  PROJECT="$SANDBOX/project"
fi

SOCKET="$SANDBOX/data/trusty-code/trusty-code.sock"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCKET}" -gt 103 ]; then
  die "the socket path $SOCKET is ${#SOCKET} bytes, over sun_path's 103; pick a shorter --dir (e.g. under /tmp)"
fi

# The FORWARDED names the caller exported, as NAME=value words for env -i.
EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1"
  "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data"
  "TRUSTY_MEMORY_SOCKET=$SANDBOX/no-memory-daemon.sock")
for name in $FORWARDED; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    ENV_WORDS+=("$name=${!name}")
  fi
done
ARGV=("$BIN" serve --project "$PROJECT")

echo "sandbox_code_daemon: sandbox dir: $SANDBOX"
echo "sandbox_code_daemon: socket: $SOCKET"
echo "sandbox_code_daemon: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "sandbox_code_daemon: argv: ${ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_code_daemon: dry run; nothing started"
  exit 0
fi

mkdir -p "$SANDBOX/home" "$SANDBOX/data"
[ -d "$PROJECT" ] || mkdir -p "$PROJECT"
if [ -f "$SANDBOX/sandbox.pid" ] && [ -f "$SANDBOX/sandbox.project" ]; then
  prev=""
  prev_project=""
  read -r prev < "$SANDBOX/sandbox.pid" || true
  IFS= read -r prev_project < "$SANDBOX/sandbox.project" || true
  if is_alive "$prev" && owned_pid "$prev" "$prev_project"; then
    die "pid $prev is a running sandbox daemon of this dir; stop it with --stop $SANDBOX first"
  fi
fi
printf '%s\n' "$PROJECT" > "$SANDBOX/sandbox.project"

CHILD=""
# teardown: end only the pid this script spawned, and only while its argv still
# ends `serve --project <project>`. The pidfile goes only once the child is dead.
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  local dead=1
  trap - EXIT INT TERM
  if [ -n "$CHILD" ] && terminate_owned "$CHILD" "$PROJECT"; then
    dead=0
    wait "$CHILD" 2>/dev/null || true
  fi
  if [ "$dead" -eq 0 ]; then rm -f "$SANDBOX/sandbox.pid"; fi
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

cd "$SANDBOX/home"
env -i "${ENV_WORDS[@]}" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "sandbox_code_daemon: started pid $CHILD (stop with: scripts/sandbox_code_daemon.sh --stop $SANDBOX)"

# #6637: the socket is the daemon's only listener; its appearance is readiness.
waited=0
while [ "$waited" -lt $((SOCKET_WAIT_SECS * 5)) ] && [ ! -S "$SOCKET" ] && is_alive "$CHILD"; do
  sleep 0.2
  waited=$((waited + 1))
done
if [ -S "$SOCKET" ]; then
  echo "sandbox_code_daemon: serving socket: $SOCKET"
elif is_alive "$CHILD"; then
  echo "sandbox_code_daemon: no socket at $SOCKET after ${SOCKET_WAIT_SECS}s; the daemon is not serving yet" >&2
else
  echo "sandbox_code_daemon: the daemon exited before binding $SOCKET" >&2
fi
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
