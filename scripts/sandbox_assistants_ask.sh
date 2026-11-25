#!/usr/bin/env bash
#
# sandbox_assistants_ask.sh — run one `ta ask` against an isolated assistant
# instance and prove its on-demand daemon exits idle (#11650 slice 3d).
#
# Why: `ta ask` starts a per-instance daemon on demand, and that daemon
#   inherits the client's environment. The other sandbox launchers (#9121)
#   start one long-lived foreground daemon by argv, which does not fit a
#   client-spawned, self-exiting daemon. A hand-run `ta ask` would inherit
#   every exported credential, load the `.env.local` at or above its cwd
#   (#9161), and read the real assistant homes.
# What: validates, then runs:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            TRUSTY_MEMORY_SOCKET=<dir>/no-memory-daemon.sock \
#            [<FORWARDED names the caller exported>] \
#            <bin> ask <instance> <question>
#   with cwd <dir>/home. No other variable reaches `ta` or its daemon.
#   FORWARDED (only when exported): RUST_LOG, TCODE_MOCK_LLM, LANG, LC_ALL,
#   TMPDIR. `TCODE_MOCK_LLM=echo` answers offline with no provider key.
#   Instance: the home is <dir>/home/trusty-agents/<instance>/. `--role
#   personal|cto` writes its `instructions.md` from the crate's role template
#   (crates/trusty-assistants/src/assistants/templates/<role>.md) when absent;
#   without `--role` the home must already hold one. The sandbox HOME holds no
#   MCP server config, so the ask registry has `okg_search` and no MCP tools.
#   Idle proof: after the ask returns, the script waits up to
#   SANDBOX_IDLE_WAIT_SECS (default 30) until the instance socket
#   <dir>/data/trusty-agents/trusty-agents-<instance>.sock and the health
#   socket are gone, and every `serving ... as pid N` line in the instance log
#   has its `pid N exited` line and pid N is dead. It prints those lines.
#   Teardown on timeout is kill-by-pid only: TERM to a logged pid whose argv
#   ends `ask-daemon <instance>`, never pkill, a name match or launchctl.
#   The sandbox directory is kept; its path is printed.
#   Binary freshness (#10755): a cargo build artifact older than the
#   trusty-assistants sources refuses (scripts/lib/bin_fresh.sh).
#
# Usage: scripts/sandbox_assistants_ask.sh [--bin PATH] [--dir DIR]
#            [--role personal|cto] [--allow-stale] [--dry-run]
#            INSTANCE QUESTION
#   --bin PATH     the ta binary (default: $CARGO_TARGET_DIR/debug/ta, else
#                  `ta` on PATH)
#   --dir DIR      an existing sandbox directory (default: a new mktemp -d
#                  under /tmp); must not be, or resolve to, the real home
#   --role ROLE    seed the instance's instructions from the ROLE template
#   --allow-stale  run a build artifact older than its sources, with a warning
#   --dry-run      print the exact env and argv, then exit 0; runs nothing
# Exit: the ask's status; 1 on a refusal or when the daemon did not exit idle;
#   2 on a usage error.
#
# Test: scripts/sandbox_assistants_ask_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh.
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

FORWARDED="RUST_LOG TCODE_MOCK_LLM LANG LC_ALL TMPDIR"
SANDBOX_PATH="/usr/bin:/bin"
IDLE_WAIT_SECS="${SANDBOX_IDLE_WAIT_SECS:-30}"
TEMPLATES="$SCRIPT_DIR/../crates/trusty-assistants/src/assistants/templates"

die() {
  echo "sandbox_assistants_ask: refused: $*" >&2
  exit 1
}

usage() {
  echo "  install paths (#11821): [--projects-root PATH] [--config-dir PATH], inside --dir" >&2
  echo "usage: scripts/sandbox_assistants_ask.sh [--bin PATH] [--dir DIR] [--role personal|cto] [--allow-stale] [--dry-run] INSTANCE QUESTION" >&2
  exit 2
}

real_home() {
  local user
  user="$(id -un)"
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

RESOLVED=""
resolve() {
  local out
  RESOLVED=""
  out="$( (cd "$1" 2>/dev/null && pwd -P && printf x) || true)"
  [ -n "$out" ] || return 0
  out="${out%x}"
  RESOLVED="${out%$'\n'}"
}

is_alive() {
  local st
  kill -0 "$1" 2>/dev/null || return 1
  st="$(ps -p "$1" -o stat= 2>/dev/null || true)"
  case "$st" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

# owned_daemon PID: PID's argv ends `ask-daemon <instance>`.
owned_daemon() {
  local args
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  [ "$1" -gt 1 ] || return 1
  args="$(ps -p "$1" -o command= 2>/dev/null || true)"
  case "$args" in *" ask-daemon $INSTANCE") return 0 ;; *) return 1 ;; esac
}

# The pids the instance log says served, one per line.
served_pids() {
  [ -f "$LOG" ] || return 0
  sed -n 's/.*: serving .* as pid \([0-9][0-9]*\)$/\1/p' "$LOG"
}

# idle: no socket, every served pid logged its exit and is dead.
idle() {
  local pid
  [ ! -e "$SOCKET" ] && [ ! -e "$HEALTH_SOCKET" ] || return 1
  for pid in $(served_pids); do
    grep -q ": pid $pid exited (" "$LOG" || return 1
    ! is_alive "$pid" || return 1
  done
  return 0
}

