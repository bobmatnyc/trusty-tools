#!/usr/bin/env bash
#
# sandbox_agents_daemon.sh — start an isolated `tagent --api` for a live check
# or a beta run (issues #10940, #9121).
#
# Why: every `tagent` start deploys the bundled agents into
#   $HOME/.trusty-agents and creates ~/trusty-agents/<instance>. A hand-built
#   "sandbox" with its own HOME still inherits every exported credential
#   (#9121), loads the `.env.local` at or above its cwd (#9161), and loses
#   ~/.aws, so it cannot reach Bedrock. This script is the sanctioned way to
#   run tagent with all of its state under one directory while still reading
#   the AWS profile files in place.
# What: validates, then runs in the foreground:
#     env -i HOME=<dir>/home PATH=/usr/bin:/bin TRUSTY_SANDBOX=1 \
#            TAGENT_NO_PROVISION=0 TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            TRUSTY_MEMORY_SOCKET=<dir>/no-memory-daemon.sock \
#            TAGENT_API_TOKEN=<contents of <dir>/api-token> \
#            [AWS_PROFILE AWS_SHARED_CREDENTIALS_FILE AWS_CONFIG_FILE ...] \
#            [<FORWARDED names the caller exported>] \
#            <bin> --api --port <port>
#   No other variable reaches tagent: no API key, no OPENROUTER*, ANTHROPIC*,
#   GITHUB*, SLACK* or TELEGRAM* name, and never AWS_ACCESS_KEY_ID,
#   AWS_SECRET_ACCESS_KEY or AWS_SESSION_TOKEN.
#   FORWARDED (pass through only when exported): RUST_LOG, TAGENT_LOG,
#   TAGENT_CONFIG_DIR, TAGENT_DEFAULT_MODEL, LANG, LC_ALL, TMPDIR, and every
#   TAGENT_MODEL_* name except one holding TOKEN, KEY, SECRET, PASSWORD,
#   CREDENTIAL or AUTH. TAGENT_CONFIG_DIR is the overlay agent dir a beta
#   config pack lives in.
#   Provisioning (#10940): TRUSTY_SANDBOX=1 alone makes tagent skip its $HOME
#   deploy. HOME here is <dir>/home, so the script sets TAGENT_NO_PROVISION=0
#   and the bundled roster lands in <dir>/home/.trusty-agents, never the real
#   home. `--no-provision` passes TAGENT_NO_PROVISION=1 instead: no roster is
#   deployed and only TAGENT_CONFIG_DIR and project agents exist.
#   Content: the roster is read from the instruction-content cache
#   (~/.trusty-mpm/content), which a sandbox HOME hides. The script copies
#   `--content PATH` (default: the real home's ~/.trusty-mpm/content, when it
#   exists) to <dir>/home/.trusty-mpm/content once, when that is absent. A
#   copy, not a link, so nothing tagent does reaches the real cache.
#   AWS (`--aws-profile NAME`): passes AWS_PROFILE=NAME and the PATHS of the
#   profile files, never their contents. AWS_SHARED_CREDENTIALS_FILE and
#   AWS_CONFIG_FILE are the caller's exported values, else the real home's
#   ~/.aws/credentials and ~/.aws/config when each exists; at least one must
#   exist. AWS_REGION and AWS_DEFAULT_REGION pass when exported. Without the
#   flag no AWS name passes.
#   Data: TRUSTY_DATA_DIR_OVERRIDE puts trusty-common data under <dir>/data.
#   TRUSTY_MEMORY_SOCKET names a path nothing serves, so no write reaches the
#   live trusty-memory palace.
#   Token: <dir>/api-token (mode 0600) holds a random token, made once and
#   reused by a restart. It reaches tagent as TAGENT_API_TOKEN, not argv, and
#   is never printed; clients read the file.
#   Working directory: tagent runs with cwd <dir>/home. The script refuses a
#   sandbox dir with a `.env.local` at or above <dir>/home; TRUSTY_SANDBOX=1
#   (#9178, #9224) tells tagent's loaders to read none, as a second layer.
#   Readiness: polls http://127.0.0.1:<port>/api/health with curl for up to
#   60 s (SANDBOX_HTTP_WAIT_SECS). A port that already answers refuses.
#   Teardown is kill-by-pid only. The child pid is written to <dir>/sandbox.pid
#   and the port to <dir>/sandbox.port. On EXIT, INT or TERM, and under
#   `--stop DIR`, the script signals that pid after checking that its argv ends
#   `--api --port <port>`. It sends TERM, waits up to 10 s, sends KILL only if
#   the argv check still holds, and waits up to 5 s more. It reports success
#   only when the process is dead. It never uses pkill, killall, a name match
#   or launchctl. `--restart` with `--dir` stops the recorded daemon first.
#   Binary freshness (#10755): a cargo build artifact older than the newest
#   trusty-agents source refuses (scripts/lib/bin_fresh.sh); an installed
#   binary gets a notice.
#   Limit: the launcher cannot isolate the OS Keychain; tagent reads
#   credentials from env and files only.
#   Output names variables and the paths this script chose; it prints no token
#   and no credential value.
#
# Usage: scripts/sandbox_agents_daemon.sh [--bin PATH] [--dir DIR] [--port N]
#                                         [--aws-profile NAME] [--content PATH]
#                                         [--no-provision] [--restart]
#                                         [--allow-stale] [--dry-run]
#        scripts/sandbox_agents_daemon.sh --stop DIR
#   --bin PATH          the tagent binary (default: target/debug/tagent under
#                       $CARGO_TARGET_DIR, else `tagent` on PATH)
#   --dir DIR           an existing sandbox directory (default: a new
#                       `mktemp -d`); must not be, or resolve to, the real home
#   --port N            the loopback HTTP port (default 8799)
#   --aws-profile NAME  pass AWS_PROFILE and the AWS profile file paths
#   --content PATH      the content cache to copy into <dir>/home
#   --no-provision      TAGENT_NO_PROVISION=1: deploy no bundled roster
#   --restart           stop the daemon recorded in --dir, then start
#   --allow-stale       run a build artifact older than its sources
#   --dry-run           print the exact env and argv, then exit 0; starts nothing
#   --stop DIR          stop the daemon recorded in DIR/sandbox.pid
# Exit: tagent's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_agents_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bin_fresh.sh
. "$SCRIPT_DIR/lib/bin_fresh.sh"

