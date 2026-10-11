#!/usr/bin/env bash
#
# sandbox_search_daemon_selftest.sh — self-test for
# scripts/sandbox_search_daemon.sh (issues #8275, #9121).
#
# Why: the launcher's whole job is what it does NOT pass and what it will NOT
#   kill. A launcher that forwarded one inherited credential, or signalled a
#   pid it did not spawn, would look exactly like a working one.
# What: runs the launcher with a stub `--bin` (a shell script that records its
#   environment names and argv; no real trusty-search starts) from a caller
#   environment polluted with fake secrets. Cases:
#     allowlist   a dry run and a real (stubbed) run show only the allowlisted
#                 names; the fake GITHUB_TOKEN / OPENROUTER_API_KEY and a
#                 *_KEY lookalike never reach the stub or the output
#     argv        exactly --foreground, --no-auto-discover and a --data-dir
#                 under the sandbox dir; no --port (#9214)
#     no-port     `--port` is refused as an unknown argument (#9214)
#     stop-owned  `--stop DIR` kills the recorded pid when its argv holds DIR
#     stop-decoy  `--stop DIR` refuses a recorded pid whose argv lacks DIR and
#                 leaves that process alive
#     stop-bad    a non-numeric or group-shaped pid refuses
#     stop-sibling a decoy whose argv names `<dir>-other` or `<dir>/data-old`
#                 is refused and survives (the match is a whole token)
#     signal      TERM to the launcher kills the child it spawned, removes
#                 sandbox.pid, and the launcher reported the socket the daemon
#                 bound at <dir>/data/trusty-search.sock
#     ignore-term a child that ignores TERM is ended by KILL, and a socket file
#                 an earlier run left is removed first, so a child that binds
#                 nothing is reported as not serving
#     cwd         the child runs under <dir>/home, never the caller's cwd, and
#                 no ancestor of that cwd holds a `.env.local`; a --dir under
#                 an ancestor `.env.local` refuses
#                 a symlinked <dir>/home into a tree under a `.env.local`, and a
#                 <dir>/home/.env.local itself, refuse
#                 a --dir under an ancestor whose name ends in a newline and holds
#                 `.env.local` refuses; a <dir>/home that exists but cannot be
#                 resolved (a dangling symlink) dies
#     stop-dead   --stop on a pid already dead says so and removes sandbox.pid
#     live-dir    a start refuses while sandbox.pid names a live daemon of the
#                 same dir, and leaves that daemon and its socket alone
#     live-socket a start refuses while <dir>/data's socket accepts a
#                 connection that sandbox.pid does not record, and leaves the
#                 listener and its socket alone (#9214)
#     no-python3  with no python3 on PATH, a start refuses on a stale socket
#                 file ("cannot tell") and leaves that file alone (#9214)
#     model-cache --model-cache is forwarded as FASTEMBED_CACHE_DIR; a missing
#                 directory refuses
#
# Usage: ./scripts/sandbox_search_daemon_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A holder stub binds a socket under $TRUSTY_DATA_DIR; never the caller's.
unset TRUSTY_DATA_DIR
# #9214: the holder stub stands in for a daemon by binding a real Unix socket.
PYTHON="$(command -v python3 || true)"
if [ -z "$PYTHON" ]; then
  echo "sandbox_search_daemon selftest: python3 is required to bind a stub socket" >&2
  exit 1
fi
LAUNCHER="$SCRIPT_DIR/sandbox_search_daemon.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(cd "$(mktemp -d)" && pwd -P)"
DECOY_PID=""
cleanup() {
  [ -z "$DECOY_PID" ] || kill "$DECOY_PID" 2>/dev/null || true
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

FAKE_TOKEN="9121-selftest-fake-token-value"
FAKE_KEY="9121-selftest-fake-key-value"

pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }

# One-shot stub: records env names and argv under the HOME it was given.
STUB="$TMP_ROOT/stub-ts"
cat > "$STUB" <<'STUB_EOF'
#!/bin/sh
env | cut -d= -f1 | sort > "$HOME/stub-env-names"
printf '%s\n' "$@" > "$HOME/stub-args"
pwd -P > "$HOME/stub-pwd"
STUB_EOF
chmod +x "$STUB"