BIN=""
DIR=""
ROLE=""
DRY_RUN=0
ALLOW_STALE=0
POSITIONAL=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --role) [ "$#" -ge 2 ] || usage; ROLE="$2"; shift 2 ;;
    --projects-root) [ "$#" -ge 2 ] || usage; SIP_PROJECTS="$2"; shift 2 ;;
    --config-dir) [ "$#" -ge 2 ] || usage; SIP_CONFIG="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --allow-stale) ALLOW_STALE=1; shift ;;
    -h|--help) usage ;;
    -*) echo "sandbox_assistants_ask: unknown argument: $1" >&2; usage ;;
    *) POSITIONAL+=("$1"); shift ;;
  esac
done
[ "${#POSITIONAL[@]}" -eq 2 ] || usage
INSTANCE="${POSITIONAL[0]}"
QUESTION="${POSITIONAL[1]}"
case "$INSTANCE" in
  ''|.|..|*[!a-z0-9._-]*) die "instance id must be [a-z0-9._-] and not . or ..: $INSTANCE" ;;
esac
case "$ROLE" in
  ''|personal|cto) ;;
  *) die "--role must be personal or cto: $ROLE" ;;
esac

if [ -z "$BIN" ]; then
  if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "$CARGO_TARGET_DIR/debug/ta" ]; then
    BIN="$CARGO_TARGET_DIR/debug/ta"
  else
    BIN="$(command -v ta || true)"
  fi
  [ -n "$BIN" ] || die "no debug ta under \$CARGO_TARGET_DIR and none on PATH; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
case "$BIN" in
  */*) BIN_DIR="${BIN%/*}"; [ -n "$BIN_DIR" ] || BIN_DIR=/ ;;
  *) BIN_DIR=. ;;
esac
resolve "$BIN_DIR"
[ -n "$RESOLVED" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$RESOLVED/${BIN##*/}"
bin_fresh_check sandbox_assistants_ask "$BIN" "$ALLOW_STALE" "${SANDBOX_SRC_ROOT:-$SCRIPT_DIR/..}" trusty-assistants || exit 1

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
  resolve "$(mktemp -d /tmp/ta-sandbox.XXXXXX)"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# #9161: `ta` loads the first `.env.local` walking up from its cwd <dir>/home.
if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
  anc="$SANDBOX"
  if [ -d "$SANDBOX/home" ]; then resolve "$SANDBOX/home"; anc="$RESOLVED"; fi
  while :; do
    [ ! -f "$anc/.env.local" ] || die "$anc/.env.local would be loaded from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

INSTANCE_HOME="$SANDBOX/home/trusty-agents/$INSTANCE"
DATA="$SANDBOX/data/trusty-agents"
SOCKET="$DATA/trusty-agents-$INSTANCE.sock"
HEALTH_SOCKET="$DATA/trusty-agents.sock"
LOG="$DATA/trusty-agents-$INSTANCE.log"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCKET}" -gt 103 ]; then
  die "the socket path $SOCKET is ${#SOCKET} bytes, over sun_path's 103; pick a shorter --dir"
fi

EXPORTED=" $(compgen -e | tr '\n' ' ') "
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1"
  "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data"
  "TRUSTY_MEMORY_SOCKET=$SANDBOX/no-memory-daemon.sock")
for name in $FORWARDED; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    ENV_WORDS+=("$name=${!name}")
  fi
done
ARGV=("$BIN" ask "$INSTANCE" "$QUESTION")

echo "sandbox_assistants_ask: sandbox dir: $SANDBOX"
echo "sandbox_assistants_ask: instance home: $INSTANCE_HOME"
echo "sandbox_assistants_ask: socket: $SOCKET"
echo "sandbox_assistants_ask: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "sandbox_assistants_ask: argv: ${ARGV[*]}"

# #11821 P6a: admit only install paths inside the sandbox dir.
sip_check sandbox_assistants_ask "$SANDBOX" || exit 1
if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_assistants_ask: dry run; nothing started"
  exit 0
fi
sip_write sandbox_assistants_ask "$SANDBOX" || exit 1

mkdir -p "$INSTANCE_HOME" "$SANDBOX/data"
if [ -n "$ROLE" ] && [ ! -f "$INSTANCE_HOME/instructions.md" ]; then
  [ -f "$TEMPLATES/$ROLE.md" ] || die "no role template at $TEMPLATES/$ROLE.md"
  cp "$TEMPLATES/$ROLE.md" "$INSTANCE_HOME/instructions.md"
  echo "sandbox_assistants_ask: seeded instructions.md from the $ROLE template"
fi
[ -f "$INSTANCE_HOME/instructions.md" ] || die "no $INSTANCE_HOME/instructions.md; pass --role"

STATUS=0
(cd "$SANDBOX/home" && env -i "${ENV_WORDS[@]}" "${ARGV[@]}") || STATUS=$?
echo "sandbox_assistants_ask: ask exit: $STATUS"

waited=0
while [ "$waited" -lt $((IDLE_WAIT_SECS * 10)) ] && ! idle; do
  sleep 0.1
  waited=$((waited + 1))
done
if [ -f "$LOG" ]; then
  echo "sandbox_assistants_ask: daemon lifecycle:"
  grep -E ': serving | exited \(' "$LOG" | sed 's/^/  /' || true
fi
if idle; then
  echo "sandbox_assistants_ask: idle: no daemon serves $INSTANCE and its sockets are gone"
  exit "$STATUS"
fi

echo "sandbox_assistants_ask: the daemon did not exit idle within ${IDLE_WAIT_SECS}s" >&2
for pid in $(served_pids); do
  if is_alive "$pid" && owned_daemon "$pid"; then
    kill -TERM "$pid" 2>/dev/null || true
    echo "sandbox_assistants_ask: sent TERM to pid $pid" >&2
  fi
done
exit 1
