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
# What: validates, then runs, in the foreground:
#     env -i HOME=<dir>/home PATH=<caller's PATH> \
#            TRUSTY_DATA_DIR_OVERRIDE=<dir>/data \
#            TRUSTY_SANDBOX=1 TMUX_TMPDIR=<dir>/data/tmux \
#            [<FORWARDED names the caller has set>] \
#            <tm> daemon --sandbox
#   The five pinned names are always set; FORWARDED names (locale, terminal,
#   account name, temp dir, log filter) pass through only when the caller set
#   them. No other variable reaches the daemon. Both lists are a subset of the
#   daemon's own closed allowlist (`ENV_ALLOWLIST` and `LOCALE_CATEGORIES` in
#   crates/trusty-mpm/src/bin/tm/commands/daemon_sandbox.rs); keep them in step.
#   The daemon refuses on its own too: any variable outside that allowlist, a
#   missing data-dir override, or HOME equal to the password-database home.
#   The real home is read from the password database (dscl on macOS, getent
#   elsewhere), never from $HOME.
#   Working directory (#9161): the daemon runs with cwd <dir>/home. trusty-common
#   loads the first `.env.local` found walking up from the cwd
#   (`load_env_local_once`, crates/trusty-common/src/credentials/dotenv.rs), and
#   from a worktree that walk reaches the main checkout's `.env.local` (#2474).
#   The script therefore never runs the daemon from the caller's cwd, and
#   refuses when the physical <dir>/home or any ancestor holds a `.env.local`.
#   It does not rely on the daemon's sandbox latch to ignore that file.
#   TRUSTY_SANDBOX=1 (#9178) also tells that loader to read no `.env.local`,
#   project or $HOME tier, as a second layer behind the refusal above.
#   tmux (#9557): the daemon gets a tmux server of its own. TMUX_TMPDIR is
#   <dir>/data/tmux (mode 0700), so tmux's socket is
#   <dir>/data/tmux/tmux-<uid>/default, never the default /tmp/tmux-<uid>/default.
#   The daemon admits tmux only on a TMUX_TMPDIR inside its data dir
#   (crates/trusty-mpm/src/core/tmux_access.rs). The socket path must fit
#   sun_path, so a --dir whose socket path exceeds 103 bytes refuses; pick a
#   shorter --dir (e.g. under /tmp). Teardown: when the daemon exits, the script
#   runs `tmux -S <that socket> kill-server`, which stops only the sandbox's
#   server. SIGINT, SIGTERM and SIGHUP are passed to the daemon first.
#   Socket (#6288): every tm client reaches the daemon over its Unix socket,
#   <dir>/data/trusty-mpm/trusty-mpm.sock; the script prints that path and the
#   TRUSTY_MPM_SOCKET pin a client uses to reach this sandbox. A --dir whose
#   socket path exceeds 103 bytes refuses, as for tmux. #6288 S3a: the daemon
#   binds no TCP port, so no TRUSTY_MPM_ADDR is passed and --port is ignored.
#   Output names variables and the paths this script chose; it never prints a
#   value it inherited.
#
# Usage: scripts/sandbox_daemon.sh [--bin PATH] [--port N] [--dir DIR] [--dry-run]
#   --bin PATH  the tm binary (default: `tm` on PATH)
#   --port N    ignored, with a notice (#6288 S3a: the daemon binds no TCP
#               port); accepted so an older invocation still starts
#   --dir DIR   an existing sandbox directory (default: a new `mktemp -d`);
#               must not be, or resolve to, the real home, nor sit under a
#               `.env.local`
#   --dry-run   validate and print what would run; create and start nothing
# Exit: the daemon's status; 1 on a refusal; 2 on a usage error.
#
# Test: scripts/sandbox_daemon_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# Always set by this script. Keep in step with the exec below.
# #9178: TRUSTY_SANDBOX=1 stops trusty-common loading any `.env.local`.
# #9557: TMUX_TMPDIR gives the daemon its own tmux server.
# #6288 S3a: TRUSTY_MPM_ADDR left with the daemon's TCP bind.
PINNED="HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX TMUX_TMPDIR"
# Passed through from the caller only when set — never a credential.
FORWARDED="LANG LC_ALL LC_COLLATE LC_CTYPE LC_MESSAGES LC_MONETARY LC_NUMERIC LC_TIME \
LC_ADDRESS LC_IDENTIFICATION LC_MEASUREMENT LC_NAME LC_PAPER LC_TELEPHONE \
LOGNAME RUST_LOG SHELL TERM TMPDIR USER"

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
DIR=""
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) [ "$#" -ge 2 ] || usage; BIN="$2"; shift 2 ;;
    # #6288 S3a: accepted and ignored; the daemon binds no TCP port.
    --port)
      [ "$#" -ge 2 ] || usage
      echo "sandbox_daemon: --port is ignored: the daemon binds no TCP port (#6288)" >&2
      shift 2 ;;
    --dir) [ "$#" -ge 2 ] || usage; DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage ;;
    *) echo "sandbox_daemon: unknown argument: $1" >&2; usage ;;
  esac
done

if [ -z "$BIN" ]; then
  BIN="$(command -v tm || true)"
  [ -n "$BIN" ] || die "no tm binary on PATH; pass --bin"