# Long-lived stub: the same record, binds the daemon's socket, then stays up
# until signalled.
HOLD="$TMP_ROOT/stub-ts-hold"
cat > "$HOLD" <<STUB_EOF
#!/bin/sh
env | cut -d= -f1 | sort > "\$HOME/stub-env-names"
[ -z "\${TRUSTY_DATA_DIR:-}" ] || "$PYTHON" -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "\$TRUSTY_DATA_DIR/trusty-search.sock"
while :; do sleep 1; done
STUB_EOF
chmod +x "$HOLD"

# Long-lived stub that ignores TERM and never binds a socket.
IGNORE="$TMP_ROOT/stub-ts-ignore"
cat > "$IGNORE" <<'STUB_EOF'
#!/bin/sh
trap '' TERM
while :; do sleep 1; done
STUB_EOF
chmod +x "$IGNORE"

# The launcher, run from a cleared environment carrying fake secrets, three
# allowlisted knobs, and a lookalike name.
run_launcher() {
  env -i PATH="$PATH" RUST_LOG=warn TRUSTY_REDB_CACHE_MB=64 TRUSTY_EMBED_INFLIGHT=2 \
    GITHUB_TOKEN="$FAKE_TOKEN" OPENROUTER_API_KEY="$FAKE_KEY" ANTHROPIC_API_KEY="$FAKE_KEY" \
    SLACK_BOT_TOKEN="$FAKE_TOKEN" SELFTEST_SECRET_KEY="$FAKE_KEY" bash "$LAUNCHER" "$@"
}

leaks() {
  case "$1" in
    *"$FAKE_TOKEN"*|*"$FAKE_KEY"*) return 0 ;;
    *) return 1 ;;
  esac
}

# expect_refusal <name> <want-exit> <needle> <launcher args...>
expect_refusal() {
  local name="$1" want="$2" needle="$3" out status
  shift 3
  set +e
  out="$(run_launcher "$@" 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq "$want" ] && printf '%s' "$out" | grep -qF -- "$needle"; then
    pass "$name"
  else
    fail "$name" "exit $status, want $want with '$needle': $out"
  fi
}

# 1. allowlist (dry run): the printed env words are exactly the allowlist.
DIR1="$TMP_ROOT/case1"
mkdir -p "$DIR1"
set +e
OUT1="$(run_launcher --bin "$STUB" --dir "$DIR1" --dry-run 2>&1)"
STATUS1=$?
set -e
GOT1="$(printf '%s\n' "$OUT1" | sed -n 's/^  \([A-Z_]*\)=.*/\1/p' | tr '\n' ' ')"
WANT1="HOME PATH TRUSTY_DATA_DIR TRUSTY_SANDBOX TRUSTY_REDB_CACHE_MB TRUSTY_EMBED_INFLIGHT RUST_LOG "
if [ "$STATUS1" -ne 0 ]; then
  fail allowlist "dry run exit $STATUS1"
elif [ "$GOT1" != "$WANT1" ]; then
  fail allowlist "dry run env [$GOT1], want [$WANT1]"
elif leaks "$OUT1"; then
  fail allowlist "a fake secret value reached the dry-run output"
elif [ -e "$DIR1/home" ]; then
  fail allowlist "a dry run created $DIR1/home"
else
  pass allowlist-dry-run
fi

# 2. allowlist (real run) + argv: a stubbed start.
DIR2="$TMP_ROOT/case2"
mkdir -p "$DIR2"
set +e
OUT2="$(run_launcher --bin "$STUB" --dir "$DIR2" 2>&1)"
STATUS2=$?
set -e
if [ "$STATUS2" -ne 0 ] || [ ! -f "$DIR2/home/stub-env-names" ]; then
  fail allowlist "real run exit $STATUS2 or the stub never ran"
  printf '%s\n' "$OUT2" | sed 's/^/    /'
