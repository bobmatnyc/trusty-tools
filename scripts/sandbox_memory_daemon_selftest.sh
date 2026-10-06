#!/usr/bin/env bash
#
# sandbox_memory_daemon_selftest.sh — self-test for
# scripts/sandbox_memory_daemon.sh (issues #9121, #9161).
#
# Why: the launcher's whole job is what it does NOT pass, where it will NOT
#   run, and what it will NOT kill. A launcher that forwarded one inherited
#   credential, started under a `.env.local`, or pointed the daemon at the real
#   palace store would look exactly like a working one.
# What: most cases drive the launcher with a stub `--bin` (a shell script that
#   records its env names, argv and cwd) from a caller environment polluted
#   with fake secrets. Cases:
#     allowlist      dry run and real (stubbed) run show only HOME, PATH,
#                    TRUSTY_DATA_DIR_OVERRIDE, TRUSTY_SANDBOX and RUST_LOG; no
#                    fake secret value reaches the stub or the output
#     rust-log       RUST_LOG passes through when exported and is absent when not
#     argv           argv is `<dir>/bin/trusty-memory serve --foreground`, the
#                    data-dir override is <dir>/data, the cwd is <dir>/home
#     envlocal-red   a --dir under an ancestor `.env.local`, and a
#                    <dir>/home/.env.local, refuse with exit 1; the stub never runs
#     envlocal-green the same layout without the file starts the stub
#     stop-owned     `--stop DIR` ends a stubbed daemon started by the launcher,
#                    removes sandbox.pid, and the launcher exits
#     stop-decoy     `--stop DIR` refuses a recorded pid whose argv lacks DIR and
#                    leaves it alive
#     live           with a real trusty-memory binary: start, socket under
#                    <dir>/data/trusty-memory, no open file under the real data
#                    dir, the real socket's inode unchanged, `--stop`, pid dead.
#                    The binary is $SELFTEST_TM_BIN, else the first non-shim
#                    `trusty-memory` on PATH; with neither, the case says `skip`.
#
# Usage: ./scripts/sandbox_memory_daemon_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_memory_daemon.sh"
PASSED=0
FAILED=0
# /tmp keeps the live socket path under sun_path's limit.
TMP_ROOT="$(cd "$(mktemp -d /tmp/tmem-selftest.XXXXXX)" && pwd -P)"
DECOY_PID=""
LIVE_DIR=""
cleanup() {
  [ -z "$DECOY_PID" ] || kill "$DECOY_PID" 2>/dev/null || true
  if [ -n "$LIVE_DIR" ] && [ -f "$LIVE_DIR/sandbox.pid" ]; then
    bash "$LAUNCHER" --stop "$LIVE_DIR" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

FAKE_TOKEN="9121-selftest-fake-token-value"
FAKE_KEY="9121-selftest-fake-key-value"

pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }

# One-shot stub: records env names, argv[0], argv, cwd and the override value.
STUB="$TMP_ROOT/stub-tmem"
cat > "$STUB" <<'STUB_EOF'
#!/bin/sh
env | cut -d= -f1 | sort > "$HOME/stub-env-names"
printf '%s\n' "$0" > "$HOME/stub-argv0"
printf '%s\n' "$@" > "$HOME/stub-args"
pwd -P > "$HOME/stub-pwd"
printf '%s\n' "${TRUSTY_DATA_DIR_OVERRIDE:-}" > "$HOME/stub-override"
STUB_EOF
chmod +x "$STUB"

# Long-lived stub: stays up until signalled.
HOLD="$TMP_ROOT/stub-tmem-hold"
cat > "$HOLD" <<'STUB_EOF'
#!/bin/sh
while :; do sleep 1; done
STUB_EOF
chmod +x "$HOLD"

# The launcher, run from a cleared environment carrying fake secrets, RUST_LOG
# and a lookalike name. A 1 s socket wait keeps stubbed runs fast.
run_launcher() {
  env -i PATH="$PATH" RUST_LOG=warn SANDBOX_SOCKET_WAIT_SECS=1 \
    GITHUB_TOKEN="$FAKE_TOKEN" OPENROUTER_API_KEY="$FAKE_KEY" ANTHROPIC_API_KEY="$FAKE_KEY" \
    SLACK_BOT_TOKEN="$FAKE_TOKEN" TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" \
    SELFTEST_SECRET_KEY="$FAKE_KEY" bash "$LAUNCHER" "$@"
}