# Passed through from the caller only when exported — never a credential.
FORWARDED="RUST_LOG TAGENT_LOG TAGENT_CONFIG_DIR TAGENT_DEFAULT_MODEL LANG LC_ALL TMPDIR"
SANDBOX_PATH="/usr/bin:/bin"
HTTP_WAIT_SECS="${SANDBOX_HTTP_WAIT_SECS:-60}"
P="sandbox_agents_daemon"

die() {
  echo "$P: refused: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/sandbox_agents_daemon.sh [--bin PATH] [--dir DIR] [--port N] [--aws-profile NAME]" >&2
  echo "                                        [--content PATH] [--no-provision] [--restart]" >&2
  echo "                                        [--allow-stale] [--dry-run]" >&2
  echo "       scripts/sandbox_agents_daemon.sh --stop DIR" >&2
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

# owned_pid PID PORT: succeeds only when PID is a plain pid above 1 and its
# argv ends with `--api --port PORT`.
owned_pid() {
  local pid="$1" port="$2" args
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  case "$port" in ''|*[!0-9]*) return 1 ;; esac
  [ "$pid" -gt 1 ] || return 1
  args="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -n "$args" ] || return 1
  case "$args" in *" --api --port $port") return 0 ;; *) return 1 ;; esac
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

# terminate_owned PID PORT: TERM, wait 10 s, KILL only while owned_pid still
# holds, wait 5 s. Returns 0 only when PID is dead.
terminate_owned() {
  local pid="$1" port="$2"
  is_alive "$pid" || return 0
  if ! owned_pid "$pid" "$port"; then
    echo "$P: refused: pid '$pid' is not a process whose argv ends '--api --port $port'; nothing signalled" >&2
    return 1
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait_dead "$pid" 100 && return 0
  if owned_pid "$pid" "$port"; then
    kill -KILL "$pid" 2>/dev/null || true
    wait_dead "$pid" 50 && return 0
  fi
  echo "$P: pid $pid is still alive after TERM and KILL; sandbox.pid kept" >&2
  return 1
}