else
  GOT2="$(grep -vxE 'PWD|OLDPWD|SHLVL|_' "$DIR2/home/stub-env-names" | tr '\n' ' ')"
  WANT2="HOME PATH RUST_LOG TRUSTY_DATA_DIR TRUSTY_EMBED_INFLIGHT TRUSTY_REDB_CACHE_MB TRUSTY_SANDBOX "
  if [ "$GOT2" != "$WANT2" ]; then
    fail allowlist "stub saw [$GOT2], want [$WANT2]"
  elif leaks "$OUT2"; then
    fail allowlist "a fake secret value reached the output"
  else
    pass allowlist-real-run
  fi
  ARGS2="$(tr '\n' ' ' < "$DIR2/home/stub-args" 2>/dev/null || true)"
  if [ "$ARGS2" = "start --foreground --no-auto-discover --data-dir $DIR2/data " ]; then
    pass argv
  else
    fail argv "stub argv was [$ARGS2]"
  fi
fi

# 3. no-port (#9214): the daemon binds no TCP port, so `--port` is unknown.
expect_refusal no-port 2 "unknown argument: --port" \
  --bin "$STUB" --dir "$DIR1" --port 17999 --dry-run

# 4. stop-owned: --stop kills the recorded pid when its argv holds DIR.
DIR4="$TMP_ROOT/case4"
mkdir -p "$DIR4"
cp "$HOLD" "$DIR4/holder"
"$DIR4/holder" --data-dir "$DIR4/data" &
OWNED_PID=$!
sleep 0.3
echo "$OWNED_PID" > "$DIR4/sandbox.pid"
set +e
OUT4="$(run_launcher --stop "$DIR4" 2>&1)"
STATUS4=$?
set -e
sleep 0.3
if [ "$STATUS4" -eq 0 ] && ! kill -0 "$OWNED_PID" 2>/dev/null && [ ! -e "$DIR4/sandbox.pid" ]; then
  pass stop-owned
else
  kill "$OWNED_PID" 2>/dev/null || true
  fail stop-owned "exit $STATUS4; $OUT4"
fi

# 5. stop-decoy: a pid whose argv lacks DIR is refused and survives.
DIR5="$TMP_ROOT/case5"
mkdir -p "$DIR5"
sleep 120 &
DECOY_PID=$!
echo "$DECOY_PID" > "$DIR5/sandbox.pid"
set +e
OUT5="$(run_launcher --stop "$DIR5" 2>&1)"
STATUS5=$?
set -e
if [ "$STATUS5" -eq 1 ] && kill -0 "$DECOY_PID" 2>/dev/null \
    && printf '%s' "$OUT5" | grep -qF "nothing signalled"; then
  pass stop-decoy
else
  fail stop-decoy "exit $STATUS5, decoy alive: $(kill -0 "$DECOY_PID" 2>/dev/null && echo yes || echo no)"
fi
kill "$DECOY_PID" 2>/dev/null || true
DECOY_PID=""

# 6. stop-bad: pids that are not a plain process id refuse.
for bad in "-1" "0" "1" "abc" "12 34"; do
  echo "$bad" > "$DIR5/sandbox.pid"
  set +e
  run_launcher --stop "$DIR5" > /dev/null 2>&1
  S=$?
  set -e
  if [ "$S" -eq 1 ]; then pass "stop-bad[$bad]"; else fail stop-bad "pid [$bad] exit $S"; fi
done

# 7. signal: TERM to the launcher kills the child it spawned.
DIR7="$TMP_ROOT/case7"
mkdir -p "$DIR7"
# `exec` in the subshell makes $! the launcher's own pid, not a wrapper's.
(exec env -i PATH="$PATH" GITHUB_TOKEN="$FAKE_TOKEN" bash "$LAUNCHER" \
  --bin "$HOLD" --dir "$DIR7") > "$TMP_ROOT/case7.out" 2>&1 &
LAUNCH_PID=$!
i=0
while [ "$i" -lt 50 ] && [ ! -s "$DIR7/sandbox.pid" ]; do sleep 0.2; i=$((i + 1)); done
i=0
while [ "$i" -lt 50 ] && ! grep -qF "serving socket: $DIR7/data/trusty-search.sock" "$TMP_ROOT/case7.out" 2>/dev/null; do
  sleep 0.2; i=$((i + 1))
done
if grep -qF "serving socket: $DIR7/data/trusty-search.sock" "$TMP_ROOT/case7.out"; then
  pass socket-reported
else
  fail socket-reported "no 'serving socket: $DIR7/data/trusty-search.sock' in: $(cat "$TMP_ROOT/case7.out")"
