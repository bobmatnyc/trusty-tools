#!/usr/bin/env bash
#
# sandbox_secrets_console_selftest.sh — self-test for
# scripts/sandbox_secrets_console.sh (issue #9793).
#
# Why: the launcher's job is what it does NOT pass and what it refuses. A
#   launcher that forwarded one inherited credential, or bound the console
#   off loopback, would look exactly like a working one.
# What: runs the launcher against two stub binaries that record the variable
#   NAMES (and the values of four non-secret ones), arguments and working
#   directory they received, from a caller
#   environment (built with `env -i`) polluted with fake secrets. No real
#   server starts. Cases:
#     allowlist   the secrets stub sees exactly HOME PATH TRUSTY_DATA_DIR_OVERRIDE
#                 TRUSTY_SANDBOX; the console stub those plus TRUSTY_MPM_ROOT
#                 and TRUSTY_SECRETS_EXTERNAL (plus what /bin/sh adds itself)
#     values      those four variables carry TRUSTY_SANDBOX=1,
#                 TRUSTY_SECRETS_EXTERNAL=1, <dir>/data and <dir>/mpm exactly
#     argv        `serve --idle-timeout-secs 14400 --socket <dir>/home/...sock`
#                 and `serve --http 127.0.0.1:<port>`; cwd <dir>/home
#     no-leak     no fake secret value in the output or any file in <dir>
#     consent     <dir>/home/.trusty-tools/trusty-common/config.yaml selects file
#     projects    each seeded project has origin git@github.com:trusty-sandbox/...
#     registry    project-paths.json lists only existing directories: a
#                 project path that is a plain file is left out
#     teardown    TERM to the launcher stops both stubs and removes the pidfiles
#     stop        `--stop DIR` from another invocation stops both stubs
#     dry-run     with and without --dir, nothing is created
#     real-home   --dir naming the real home refuses (as a dry run)
#     env-local   a --dir below an ancestor `.env.local` refuses
#     bind        0.0.0.0, a LAN address and `localhost` refuse
#     rel-bin     a relative --secrets-bin or --console-bin refuses
#     bad-path    a --dir whose path holds a quote refuses (it is written to JSON)
#     bad-arg     an unknown argument is a usage error (exit 2)
#     tm-bin      --tm-bin <dir> and --tm-bin <dir>/tm give the console stub
#                 PATH=<dir>:/usr/bin:/bin and the secrets stub the default;
#                 without it both keep /usr/bin:/bin (#9979)
#     tm-bin-bad  a missing path, a dir or file with no executable tm, and a
#                 relative path refuse (exit 1)
#     events-*    (#11159) under umask 002 <dir>/data/trusty-events is 0700
#                 once the children run (the console stub mkdirs it as the
#                 real console does), in a new --dir, over a reused 0775 dir,
#                 over a 0777 dir, and with a relative --dir; a symlinked
#                 <dir>/data or <dir>/data/trusty-events refuses (exit 1)
#     events-bin-* (#11159) --events-bin starts a third child, with the four
#                 pinned names and `serve --socket <dir>/data/trusty-events/
#                 trusty-events.sock`, before the console (the console stub
#                 sees the socket); TERM stops it and removes events.pid; a
#                 stale socket is removed first, and a bus that binds none
#                 refuses (exit 1) before the console runs, with teardown; a
#                 relative path refuses; a live events pid refuses a second
#                 run, and `--stop DIR` stops it
#
# Usage: ./scripts/sandbox_secrets_console_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_secrets_console.sh"
PASSED=0
FAILED=0
# Physical, so it compares equal to the stub's `pwd -P`.
TMP_ROOT="$(cd "$(mktemp -d)" && pwd -P)"
LAUNCHER_PID=""
# shellcheck disable=SC2329  # invoked through the trap below
cleanup() {
  if [ -n "$LAUNCHER_PID" ]; then kill -TERM "$LAUNCHER_PID" 2>/dev/null || true; fi
  for d in "$TMP_ROOT"/*/; do
    [ -d "$d" ] && bash "$LAUNCHER" --stop "$d" >/dev/null 2>&1 || true
  done
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

FAKE_TOKEN="9793-selftest-fake-token-value"
FAKE_KEY="9793-selftest-fake-key-value"
PORT=17993

pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }
# expect_eq NAME GOT WANT: pass when GOT equals WANT.
expect_eq() {
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "saw [$2], want [$3]"; fi
}

# Each stub records under the HOME it was given, then runs until TERM.
mkdir -p "$TMP_ROOT/bin"
for role in secrets console events events-nobind; do
  cat > "$TMP_ROOT/bin/stub-$role" <<'STUB_EOF'
#!/bin/sh
role="${0##*/}"
env | cut -d= -f1 | sort > "$HOME/$role.names"
printf '%s\n' "$PATH" > "$HOME/$role.path"
# #9793: the four pinned variables with their values; none is a secret.
env | grep -E '^(TRUSTY_SANDBOX|TRUSTY_DATA_DIR_OVERRIDE|TRUSTY_MPM_ROOT|TRUSTY_SECRETS_EXTERNAL)=' \
  | LC_ALL=C sort > "$HOME/$role.values"