leaks() {
  case "$1" in
    *"$FAKE_TOKEN"*|*"$FAKE_KEY"*) return 0 ;;
    *) return 1 ;;
  esac
}

# 1. allowlist (dry run) + rust-log set.
DIR1="$TMP_ROOT/case1"
mkdir -p "$DIR1"
set +e
OUT1="$(run_launcher --bin "$STUB" --dir "$DIR1" --dry-run 2>&1)"
STATUS1=$?
set -e
GOT1="$(printf '%s\n' "$OUT1" | sed -n 's/^  \([A-Z_]*\)=.*/\1/p' | tr '\n' ' ')"
WANT1="HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX RUST_LOG "
if [ "$STATUS1" -ne 0 ]; then
  fail allowlist-dry-run "exit $STATUS1: $OUT1"
elif [ "$GOT1" != "$WANT1" ]; then
  fail allowlist-dry-run "env [$GOT1], want [$WANT1]"
elif leaks "$OUT1"; then
  fail allowlist-dry-run "a fake secret value reached the output"
elif [ -e "$DIR1/home" ] || [ -e "$DIR1/bin" ]; then
  fail allowlist-dry-run "a dry run created files in $DIR1"
elif ! printf '%s\n' "$OUT1" | grep -qx '  RUST_LOG=warn'; then
  fail rust-log-set "RUST_LOG=warn not passed"
else
  pass allowlist-dry-run
  pass rust-log-set
fi

# 2. rust-log unset: not exported, not passed.
set +e
OUT2="$(env -i PATH="$PATH" bash "$LAUNCHER" --bin "$STUB" --dir "$DIR1" --dry-run 2>&1)"
set -e
if printf '%s' "$OUT2" | grep -q 'RUST_LOG'; then
  fail rust-log-unset "RUST_LOG appeared without being exported"
else
  pass rust-log-unset
fi

# 3. allowlist (real run) + argv + data dir + cwd.
DIR3="$TMP_ROOT/case3"
mkdir -p "$DIR3"
set +e
OUT3="$(run_launcher --bin "$STUB" --dir "$DIR3" 2>&1)"
STATUS3=$?
set -e
if [ "$STATUS3" -ne 0 ] || [ ! -f "$DIR3/home/stub-env-names" ]; then
  fail allowlist-real-run "exit $STATUS3 or the stub never ran: $OUT3"
else
  GOT3="$(grep -vxE 'PWD|OLDPWD|SHLVL|_' "$DIR3/home/stub-env-names" | tr '\n' ' ')"
  WANT3="HOME PATH RUST_LOG TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX "
  if [ "$GOT3" != "$WANT3" ]; then
    fail allowlist-real-run "stub saw [$GOT3], want [$WANT3]"
  elif leaks "$OUT3"; then
    fail allowlist-real-run "a fake secret value reached the output"
  else
    pass allowlist-real-run
  fi
  ARGV0="$(cat "$DIR3/home/stub-argv0")"
  ARGS3="$(tr '\n' ' ' < "$DIR3/home/stub-args")"
  if [ "$ARGV0" = "$DIR3/bin/trusty-memory" ] && [ "$ARGS3" = "serve --foreground " ]; then
    pass argv
  else
    fail argv "argv0 [$ARGV0] args [$ARGS3]"
  fi
  OVR="$(cat "$DIR3/home/stub-override")"
  PWD3="$(cat "$DIR3/home/stub-pwd")"
  if [ "$OVR" = "$DIR3/data" ] && [ "$PWD3" = "$DIR3/home" ]; then
    pass data-dir-and-cwd
  else
    fail data-dir-and-cwd "override [$OVR] cwd [$PWD3]"
  fi
fi

# expect_envlocal_refusal <name> <dir>: exit 1, the .env.local message, no stub run.
expect_envlocal_refusal() {
  local name="$1" dir="$2" out status
  set +e
  out="$(run_launcher --bin "$STUB" --dir "$dir" 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq 1 ] && printf '%s' "$out" | grep -qF '.env.local would be loaded' \
    && [ ! -e "$dir/home/stub-env-names" ] && [ ! -e "$dir/sandbox.pid" ]; then
    pass "$name"
  else
    fail "$name" "exit $status, want 1 and no start: $out"
  fi
}