fi
CHILD7="$(tr -d ' \n' < "$DIR7/sandbox.pid" 2>/dev/null || true)"
if [ -z "$CHILD7" ] || ! kill -0 "$CHILD7" 2>/dev/null; then
  fail signal "the child never started"
else
  kill -TERM "$LAUNCH_PID"
  set +e
  wait "$LAUNCH_PID"
  set -e
  sleep 0.3
  if kill -0 "$CHILD7" 2>/dev/null; then
    kill "$CHILD7" 2>/dev/null || true
    fail signal "the child $CHILD7 survived TERM to the launcher"
  elif [ -e "$DIR7/sandbox.pid" ]; then
    fail signal "sandbox.pid was not removed"
  else
    pass signal
  fi
fi

# 8. model-cache: forwarded as FASTEMBED_CACHE_DIR; a missing directory refuses.
CACHE="$TMP_ROOT/model-cache"
mkdir -p "$CACHE"
set +e
OUT8="$(run_launcher --bin "$STUB" --dir "$DIR1" --model-cache "$CACHE" --dry-run 2>&1)"
S8=$?
OUT8B="$(run_launcher --bin "$STUB" --dir "$DIR1" --model-cache "$TMP_ROOT/nope" --dry-run 2>&1)"
S8B=$?
set -e
if [ "$S8" -eq 0 ] && printf '%s' "$OUT8" | grep -qF "  FASTEMBED_CACHE_DIR=$CACHE" \
    && [ "$S8B" -eq 1 ] && printf '%s' "$OUT8B" | grep -qF "not an existing directory"; then
  pass model-cache
else
  fail model-cache "forwarded exit $S8, missing-dir exit $S8B"
fi

# 9. stop-sibling: a decoy whose argv names a sibling of DIR is not killed.
DIR9="$TMP_ROOT/case9"
mkdir -p "$DIR9" "$DIR9-other"
for decoy_arg in "$DIR9-other/data" "$DIR9/data-old"; do
  cp "$HOLD" "$DIR9-other/decoy"
  "$DIR9-other/decoy" --data-dir "$decoy_arg" &
  DECOY_PID=$!
  sleep 0.3
  echo "$DECOY_PID" > "$DIR9/sandbox.pid"
  set +e
  OUT9="$(run_launcher --stop "$DIR9" 2>&1)"
  S9=$?
  set -e
  if [ "$S9" -eq 1 ] && kill -0 "$DECOY_PID" 2>/dev/null && [ -e "$DIR9/sandbox.pid" ]; then
    pass "stop-sibling[${decoy_arg#"$TMP_ROOT"/}]"
  else
    fail stop-sibling "[$decoy_arg] exit $S9: $OUT9"
  fi
  kill "$DECOY_PID" 2>/dev/null || true
  DECOY_PID=""
done

# 10. ignore-term: KILL ends a TERM-ignoring child; a stale socket file from an
# earlier run is removed first, so a child that binds nothing is not serving.
DIR10="$TMP_ROOT/case10"
mkdir -p "$DIR10/data"
"$PYTHON" -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' \
  "$DIR10/data/trusty-search.sock"
(exec env -i PATH="$PATH" SANDBOX_SOCKET_WAIT_SECS=1 bash "$LAUNCHER" \
  --bin "$IGNORE" --dir "$DIR10") > "$TMP_ROOT/case10.out" 2>&1 &
LAUNCH10=$!
i=0
while [ "$i" -lt 50 ] && ! grep -q "no socket at" "$TMP_ROOT/case10.out" 2>/dev/null; do
  sleep 0.2; i=$((i + 1))
done
CHILD10="$(tr -d ' \n' < "$DIR10/sandbox.pid" 2>/dev/null || true)"
if ! grep -q "no socket at" "$TMP_ROOT/case10.out"; then
  fail ignore-term "no 'no socket at' report: $(cat "$TMP_ROOT/case10.out")"
elif grep -q "serving socket: " "$TMP_ROOT/case10.out"; then
  fail ignore-term "reported a stale socket file as serving"