printf '%s\n' "$@" > "$HOME/$role.args"
pwd -P > "$HOME/$role.pwd"
# #11159: the real console resolves the bus socket with create_dir_all, so
# under umask 002 it makes <data>/trusty-events 0775 unless it already exists.
[ "$role" != stub-console ] || mkdir -p "$TRUSTY_DATA_DIR_OVERRIDE/trusty-events"
# #11159: the console records whether the bus socket existed when it started.
[ "$role" != stub-console ] || { [ -S "$TRUSTY_DATA_DIR_OVERRIDE/trusty-events/trusty-events.sock" ] && echo present || echo absent; } > "$HOME/console.bus"
# #11159: stub-events binds the --socket it was given, as the real bus does;
# stub-events-nobind never does. chdir keeps the bind under the sun_path limit.
if [ "$role" = stub-events ]; then
  python3 -I -c 'import os,socket,sys,time; d,n=os.path.split(sys.argv[1]); os.chdir(d); s=socket.socket(socket.AF_UNIX); s.bind(n); s.listen(1); time.sleep(3600)' "$3" &
  py=$!
  trap 'kill "$py" 2>/dev/null; exit 0' TERM INT
else
  trap 'exit 0' TERM INT
fi
while :; do sleep 0.1; done
STUB_EOF
  chmod +x "$TMP_ROOT/bin/stub-$role"
done
SB="$TMP_ROOT/bin/stub-secrets"
CB="$TMP_ROOT/bin/stub-console"
EB="$TMP_ROOT/bin/stub-events"
EBN="$TMP_ROOT/bin/stub-events-nobind"

# mode_of PATH: the permission bits in octal, e.g. 700; empty when absent.
mode_of() {
  case "$(uname -s)" in
    Darwin) stat -f %Lp "$1" 2>/dev/null || true ;;
    *) stat -c %a "$1" 2>/dev/null || true ;;
  esac
}

# The launcher from a cleared environment carrying fake secrets.
run_launcher() {
  env -i PATH="$PATH" TMPDIR="${RUN_TMPDIR:-$TMP_ROOT}" SANDBOX_SOCKET_WAIT_SECS=0 \
    TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" OPENAI_API_KEY="$FAKE_KEY" \
    OP_SERVICE_ACCOUNT_TOKEN="$FAKE_TOKEN" TRUSTY_CONSOLE_BIND=0.0.0.0:7788 \
    bash "$LAUNCHER" "$@"
}

# spawn_launcher ARGS...: run_launcher in the background; `exec` makes $!
# the launcher itself, so a TERM sent to $! reaches it.
spawn_launcher() {
  (
    exec env -i PATH="$PATH" TMPDIR="$TMP_ROOT" SANDBOX_SOCKET_WAIT_SECS="${RUN_WAIT_SECS:-0}" \
      TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" OPENAI_API_KEY="$FAKE_KEY" \
      OP_SERVICE_ACCOUNT_TOKEN="$FAKE_TOKEN" TRUSTY_CONSOLE_BIND=0.0.0.0:7788 \
      bash "$LAUNCHER" "$@"
  ) &
  LAUNCHER_PID=$!
}