# 4. envlocal-red: an ancestor `.env.local`, then <dir>/home/.env.local.
PROJ="$TMP_ROOT/proj"
DIR4="$PROJ/sub/case4"
mkdir -p "$DIR4"
echo "GITHUB_TOKEN=$FAKE_TOKEN" > "$PROJ/.env.local"
expect_envlocal_refusal envlocal-red-ancestor "$DIR4"
DIR5="$TMP_ROOT/case5"
mkdir -p "$DIR5/home"
echo "GITHUB_TOKEN=$FAKE_TOKEN" > "$DIR5/home/.env.local"
expect_envlocal_refusal envlocal-red-home "$DIR5"

# 5. envlocal-green: the same layouts without the file start the stub.
rm -f "$PROJ/.env.local" "$DIR5/home/.env.local"
for d in "$DIR4" "$DIR5"; do
  set +e
  out="$(run_launcher --bin "$STUB" --dir "$d" 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq 0 ] && [ -f "$d/home/stub-env-names" ]; then
    pass "envlocal-green ${d#"$TMP_ROOT"/}"
  else
    fail "envlocal-green ${d#"$TMP_ROOT"/}" "exit $status or the stub never ran: $out"
  fi
done

# 6. stop-owned: a stubbed daemon started by the launcher ends under --stop.
DIR6="$TMP_ROOT/case6"
mkdir -p "$DIR6"
run_launcher --bin "$HOLD" --dir "$DIR6" > "$TMP_ROOT/case6.log" 2>&1 &
LAUNCHER6=$!
i=0
while [ "$i" -lt 50 ] && [ ! -s "$DIR6/sandbox.pid" ]; do sleep 0.1; i=$((i + 1)); done
CHILD6="$(cat "$DIR6/sandbox.pid" 2>/dev/null || true)"
set +e
OUT6="$(bash "$LAUNCHER" --stop "$DIR6" 2>&1)"
STATUS6=$?
set -e
i=0
while [ "$i" -lt 50 ] && kill -0 "$LAUNCHER6" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
if [ -z "$CHILD6" ]; then
  fail stop-owned "the launcher wrote no sandbox.pid: $(cat "$TMP_ROOT/case6.log")"
elif [ "$STATUS6" -ne 0 ] || kill -0 "$CHILD6" 2>/dev/null || [ -e "$DIR6/sandbox.pid" ] \
  || kill -0 "$LAUNCHER6" 2>/dev/null; then
  fail stop-owned "exit $STATUS6; child or launcher alive, or pidfile kept: $OUT6"
else
  pass stop-owned
fi
wait "$LAUNCHER6" 2>/dev/null || true

# 7. stop-decoy: a recorded pid whose argv lacks DIR is refused and survives.
DIR7="$TMP_ROOT/case7"
mkdir -p "$DIR7"
"$HOLD" serve --foreground &
DECOY_PID=$!
echo "$DECOY_PID" > "$DIR7/sandbox.pid"
set +e
OUT7="$(bash "$LAUNCHER" --stop "$DIR7" 2>&1)"
STATUS7=$?
set -e
if [ "$STATUS7" -eq 1 ] && kill -0 "$DECOY_PID" 2>/dev/null && [ -f "$DIR7/sandbox.pid" ]; then
  pass stop-decoy
else
  fail stop-decoy "exit $STATUS7; decoy alive? pidfile kept? $OUT7"
fi
kill "$DECOY_PID" 2>/dev/null || true
wait "$DECOY_PID" 2>/dev/null || true
DECOY_PID=""