# stop_recorded DIR: terminate the pid in DIR/sandbox.pid.
stop_recorded() {
  local dir="$1" pid port
  [ -f "$dir/sandbox.pid" ] || { echo "$P: no $dir/sandbox.pid" >&2; return 1; }
  [ -f "$dir/sandbox.port" ] || { echo "$P: no $dir/sandbox.port; nothing signalled" >&2; return 1; }
  pid=""
  port=""
  read -r pid < "$dir/sandbox.pid" || true
  read -r port < "$dir/sandbox.port" || true
  case "$pid" in ''|*[!0-9]*) echo "$P: refused: pidfile holds '$pid', not a pid; nothing signalled" >&2; return 1 ;; esac
  [ "$pid" -gt 1 ] || { echo "$P: refused: pid '$pid'; nothing signalled" >&2; return 1; }
  if ! is_alive "$pid"; then
    rm -f "$dir/sandbox.pid"
    echo "$P: pid $pid already dead; removed sandbox.pid"
    return 0
  fi
  terminate_owned "$pid" "$port" || return 1
  rm -f "$dir/sandbox.pid"
  echo "$P: stopped pid $pid"
}

# http_answers PORT: curl gets any HTTP status from /api/health.
http_answers() {
  local code
  code="$(curl -s -o /dev/null -m 2 -w '%{http_code}' "http://127.0.0.1:$1/api/health" 2>/dev/null || true)"
  [ -n "$code" ] && [ "$code" != "000" ]
}

BIN=""
DIR=""
PORT="8799"
AWS_PROFILE_NAME=""
CONTENT=""
STOP_DIR=""
NO_PROVISION=0
RESTART=0
DRY_RUN=0
ALLOW_STALE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --port) [ "$#" -ge 2 ] || usage; PORT="$2"; shift 2 ;;
    --aws-profile) [ "$#" -ge 2 ] || usage; AWS_PROFILE_NAME="$2"; shift 2 ;;
    --content) [ "$#" -ge 2 ] || usage; CONTENT="$2"; shift 2 ;;
    --stop) [ "$#" -ge 2 ] || usage; STOP_DIR="$2"; shift 2 ;;
    --no-provision) NO_PROVISION=1; shift ;;
    --restart) RESTART=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --allow-stale) ALLOW_STALE=1; shift ;;
    -h|--help) usage ;;
    *) echo "$P: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -n "$STOP_DIR" ]; then
  resolve "$STOP_DIR"
  [ -n "$RESOLVED" ] || die "--stop is not an existing directory: $STOP_DIR"
  stop_recorded "$RESOLVED" || exit 1
  exit 0
fi

case "$PORT" in ''|*[!0-9]*) echo "$P: --port is not a number: $PORT" >&2; usage ;; esac
[ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || { echo "$P: --port out of range: $PORT" >&2; usage; }
case "$AWS_PROFILE_NAME" in *[!A-Za-z0-9._-]*) die "--aws-profile holds a character outside [A-Za-z0-9._-]" ;; esac
[ "$RESTART" -eq 0 ] || [ -n "$DIR" ] || die "--restart needs --dir"

if [ -z "$BIN" ]; then
  if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "$CARGO_TARGET_DIR/debug/tagent" ]; then
    BIN="$CARGO_TARGET_DIR/debug/tagent"
  else
    BIN="$(command -v tagent || true)"
  fi
  [ -n "$BIN" ] || die "no debug tagent under \$CARGO_TARGET_DIR and none on PATH; pass --bin"
fi
[ -x "$BIN" ] && [ -f "$BIN" ] || die "--bin is not an executable file: $BIN"
case "$BIN" in
  */*) BIN_DIR="${BIN%/*}"; [ -n "$BIN_DIR" ] || BIN_DIR=/ ;;
  *) BIN_DIR=. ;;
esac
resolve "$BIN_DIR"
[ -n "$RESOLVED" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$RESOLVED/${BIN##*/}"
# #10755: refuse a stale build artifact before anything is created.
bin_fresh_check "$P" "$BIN" "$ALLOW_STALE" "${SANDBOX_SRC_ROOT:-$SCRIPT_DIR/..}" trusty-agents || exit 1

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
  MADE="$(mktemp -d "${TMPDIR:-/tmp}/ta-sandbox.XXXXXX")"
  resolve "$MADE"
  SANDBOX="$RESOLVED"
  [ -n "$SANDBOX" ] || die "could not create a sandbox directory"