# wait_for FILE: up to 10 s for FILE to exist.
wait_for() {
  local i=0
  while [ "$i" -lt 100 ] && [ ! -s "$1" ]; do sleep 0.1; i=$((i + 1)); done
  [ -s "$1" ]
}

# values FILE: the recorded NAME=value lines on one line.
values() { tr '\n' ' ' < "$1"; }

# names FILE: the recorded names without the ones /bin/sh exports itself.
names() { grep -vxE 'PWD|OLDPWD|SHLVL|_' "$1" | tr '\n' ' '; }

real_home() {
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$(id -un)" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# 1. A full stubbed run, started in the background and stopped with TERM.
DIR1="$TMP_ROOT/case1"
mkdir -p "$DIR1/projects"
# A project path that is a plain file must stay out of the registry.
: > "$DIR1/projects/sandbox-2"
spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR1" \
  --bind "127.0.0.1:$PORT" --projects 3 > "$TMP_ROOT/out1" 2>&1
H1="$DIR1/home"
if ! wait_for "$H1/stub-console.pwd" || ! wait_for "$H1/stub-secrets.pwd"; then
  fail allowlist "the stubs never ran"
  sed 's/^/    /' "$TMP_ROOT/out1"
else
  GOT="$(names "$H1/stub-secrets.names")"
  WANT="HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX "
  expect_eq "allowlist secrets" "$GOT" "$WANT"
  GOT="$(names "$H1/stub-console.names")"
  WANT="HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ROOT TRUSTY_SANDBOX TRUSTY_SECRETS_EXTERNAL "
  expect_eq "allowlist console" "$GOT" "$WANT"
  # #9793: the values, not only the names; a launcher pinning the wrong one fails.
  GOT="$(values "$H1/stub-secrets.values")"
  WANT="TRUSTY_DATA_DIR_OVERRIDE=$DIR1/data TRUSTY_SANDBOX=1 "
  expect_eq "values secrets" "$GOT" "$WANT"
  GOT="$(values "$H1/stub-console.values")"
  WANT="TRUSTY_DATA_DIR_OVERRIDE=$DIR1/data TRUSTY_MPM_ROOT=$DIR1/mpm TRUSTY_SANDBOX=1 TRUSTY_SECRETS_EXTERNAL=1 "
  expect_eq "values console" "$GOT" "$WANT"

  GOT="$(tr '\n' ' ' < "$H1/stub-secrets.args")"
  WANT="serve --idle-timeout-secs 14400 --socket $H1/.trusty-tools/trusty-secrets/secrets.sock "
  expect_eq "argv secrets" "$GOT" "$WANT"
  GOT="$(tr '\n' ' ' < "$H1/stub-console.args")"
  expect_eq "argv console" "$GOT" "serve --http 127.0.0.1:$PORT "
  if [ "$(cat "$H1/stub-secrets.pwd")" = "$H1" ] && [ "$(cat "$H1/stub-console.pwd")" = "$H1" ]; then
    pass cwd
  else
    fail cwd "stubs did not run in <dir>/home"
  fi

  CONSENT="$H1/.trusty-tools/trusty-common/config.yaml"
  if [ "$(cat "$CONSENT" 2>/dev/null)" = "$(printf 'secrets:\n  default_backend: file')" ]; then
    pass consent
  else
    fail consent "config.yaml does not select file"
  fi

  ok_projects=1
  for n in 1 3; do
    url="$(git -C "$DIR1/projects/sandbox-$n" remote get-url origin 2>/dev/null || true)"
    [ "$url" = "git@github.com:trusty-sandbox/sandbox-$n.git" ] || ok_projects=0
  done
  expect_eq projects "$ok_projects" 1

  REG="$DIR1/mpm/project-paths.json"
  if command -v python3 > /dev/null 2>&1; then
    REG_OUT="$(python3 -c 'import json, os, sys
rows = json.load(open(sys.argv[1]))
print(" ".join(r["alias"] for r in rows if os.path.isdir(r["path"])), len(rows))' "$REG" 2>&1 || true)"
    expect_eq registry "$REG_OUT" "sandbox-1 sandbox-3 2"
  else
    if grep -q 'sandbox-2' "$REG"; then fail registry "the plain file is listed"; else pass "registry (grep)"; fi
  fi

  if grep -rqF -e "$FAKE_TOKEN" -e "$FAKE_KEY" "$TMP_ROOT/out1" "$DIR1"; then
    fail no-leak "a fake secret reached the output or the sandbox"
  else
    pass no-leak
  fi

  SP="$(cat "$DIR1/secrets.pid")"
  CP="$(cat "$DIR1/console.pid")"
  kill -TERM "$LAUNCHER_PID"
  wait "$LAUNCHER_PID" 2>/dev/null || true
  LAUNCHER_PID=""
  if kill -0 "$SP" 2>/dev/null || kill -0 "$CP" 2>/dev/null; then
    fail teardown "a stub outlived the launcher"
  elif [ -e "$DIR1/secrets.pid" ] || [ -e "$DIR1/console.pid" ]; then
    fail teardown "a pidfile was left"
  else
    pass teardown
  fi
fi

# 2. --stop from another invocation.
DIR2="$TMP_ROOT/case2"
mkdir -p "$DIR2"
spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR2" \
  --bind "[::1]:$PORT" --projects 1 > "$TMP_ROOT/out2" 2>&1
if wait_for "$DIR2/home/stub-console.pwd" && wait_for "$DIR2/console.pid"; then
  SP="$(cat "$DIR2/secrets.pid")"
  CP="$(cat "$DIR2/console.pid")"
  if run_launcher --stop "$DIR2" > /dev/null 2>&1 && ! kill -0 "$SP" 2>/dev/null && ! kill -0 "$CP" 2>/dev/null; then
    pass stop
  else
    fail stop "--stop left a stub running"
  fi
else
  fail stop "the stubs never ran"
  sed 's/^/    /' "$TMP_ROOT/out2"
fi
wait "$LAUNCHER_PID" 2>/dev/null || true
LAUNCHER_PID=""

# 3. Dry runs create nothing, with and without --dir.
DIR3="$TMP_ROOT/case3"
mkdir -p "$DIR3" "$TMP_ROOT/dry-tmp"
set +e
run_launcher --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR3" --dry-run > /dev/null 2>&1
S_DIR=$?
RUN_TMPDIR="$TMP_ROOT/dry-tmp" run_launcher --secrets-bin "$SB" --console-bin "$CB" --dry-run > /dev/null 2>&1
S_TMP=$?
set -e
if [ "$S_DIR" -ne 0 ] || [ "$S_TMP" -ne 0 ]; then
  fail dry-run "exit $S_DIR / $S_TMP"
elif [ -n "$(ls -A "$DIR3")" ] || [ -n "$(ls -A "$TMP_ROOT/dry-tmp")" ]; then
  fail dry-run "a dry run created files"
else
  pass dry-run
fi

# expect_refusal NAME WANT_STATUS ARGS...: the launcher exits WANT_STATUS
# within 10 s. One that is still running did not refuse: it is stopped and
# the case fails, so a regression fails instead of hanging the selftest.
expect_refusal() {
  local name="$1" want="$2" pid status i=0
  shift 2
  spawn_launcher "$@" > "$TMP_ROOT/out-$name" 2>&1
  pid="$LAUNCHER_PID"
  while [ "$i" -lt 100 ] && kill -0 "$pid" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    LAUNCHER_PID=""
    fail "$name" "still running after 10 s; it did not refuse"
    return 0
  fi
  set +e
  wait "$pid"
  status=$?
  set -e
  LAUNCHER_PID=""
  if [ "$status" -eq "$want" ]; then pass "$name"; else fail "$name" "exit $status, want $want"; fi
}

DIR4="$TMP_ROOT/case4"
mkdir -p "$DIR4"
REAL_HOME="$(real_home)"
if [ -n "$REAL_HOME" ] && [ -d "$REAL_HOME" ]; then
  expect_refusal real-home 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$REAL_HOME" --dry-run
else
  echo "skip real-home: no password-database home"
fi
mkdir -p "$TMP_ROOT/envlocal/inner"
echo "X=1" > "$TMP_ROOT/envlocal/.env.local"
expect_refusal env-local 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$TMP_ROOT/envlocal/inner"
[ ! -e "$TMP_ROOT/envlocal/inner/home" ] || fail env-local-created "the refusal created <dir>/home"
for bind in 0.0.0.0:17890 192.168.1.10:17890 localhost:17890; do
  expect_refusal "bind-$bind" 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR4" --bind "$bind"
done
# From $TMP_ROOT, where the relative paths name the real stubs.
cd "$TMP_ROOT"
expect_refusal rel-secrets-bin 1 --secrets-bin bin/stub-secrets --console-bin "$CB" --dir "$DIR4"
expect_refusal rel-console-bin 1 --secrets-bin "$SB" --console-bin bin/stub-console --dir "$DIR4"
cd "$SCRIPT_DIR"
mkdir -p "$TMP_ROOT/quo\"te"
expect_refusal bad-path 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$TMP_ROOT/quo\"te"
[ -z "$(ls -A "$TMP_ROOT/quo\"te")" ] || fail bad-path-created "the refusal created files"
expect_refusal bad-arg 2 --secrets-bin "$SB" --console-bin "$CB" --nope
[ -z "$(ls -A "$DIR4")" ] || fail refusals-created "a refused run created files in --dir"

# #9979: --tm-bin prepends one directory to the console's PATH, nothing else.
mkdir -p "$TMP_ROOT/tmbin" "$TMP_ROOT/tmbin-empty"
printf '#!/bin/sh\nexit 0\n' > "$TMP_ROOT/tmbin/tm"
chmod +x "$TMP_ROOT/tmbin/tm"
TMB="$(cd "$TMP_ROOT/tmbin" && pwd -P)"
run_tm_bin_case() {
  local name="$1" arg="$2" d="$TMP_ROOT/case-$1"
  mkdir -p "$d"
  spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --dir "$d" --bind "127.0.0.1:$PORT" --projects 1 \
    ${arg:+--tm-bin "$arg"} > "$TMP_ROOT/out-$name" 2>&1
  if wait_for "$d/home/stub-console.path" && wait_for "$d/home/stub-secrets.path"; then
    expect_eq "$name console PATH" "$(cat "$d/home/stub-console.path")" "${arg:+$TMB:}/usr/bin:/bin"
    expect_eq "$name secrets PATH" "$(cat "$d/home/stub-secrets.path")" "/usr/bin:/bin"
  else
    fail "$name" "the stubs never ran"
    sed 's/^/    /' "$TMP_ROOT/out-$name"
  fi
  kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
  wait "$LAUNCHER_PID" 2>/dev/null || true
  LAUNCHER_PID=""
  bash "$LAUNCHER" --stop "$d" > /dev/null 2>&1 || true
}
run_tm_bin_case tm-bin-dir "$TMB"
run_tm_bin_case tm-bin-file "$TMB/tm"
run_tm_bin_case tm-bin-none ""
DIR5="$TMP_ROOT/case5"
mkdir -p "$DIR5"
: > "$TMP_ROOT/tmbin-empty/notexec"
for bad in "$TMP_ROOT/no-such-dir" "$TMP_ROOT/tmbin-empty" "$TMP_ROOT/tmbin-empty/notexec" "tmbin"; do
  expect_refusal "tm-bin-bad-${bad##*/}" 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR5" --tm-bin "$bad"
done
[ -z "$(ls -A "$DIR5")" ] || fail tm-bin-bad-created "a refused run created files in --dir"

# #11159: under umask 002 the bus dir is 0700 once the children run.
# run_events_case NAME PREMODE [rel]: PREMODE is a mode to pre-create the dir
# with, or "-" for none; "rel" passes --dir as a relative path.
run_events_case() {
  local name="$1" pre="$2" rel="${3:-}" d="$TMP_ROOT/case-$1" arg old_umask
  mkdir -p "$d"
  arg="$d"
  [ "$rel" != rel ] || arg="${d##*/}"
  old_umask="$(umask)"
  umask 002
  [ "$pre" = "-" ] || { mkdir -p "$d/data/trusty-events"; chmod "$pre" "$d/data/trusty-events"; }
  ( cd "$TMP_ROOT" && exec env -i PATH="$PATH" TMPDIR="$TMP_ROOT" SANDBOX_SOCKET_WAIT_SECS=0 \
      bash "$LAUNCHER" --secrets-bin "$SB" --console-bin "$CB" --dir "$arg" \
      --bind "127.0.0.1:$PORT" --projects 1 ) > "$TMP_ROOT/out-$name" 2>&1 &
  LAUNCHER_PID=$!
  umask "$old_umask"
  if wait_for "$d/home/stub-console.pwd"; then
    expect_eq "$name dir mode" "$(mode_of "$d/data/trusty-events")" 700
  else
    fail "$name" "the stubs never ran"
    sed 's/^/    /' "$TMP_ROOT/out-$name"
  fi
  kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
  wait "$LAUNCHER_PID" 2>/dev/null || true
  LAUNCHER_PID=""
  bash "$LAUNCHER" --stop "$d" > /dev/null 2>&1 || true
}
run_events_case events-new -
run_events_case events-reused 775
run_events_case events-wrong-mode 777
run_events_case events-rel-dir - rel

# A symlinked <dir>/data or <dir>/data/trusty-events refuses, creating nothing.
DIR7="$TMP_ROOT/case7"
mkdir -p "$DIR7/real"
ln -s "$DIR7/real" "$DIR7/data"
expect_refusal events-data-symlink 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR7"
[ -z "$(ls -A "$DIR7/real")" ] || fail events-data-symlink-created "a refused run wrote through the symlink"
DIR8="$TMP_ROOT/case8"
mkdir -p "$DIR8/data" "$DIR8/real"
ln -s "$DIR8/real" "$DIR8/data/trusty-events"
expect_refusal events-dir-symlink 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$DIR8"
[ -z "$(ls -A "$DIR8/real")" ] || fail events-dir-symlink-created "a refused run wrote through the symlink"

# #11159: --events-bin starts the bus before the console, with the pinned names
# and socket; TERM stops it. RUN_WAIT_SECS gives the stub time to bind.
alive() { kill -0 "$1" 2>/dev/null; }
# gone PATH...: true when none of the paths exists.
gone() { local f; for f in "$@"; do [ ! -e "$f" ] || return 1; done; }
RUN_WAIT_SECS=5
D9="$TMP_ROOT/case-events-bin"
mkdir -p "$D9"
spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --events-bin "$EB" --dir "$D9" \
  --bind "127.0.0.1:$PORT" --projects 1 > "$TMP_ROOT/out-events-bin" 2>&1
if wait_for "$D9/home/stub-console.pwd" && wait_for "$D9/home/stub-events.args"; then
  expect_eq events-bin-argv "$(tr '\n' ' ' < "$D9/home/stub-events.args")" \
    "serve --socket $D9/data/trusty-events/trusty-events.sock "
  expect_eq events-bin-allowlist "$(names "$D9/home/stub-events.names")" \
    "HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX "
  expect_eq events-bin-before-console "$(cat "$D9/home/console.bus")" present
  expect_eq events-bin-dir-mode "$(mode_of "$D9/data/trusty-events")" 700
else
  fail events-bin "the stubs never ran"
  sed 's/^/    /' "$TMP_ROOT/out-events-bin"
fi
EP9=""
[ -f "$D9/events.pid" ] && EP9="$(cat "$D9/events.pid")"
kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
wait "$LAUNCHER_PID" 2>/dev/null || true
LAUNCHER_PID=""
if [ -n "$EP9" ] && ! alive "$EP9" && [ ! -e "$D9/events.pid" ]; then
  pass events-bin-teardown
else
  fail events-bin-teardown "the events stub outlived the launcher, or left its pidfile"
fi
bash "$LAUNCHER" --stop "$D9" > /dev/null 2>&1 || true

# Without --events-bin the console sees no bus socket.
D10="$TMP_ROOT/case-events-none"
mkdir -p "$D10"
spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --dir "$D10" \
  --bind "127.0.0.1:$PORT" --projects 1 > "$TMP_ROOT/out-events-none" 2>&1
if wait_for "$D10/home/console.bus"; then
  expect_eq events-none-no-bus "$(cat "$D10/home/console.bus")" absent
  if gone "$D10/events.pid"; then pass events-none-no-pidfile; else fail events-none-no-pidfile "events.pid exists"; fi
else
  fail events-none "the console stub never ran"
fi
kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
wait "$LAUNCHER_PID" 2>/dev/null || true
LAUNCHER_PID=""
bash "$LAUNCHER" --stop "$D10" > /dev/null 2>&1 || true

# A stale socket from an earlier run is removed, and a bus that binds none
# refuses before the console runs (exit 1), leaving no child behind.
D11="$TMP_ROOT/case-events-stale"
mkdir -p "$D11/data/trusty-events"
chmod 700 "$D11/data/trusty-events"
python3 -I -c 'import os,socket,sys; d,n=os.path.split(sys.argv[1]); os.chdir(d); socket.socket(socket.AF_UNIX).bind(n)' \
  "$D11/data/trusty-events/trusty-events.sock"
[ -S "$D11/data/trusty-events/trusty-events.sock" ] || fail events-stale-setup "no stale socket made"
RUN_WAIT_SECS=1
expect_refusal events-bin-nobind 1 --secrets-bin "$SB" --console-bin "$CB" --events-bin "$EBN" --dir "$D11" \
  --bind "127.0.0.1:$PORT" --projects 1
RUN_WAIT_SECS=5
if gone "$D11/data/trusty-events/trusty-events.sock"; then pass events-stale-removed; else fail events-stale-removed "the stale socket survived"; fi
if gone "$D11/home/stub-console.pwd"; then pass events-nobind-no-console; else fail events-nobind-no-console "the console started"; fi
if gone "$D11/events.pid" "$D11/secrets.pid"; then pass events-nobind-teardown; else fail events-nobind-teardown "a pidfile remains"; fi

D12="$TMP_ROOT/case-events-rel"
mkdir -p "$D12"
expect_refusal rel-events-bin 1 --secrets-bin "$SB" --console-bin "$CB" --dir "$D12" --events-bin bin/stub-events
[ -z "$(ls -A "$D12")" ] || fail rel-events-bin-created "a refused run created files in --dir"

# A live events pid refuses a second run, and --stop stops it. The launcher is
# killed, then the secrets and console children, so only the bus is left.
D13="$TMP_ROOT/case-events-live"
mkdir -p "$D13"
spawn_launcher --secrets-bin "$SB" --console-bin "$CB" --events-bin "$EB" --dir "$D13" \
  --bind "127.0.0.1:$PORT" --projects 1 > "$TMP_ROOT/out-events-live" 2>&1
if wait_for "$D13/console.pid" && wait_for "$D13/events.pid" && wait_for "$D13/home/console.bus"; then
  kill -KILL "$LAUNCHER_PID" 2>/dev/null || true
  wait "$LAUNCHER_PID" 2>/dev/null || true
  LAUNCHER_PID=""
  EP13="$(cat "$D13/events.pid")"
  for role in secrets console; do kill -TERM "$(cat "$D13/$role.pid")" 2>/dev/null || true; done
  sleep 0.5
  alive "$EP13" || fail events-live-setup "the events stub is not running"
  expect_refusal events-live-pid 1 --secrets-bin "$SB" --console-bin "$CB" --events-bin "$EB" --dir "$D13" \
    --bind "127.0.0.1:$PORT" --projects 1
  if grep -q "(events) is recorded live" "$TMP_ROOT/out-events-live-pid"; then
    pass events-live-pid-names-role
  else
    fail events-live-pid-names-role "the refusal does not name the events child"
  fi
  bash "$LAUNCHER" --stop "$D13" > /dev/null 2>&1 || true
  if ! alive "$EP13" && [ ! -e "$D13/events.pid" ]; then pass events-stop; else fail events-stop "--stop left the events stub or its pidfile"; fi
else
  fail events-live "the stubs never ran"
  kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
  LAUNCHER_PID=""
fi
RUN_WAIT_SECS=0

echo "sandbox_secrets_console_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