# 8. live: a real trusty-memory, isolated from the real palace store.
LIVE_BIN="${SELFTEST_TM_BIN:-}"
if [ -z "$LIVE_BIN" ]; then
  while IFS= read -r cand; do
    case "$cand" in */shims/*) continue ;; esac
    LIVE_BIN="$cand"
    break
  done < <(type -ap trusty-memory || true)
fi
if [ -z "$LIVE_BIN" ]; then
  echo "skip live (no trusty-memory binary; set SELFTEST_TM_BIN)"
else
  echo "info live binary: $LIVE_BIN ($("$LIVE_BIN" --version 2>/dev/null || echo 'version unknown'))"
  case "$(uname -s)" in
    Darwin) REAL_DATA="$HOME/Library/Application Support/trusty-memory" ;;
    *) REAL_DATA="$HOME/.local/share/trusty-memory" ;;
  esac
  REAL_SOCK="$REAL_DATA/trusty-memory.sock"
  # GNU stat takes -c, BSD stat -f.
  inode() {
    if [ ! -e "$1" ]; then echo absent
    elif ! stat -c %i "$1" 2>/dev/null; then stat -f %i "$1"
    fi
  }
  SOCK_BEFORE="$(inode "$REAL_SOCK")"
  LIVE_DIR="$TMP_ROOT/live"
  mkdir -p "$LIVE_DIR"
  SOCK="$LIVE_DIR/data/trusty-memory/trusty-memory.sock"
  env -i PATH="$PATH" GITHUB_TOKEN="$FAKE_TOKEN" bash "$LAUNCHER" --bin "$LIVE_BIN" --dir "$LIVE_DIR" \
    > "$TMP_ROOT/live.log" 2>&1 &
  LAUNCHER8=$!
  i=0
  # Wait for the launcher's own report: it polls the socket every 0.2 s, so
  # the file can exist before the line does.
  while [ "$i" -lt 600 ] && ! grep -qF "listening on $SOCK" "$TMP_ROOT/live.log" \
    && kill -0 "$LAUNCHER8" 2>/dev/null; do
    sleep 0.1
    i=$((i + 1))
  done
  PID8="$(cat "$LIVE_DIR/sandbox.pid" 2>/dev/null || true)"
  if [ ! -S "$SOCK" ] || [ -z "$PID8" ]; then
    fail live-start "no socket at $SOCK or no pid: $(tail -20 "$TMP_ROOT/live.log")"
  elif ! grep -qF "listening on $SOCK" "$TMP_ROOT/live.log"; then
    fail live-start "the launcher never reported the socket"
  else
    pass live-start
    if command -v lsof >/dev/null 2>&1; then
      OPEN="$(lsof -p "$PID8" -Fn 2>/dev/null | sed -n 's/^n//p' || true)"
      if printf '%s\n' "$OPEN" | grep -qF "$REAL_DATA"; then
        fail live-real-data-untouched "the sandbox daemon holds a file under $REAL_DATA"
      elif ! printf '%s\n' "$OPEN" | grep -qF "$LIVE_DIR/data/trusty-memory/"; then
        fail live-real-data-untouched "lsof shows no file under the sandbox data dir: $OPEN"
      else
        pass live-real-data-untouched
      fi
    else
      echo "skip live-real-data-untouched lsof (no lsof)"
    fi
    if leaks "$(cat "$TMP_ROOT/live.log")"; then
      fail live-no-leak "a fake secret value reached the daemon log"
    else
      pass live-no-leak
    fi
  fi
  set +e
  OUT8="$(bash "$LAUNCHER" --stop "$LIVE_DIR" 2>&1)"
  STATUS8=$?
  set -e
  i=0
  while [ "$i" -lt 100 ] && kill -0 "$LAUNCHER8" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
  if [ "$STATUS8" -ne 0 ] || { [ -n "$PID8" ] && kill -0 "$PID8" 2>/dev/null; } \
    || [ -e "$LIVE_DIR/sandbox.pid" ] || kill -0 "$LAUNCHER8" 2>/dev/null; then
    fail live-stop "exit $STATUS8; daemon or launcher alive, or pidfile kept: $OUT8"
  else
    pass live-stop
  fi
  wait "$LAUNCHER8" 2>/dev/null || true
  SOCK_AFTER="$(inode "$REAL_SOCK")"
  if [ "$SOCK_BEFORE" = "$SOCK_AFTER" ]; then
    pass "live-real-socket-unchanged (inode $SOCK_AFTER)"
  else
    fail live-real-socket-unchanged "real socket inode $SOCK_BEFORE -> $SOCK_AFTER"
  fi
  LIVE_DIR=""
fi

echo "sandbox_memory_daemon_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