fi
kill -TERM "$LAUNCH10" 2>/dev/null || true
set +e
wait "$LAUNCH10"
set -e
if [ -n "$CHILD10" ] && kill -0 "$CHILD10" 2>/dev/null; then
  kill -KILL "$CHILD10" 2>/dev/null || true
  fail ignore-term "the TERM-ignoring child $CHILD10 survived teardown"
elif [ -e "$DIR10/sandbox.pid" ]; then
  fail ignore-term "sandbox.pid was not removed after the child died"
else
  pass ignore-term
fi

# 11. cwd: the child runs under <dir>/home even when the caller sits beside a
# `.env.local`, and a --dir below an ancestor `.env.local` refuses.
LEAK="$TMP_ROOT/leak"
DIR11="$TMP_ROOT/case11"
mkdir -p "$LEAK" "$DIR11"
echo "OPENROUTER_API_KEY=$FAKE_KEY" > "$LEAK/.env.local"
set +e
(cd "$LEAK" && run_launcher --bin "$STUB" --dir "$DIR11" > /dev/null 2>&1)
S11=$?
set -e
CWD11="$(cat "$DIR11/home/stub-pwd" 2>/dev/null || true)"
ANCESTOR_HIT=""
a="$CWD11"
while [ -n "$a" ]; do
  [ ! -f "$a/.env.local" ] || ANCESTOR_HIT="$a"
  [ "$a" != "/" ] || break
  a="$(dirname "$a")"
done
case "$CWD11" in
  "$DIR11"/home)
    if [ "$S11" -eq 0 ] && [ -z "$ANCESTOR_HIT" ]; then pass cwd
    else fail cwd "exit $S11, .env.local at [$ANCESTOR_HIT]"; fi ;;
  *) fail cwd "child cwd was [$CWD11], want $DIR11/home" ;;
esac
mkdir -p "$LEAK/sb"
expect_refusal cwd-env-local 1 ".env.local would be loaded" \
  --bin "$STUB" --dir "$LEAK/sb" --dry-run

# 11b. a home symlinked into a tree whose ancestor holds `.env.local` refuses
# (the daemon's cwd is the physical path); so does <dir>/home/.env.local itself.
LEAK2="$TMP_ROOT/leak2"
DIR13="$TMP_ROOT/case13"
mkdir -p "$LEAK2/real" "$DIR13"
touch "$LEAK2/.env.local"
ln -s "$LEAK2/real" "$DIR13/home"
expect_refusal cwd-symlinked-home 1 ".env.local would be loaded" \
  --bin "$STUB" --dir "$DIR13" --dry-run
DIR14="$TMP_ROOT/case14"
mkdir -p "$DIR14/home"
touch "$DIR14/home/.env.local"
expect_refusal cwd-home-env-local 1 "$DIR14/home/.env.local would be loaded" \
  --bin "$STUB" --dir "$DIR14" --dry-run

# 11d. an ancestor whose name ends in a newline: `$(dirname ...)` would strip
# it and miss the `.env.local` inside.
NL_ANC="$TMP_ROOT/nl
"
DIR16="$NL_ANC/case16"
mkdir -p "$DIR16"
touch "$NL_ANC/.env.local"
expect_refusal cwd-newline-ancestor 1 ".env.local would be loaded" \
  --bin "$STUB" --dir "$DIR16" --dry-run

# 11e. a <dir>/home that exists but does not resolve dies (a dangling symlink
# would otherwise leave <dir>/home/.env.local unchecked).
DIR17="$TMP_ROOT/case17"
mkdir -p "$DIR17"
ln -s "$TMP_ROOT/no-such-target" "$DIR17/home"
expect_refusal cwd-unresolvable-home 1 "cannot be resolved" \
  --bin "$STUB" --dir "$DIR17" --dry-run

# 11c. stop-dead: a pid already dead on entry is reported as such.
DIR15="$TMP_ROOT/case15"
mkdir -p "$DIR15"
sh -c 'exit 0' &
DEAD_PID=$!
wait "$DEAD_PID" || true
echo "$DEAD_PID" > "$DIR15/sandbox.pid"
set +e
OUT15="$(run_launcher --stop "$DIR15" 2>&1)"
S15=$?
set -e
if [ "$S15" -eq 0 ] && [ ! -e "$DIR15/sandbox.pid" ] \
    && printf '%s' "$OUT15" | grep -qF "pid $DEAD_PID already dead; removed sandbox.pid"; then
  pass stop-dead