fi
[ -x "$BIN" ] || die "--bin is not an executable file: $BIN"
# Absolute, so a relative --bin still names the same file from the sandbox cwd.
BIN_DIR="$(resolve "$(dirname "$BIN")")"
[ -n "$BIN_DIR" ] || die "cannot resolve the directory of --bin: $BIN"
BIN="$BIN_DIR/$(basename "$BIN")"

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

# #9161: the daemon's cwd is <dir>/home, and `current_dir()` is the physical
# path, so the walk starts at the physical <dir>/home (a symlinked home must
# not escape the check) and covers <dir>/home/.env.local itself. It climbs to
# /, past every boundary the loader stops at, so it covers the loader's reach.
# Only an absent <dir>/home (mkdir creates it below) starts the walk at <dir>.
# `${anc%/*}`, not `$(dirname)`: command substitution strips a trailing newline
# from a directory name and would skip that directory.
if [ "${SANDBOX#<}" = "$SANDBOX" ]; then
  if [ -e "$SANDBOX/home" ] || [ -L "$SANDBOX/home" ]; then
    anc="$(resolve "$SANDBOX/home")"
    [ -n "$anc" ] || die "cannot resolve <dir>/home: $SANDBOX/home"
    # `$(resolve)` strips a trailing newline, so the result can name another dir.
    [ "$anc" -ef "$SANDBOX/home" ] || die "cannot resolve <dir>/home: $SANDBOX/home"
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

# #9557: the sandbox's own tmux socket directory and socket.
TMUX_DIR="$SANDBOX/data/tmux"
SOCK="$TMUX_DIR/tmux-$(id -u)/default"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#SOCK}" -gt 103 ]; then
  die "the tmux socket path is ${#SOCK} bytes, over the 103-byte sun_path limit: $SOCK; pick a shorter --dir"
fi
# #6288: the daemon socket every tm client dials, as
# trusty_common::daemon_socket_path("trusty-mpm") derives it under
# TRUSTY_DATA_DIR_OVERRIDE. It is longer than the tmux socket, so it is
# checked against sun_path too: the daemon would refuse to bind it.
DAEMON_SOCK="$SANDBOX/data/trusty-mpm/trusty-mpm.sock"
if [ "${SANDBOX#<}" = "$SANDBOX" ] && [ "${#DAEMON_SOCK}" -gt 103 ]; then
  die "the daemon socket path is ${#DAEMON_SOCK} bytes, over the 103-byte sun_path limit: $DAEMON_SOCK; pick a shorter --dir"
fi

# The FORWARDED names the caller exported, as NAME=value words for env -i.
# `compgen -e` lists exported names only: bash itself sets SHELL and TERM as
# plain shell variables, which the caller never passed. Values are read by
# indirect expansion and passed straight to exec; only names are printed.
EXPORTED=" $(compgen -e | tr '\n' ' ') "
PASSED_NAMES="$PINNED"
EXTRA=()
for name in $FORWARDED; do
  if [ "${EXPORTED#* "$name" }" != "$EXPORTED" ]; then
    EXTRA+=("$name=${!name}")
    PASSED_NAMES="$PASSED_NAMES $name"
  fi
done

echo "sandbox_daemon: sandbox dir: $SANDBOX"
echo "sandbox_daemon: daemon socket: $DAEMON_SOCK"
echo "sandbox_daemon: client: TRUSTY_MPM_SOCKET=$DAEMON_SOCK tm <command>"
echo "sandbox_daemon: variables passed (names only): $PASSED_NAMES"
echo "sandbox_daemon: working directory: $SANDBOX/home"
echo "sandbox_daemon: tmux socket: $SOCK"
echo "sandbox_daemon: command: env -i $PASSED_NAMES $BIN daemon --sandbox"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "sandbox_daemon: dry run; nothing started"
  exit 0
fi

mkdir -p "$SANDBOX/home" "$SANDBOX/data"
# #9161: never the caller's cwd; see the Working directory note in the header.
# CDPATH stays set: the path is absolute, and CDPATH never rewrites one.
cd "$SANDBOX/home" || die "cannot enter <dir>/home: $SANDBOX/home"
# #9557: 0700, since the daemon refuses a socket dir others can write.
{ mkdir -p "$TMUX_DIR" && chmod 700 "$TMUX_DIR"; } || die "cannot create the tmux socket dir: $TMUX_DIR"

# teardown: stop the sandbox's own tmux server, addressed by socket path only.
teardown() {
  if [ -S "$SOCK" ] && command -v tmux >/dev/null 2>&1; then
    env -i PATH="$PATH" tmux -S "$SOCK" kill-server >/dev/null 2>&1 || true
  fi
}

# `${EXTRA[@]+...}`: bash 3.2 treats an empty array as unset under `set -u`.
env -i \
  HOME="$SANDBOX/home" \
  PATH="$PATH" \
  TRUSTY_DATA_DIR_OVERRIDE="$SANDBOX/data" \
  TRUSTY_SANDBOX=1 \
  TMUX_TMPDIR="$TMUX_DIR" \
  ${EXTRA[@]+"${EXTRA[@]}"} \
  "$BIN" daemon --sandbox &
DAEMON_PID=$!
trap 'kill -TERM "$DAEMON_PID" 2>/dev/null || true' INT TERM HUP
# A trapped signal ends `wait` early; wait again until the daemon is reaped.
while :; do
  set +e
  wait "$DAEMON_PID"
  STATUS=$?
  set -e
  kill -0 "$DAEMON_PID" 2>/dev/null || break
done
teardown
exit "$STATUS"