fi

# #9161: tagent loads the first `.env.local` found walking up from its cwd,
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
    [ ! -f "$anc/.env.local" ] || die "$anc/.env.local would be loaded by tagent from <dir>/home; pick a --dir outside it"
    [ "$anc" != "/" ] || break
    anc="${anc%/*}"
    [ -n "$anc" ] || anc=/
  done
fi

# Content cache to seed: --content, else the real home's when present.
if [ -n "$CONTENT" ]; then
  resolve "$CONTENT"
  [ -n "$RESOLVED" ] || die "--content is not an existing directory: $CONTENT"
  CONTENT="$RESOLVED"
elif [ -d "$REAL_HOME_RESOLVED/.trusty-mpm/content" ]; then
  CONTENT="$REAL_HOME_RESOLVED/.trusty-mpm/content"
fi

EXPORTED=" $(compgen -e | tr '\n' ' ') "
is_exported() { [ "${EXPORTED#* "$1" }" != "$EXPORTED" ]; }

if [ "$NO_PROVISION" -eq 1 ]; then PROVISION_WORD="TAGENT_NO_PROVISION=1"; else PROVISION_WORD="TAGENT_NO_PROVISION=0"; fi
ENV_WORDS=("HOME=$SANDBOX/home" "PATH=$SANDBOX_PATH" "TRUSTY_SANDBOX=1" "$PROVISION_WORD"
  "TRUSTY_DATA_DIR_OVERRIDE=$SANDBOX/data"
  "TRUSTY_MEMORY_SOCKET=$SANDBOX/no-memory-daemon.sock")

# #10940: AWS by file path only, and only on request.
if [ -n "$AWS_PROFILE_NAME" ]; then
  if is_exported AWS_SHARED_CREDENTIALS_FILE; then AWS_CREDS="$AWS_SHARED_CREDENTIALS_FILE"; else AWS_CREDS="$REAL_HOME_RESOLVED/.aws/credentials"; fi
  if is_exported AWS_CONFIG_FILE; then AWS_CONF="$AWS_CONFIG_FILE"; else AWS_CONF="$REAL_HOME_RESOLVED/.aws/config"; fi
  [ -f "$AWS_CREDS" ] || [ -f "$AWS_CONF" ] || die "--aws-profile: neither $AWS_CREDS nor $AWS_CONF is a file"
  ENV_WORDS+=("AWS_PROFILE=$AWS_PROFILE_NAME")
  [ ! -f "$AWS_CREDS" ] || ENV_WORDS+=("AWS_SHARED_CREDENTIALS_FILE=$AWS_CREDS")
  [ ! -f "$AWS_CONF" ] || ENV_WORDS+=("AWS_CONFIG_FILE=$AWS_CONF")
  for name in AWS_REGION AWS_DEFAULT_REGION; do
    if is_exported "$name"; then ENV_WORDS+=("$name=${!name}"); fi
  done
fi
for name in $FORWARDED; do
  if is_exported "$name"; then ENV_WORDS+=("$name=${!name}"); fi
done
for name in $EXPORTED; do
  case "$name" in TAGENT_MODEL_*) ;; *) continue ;; esac
  case "$name" in *TOKEN*|*KEY*|*SECRET*|*PASSWORD*|*CREDENTIAL*|*AUTH*) continue ;; esac
  ENV_WORDS+=("$name=${!name}")
done
ARGV=("$BIN" --api --port "$PORT")
TOKEN_FILE="$SANDBOX/api-token"

echo "$P: sandbox dir: $SANDBOX"
echo "$P: url: http://127.0.0.1:$PORT (token in $TOKEN_FILE)"
if [ -n "$CONTENT" ]; then
  echo "$P: content cache: $CONTENT (copied to $SANDBOX/home/.trusty-mpm/content when absent)"
else
  echo "$P: no content cache found; pass --content PATH, or tagent deploys no roster"