else
  fail stop-dead "exit $S15: $OUT15"
fi

# 12. live-dir (#9214): a start refuses while sandbox.pid names a live daemon
# of this dir, and leaves that daemon and its socket in place.
DIR12="$TMP_ROOT/case12"
mkdir -p "$DIR12/data"
cp "$HOLD" "$DIR12/holder"
"$DIR12/holder" --data-dir "$DIR12/data" &
DECOY_PID=$!
sleep 0.3
echo "$DECOY_PID" > "$DIR12/sandbox.pid"
touch "$DIR12/data/trusty-search.sock"
set +e
OUT12="$(run_launcher --bin "$STUB" --dir "$DIR12" 2>&1)"
S12=$?
set -e
if [ "$S12" -eq 1 ] && printf '%s' "$OUT12" | grep -qF "running sandbox daemon of this dir" \
    && kill -0 "$DECOY_PID" 2>/dev/null && [ -e "$DIR12/data/trusty-search.sock" ]; then
  pass live-dir
else
  fail live-dir "exit $S12: $OUT12"
fi
kill "$DECOY_PID" 2>/dev/null || true
DECOY_PID=""

# 13. live-socket (#9214): no sandbox.pid, but a listener accepts on
# <dir>/data's socket. The start refuses and unlinks nothing.
DIR18="$TMP_ROOT/case18"
SOCK18="$DIR18/data/trusty-search.sock"
mkdir -p "$DIR18/data"
"$PYTHON" -c 'import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(8)
open(sys.argv[2], "w").close()
time.sleep(120)' "$SOCK18" "$DIR18/listening" &
DECOY_PID=$!
i=0
while [ "$i" -lt 50 ] && [ ! -e "$DIR18/listening" ]; do sleep 0.1; i=$((i + 1)); done
set +e
OUT18="$(run_launcher --bin "$STUB" --dir "$DIR18" 2>&1)"
S18=$?
set -e
if [ ! -e "$DIR18/listening" ]; then
  fail live-socket "the listener never bound $SOCK18"
elif [ "$S18" -eq 1 ] && printf '%s' "$OUT18" | grep -qF "$SOCK18 accepts connections" \
    && [ -S "$SOCK18" ] && kill -0 "$DECOY_PID" 2>/dev/null && [ ! -e "$DIR18/home/stub-args" ]; then
  pass live-socket
else
  fail live-socket "exit $S18, socket kept: $([ -S "$SOCK18" ] && echo yes || echo no): $OUT18"
fi
kill "$DECOY_PID" 2>/dev/null || true
DECOY_PID=""

# 14. no-python3 (#9214): with no python3 on PATH the launcher cannot tell a
# stale socket from a live one. It refuses and leaves the socket file alone.
# NOPY holds links to every other tool the launcher runs before that check.
DIR19="$TMP_ROOT/case19"
SOCK19="$DIR19/data/trusty-search.sock"
NOPY="$TMP_ROOT/nopy-bin"
mkdir -p "$DIR19/data" "$NOPY"
for tool in bash env id uname dscl getent cut sed tr ps mkdir rm; do
  t="$(command -v "$tool" || true)"
  [ -z "$t" ] || ln -s "$t" "$NOPY/$tool"
done
"$PYTHON" -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$SOCK19"
set +e
OUT19="$(env -i PATH="$NOPY" "$NOPY/bash" "$LAUNCHER" --bin "$STUB" --dir "$DIR19" 2>&1)"
S19=$?
set -e
if [ -n "$(env -i PATH="$NOPY" "$NOPY/bash" -c 'command -v python3' || true)" ]; then
  fail no-python3 "python3 is reachable on the restricted PATH $NOPY"
elif [ "$S19" -eq 1 ] && printf '%s' "$OUT19" | grep -qF "cannot tell whether $SOCK19 is live" \
    && [ -S "$SOCK19" ] && [ ! -e "$DIR19/home/stub-args" ]; then
  pass no-python3
else
  fail no-python3 "exit $S19, socket kept: $([ -S "$SOCK19" ] && echo yes || echo no): $OUT19"
fi

echo "sandbox_search_daemon selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
