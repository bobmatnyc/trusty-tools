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
#     argv        --foreground, --no-auto-discover and a --data-dir under the
#                 sandbox dir are present
#     port        the default port is never 7878 and an explicit 7878 refuses
#     stop-owned  `--stop DIR` kills the recorded pid when its argv holds DIR
#     stop-decoy  `--stop DIR` refuses a recorded pid whose argv lacks DIR and
#                 leaves that process alive
#     stop-bad    a non-numeric or group-shaped pid refuses
#     signal      TERM to the launcher kills the child it spawned and removes
#                 sandbox.pid
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
STUB_EOF
chmod +x "$STUB"

# Long-lived stub: the same record, then it stays up until signalled.
HOLD="$TMP_ROOT/stub-ts-hold"
cat > "$HOLD" <<'STUB_EOF'
#!/bin/sh
env | cut -d= -f1 | sort > "$HOME/stub-env-names"
while :; do sleep 1; done
STUB_EOF
chmod +x "$HOLD"

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

# 1. allowlist (dry run): the printed env words are exactly the allowlist.
DIR1="$TMP_ROOT/case1"
mkdir -p "$DIR1"
set +e
OUT1="$(run_launcher --bin "$STUB" --dir "$DIR1" --dry-run 2>&1)"
STATUS1=$?
set -e
GOT1="$(printf '%s\n' "$OUT1" | sed -n 's/^  \([A-Z_]*\)=.*/\1/p' | tr '\n' ' ')"
WANT1="HOME PATH TRUSTY_DATA_DIR TRUSTY_REDB_CACHE_MB TRUSTY_EMBED_INFLIGHT RUST_LOG "
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

# 2. allowlist (real run) + argv + port: a stubbed start.
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
  WANT2="HOME PATH RUST_LOG TRUSTY_DATA_DIR TRUSTY_EMBED_INFLIGHT TRUSTY_REDB_CACHE_MB "
  if [ "$GOT2" != "$WANT2" ]; then
    fail allowlist "stub saw [$GOT2], want [$WANT2]"
  elif leaks "$OUT2"; then
    fail allowlist "a fake secret value reached the output"
  else
    pass allowlist-real-run
  fi
  ARGS2="$(tr '\n' ' ' < "$DIR2/home/stub-args" 2>/dev/null || true)"
  case "$ARGS2" in
    "start --foreground --no-auto-discover --data-dir $DIR2/data --port "*) pass argv ;;
    *) fail argv "stub argv was [$ARGS2]" ;;
  esac
  PORT2="${ARGS2##*--port }"
  PORT2="${PORT2%% *}"
  if [ -z "$PORT2" ] || [ "$PORT2" = "7878" ]; then
    fail port "default port was [$PORT2]"
  else
    pass port-default
  fi
fi

# 3. port: an explicit 7878 refuses.
set +e
OUT3="$(run_launcher --bin "$STUB" --dir "$DIR1" --port 7878 --dry-run 2>&1)"
STATUS3=$?
set -e
if [ "$STATUS3" -eq 1 ] && printf '%s' "$OUT3" | grep -qF "live daemon's port"; then
  pass port-7878-refused
else
  fail port "explicit 7878: exit $STATUS3"
fi

# 4. stop-owned: --stop kills the recorded pid when its argv holds DIR.
DIR4="$TMP_ROOT/case4"
mkdir -p "$DIR4"
cp "$HOLD" "$DIR4/holder"
"$DIR4/holder" &
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

echo "sandbox_search_daemon selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