fi
echo "$P: env -i:"
for w in "${ENV_WORDS[@]}"; do echo "  $w"; done
echo "  TAGENT_API_TOKEN=<contents of $TOKEN_FILE>"
echo "$P: argv: ${ARGV[*]}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "$P: dry run; nothing started"
  exit 0
fi

if [ "$RESTART" -eq 1 ] && [ -f "$SANDBOX/sandbox.pid" ]; then
  stop_recorded "$SANDBOX" || die "--restart could not stop the recorded daemon"
fi
if [ -f "$SANDBOX/sandbox.pid" ] && [ -f "$SANDBOX/sandbox.port" ]; then
  prev=""
  prev_port=""
  read -r prev < "$SANDBOX/sandbox.pid" || true
  read -r prev_port < "$SANDBOX/sandbox.port" || true
  if is_alive "$prev" && owned_pid "$prev" "$prev_port"; then
    die "pid $prev is a running sandbox daemon of this dir; stop it with --stop $SANDBOX or pass --restart"
  fi
fi
! http_answers "$PORT" || die "port $PORT already answers HTTP; pick another --port"

mkdir -p "$SANDBOX/home" "$SANDBOX/data"
if [ -n "$CONTENT" ] && [ ! -e "$SANDBOX/home/.trusty-mpm/content" ]; then
  mkdir -p "$SANDBOX/home/.trusty-mpm"
  cp -R "$CONTENT" "$SANDBOX/home/.trusty-mpm/content"
fi
if [ ! -s "$TOKEN_FILE" ]; then
  # One line with its newline: the child's `read` fails on an unterminated one.
  (umask 077 && { od -An -tx1 -N24 /dev/urandom | tr -d ' \n'; echo; } > "$TOKEN_FILE")
fi
chmod 600 "$TOKEN_FILE"
[ -s "$TOKEN_FILE" ] || die "could not write a token to $TOKEN_FILE"
printf '%s\n' "$PORT" > "$SANDBOX/sandbox.port"

CHILD=""
# teardown: end only the pid this script spawned, and only while its argv still
# ends `--api --port <port>`. The pidfile goes only once the child is dead.
# shellcheck disable=SC2329  # invoked through the traps below
teardown() {
  local dead=1 cur
  trap - EXIT INT TERM
  if [ -n "$CHILD" ] && terminate_owned "$CHILD" "$PORT"; then
    dead=0
    wait "$CHILD" 2>/dev/null || true
  fi
  # A --restart has already recorded its own child; leave that pidfile.
  if [ "$dead" -eq 0 ] && [ -f "$SANDBOX/sandbox.pid" ]; then
    cur=""
    read -r cur < "$SANDBOX/sandbox.pid" || true
    [ "$cur" != "$CHILD" ] || rm -f "$SANDBOX/sandbox.pid"
  fi
}
trap teardown EXIT
trap 'teardown; exit 130' INT
trap 'teardown; exit 143' TERM

cd "$SANDBOX/home"
# The token goes file -> env inside the child, so no argv ever holds it; the
# `exec` leaves tagent's argv exactly ARGV for the ownership check.
# shellcheck disable=SC2016  # expanded by the child shell
env -i "${ENV_WORDS[@]}" /bin/sh -c \
  'IFS= read -r TAGENT_API_TOKEN < "$0" && export TAGENT_API_TOKEN && exec "$@"' \
  "$TOKEN_FILE" "${ARGV[@]}" &
CHILD=$!
echo "$CHILD" > "$SANDBOX/sandbox.pid"
echo "$P: started pid $CHILD (stop with: scripts/sandbox_agents_daemon.sh --stop $SANDBOX)"

waited=0
while [ "$waited" -lt $((HTTP_WAIT_SECS * 5)) ] && ! http_answers "$PORT" && is_alive "$CHILD"; do
  sleep 0.2
  waited=$((waited + 1))
done
if http_answers "$PORT"; then
  echo "$P: serving: http://127.0.0.1:$PORT"
elif is_alive "$CHILD"; then
  echo "$P: no answer on port $PORT after ${HTTP_WAIT_SECS}s; tagent is not serving yet" >&2
else
  echo "$P: tagent exited before serving port $PORT" >&2
fi
STATUS=0
wait "$CHILD" || STATUS=$?
exit "$STATUS"
