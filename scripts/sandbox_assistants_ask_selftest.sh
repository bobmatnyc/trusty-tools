#!/usr/bin/env bash
#
# sandbox_assistants_ask_selftest.sh — self-test for
# scripts/sandbox_assistants_ask.sh (#11650 slice 3d).
#
# Why: the launcher's job is what it does NOT pass to `ta` and its daemon, and
#   proving the daemon is gone. A launcher that forwarded one credential, or
#   reported idle while a daemon still served, would look like a working one.
# What: runs the launcher with a stub `--bin` (a shell script that records its
#   env, argv and cwd and writes the daemon log lines `ta` writes; no real ta
#   starts) from a caller environment holding fake secrets. Cases:
#     allowlist  only the pinned and forwarded names reach the stub; the fake
#                GITHUB_TOKEN, OPENROUTER_API_KEY and SLACK_BOT_TOKEN never do
#     argv-cwd   exactly `ask <instance> <question>`, run from <dir>/home
#     role       `--role cto` seeds instructions.md from the cto template;
#                no `--role` and no instructions.md refuses
#     idle       a daemon that logged its exit passes with exit 0
#     not-idle   a live `ask-daemon <instance>` the log names, holding its
#                socket, makes the launcher exit 1 and TERM that pid
#     refusals   a --dir resolving to the real home, an ancestor `.env.local`,
#                and an instance id with a `/` each refuse
#     dry-run    prints env and argv and starts nothing
#
# Usage: ./scripts/sandbox_assistants_ask_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
LAUNCHER="$SCRIPT_DIR/sandbox_assistants_ask.sh"
WORK="$(mktemp -d /tmp/tasb.XXXXXX)"
WORK="$(cd "$WORK" && pwd -P)"
DAEMON=""
cleanup() {
  if [ -n "$DAEMON" ]; then kill "$DAEMON" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

FAILED=""
fail() {
  echo "FAIL: $1" >&2
  FAILED="$FAILED $1"
}

STUB="$WORK/ta"
cat > "$STUB" <<'STUB'
#!/usr/bin/env bash
# A stand-in for `ta`: `ask` records what it was given and writes the
# lifecycle lines of a daemon; `ask-daemon` just stays alive.
if [ "$1" = "ask-daemon" ]; then
  /bin/sleep 60
  exit 0
fi
data="$TRUSTY_DATA_DIR_OVERRIDE/trusty-agents"
/bin/mkdir -p "$data"
/usr/bin/env > "$HOME/../stub.env"
printf '%s\n' "$@" > "$HOME/../stub.argv"
/bin/pwd -P > "$HOME/../stub.cwd"
log="$data/trusty-agents-$2.log"
if [ "${STUB_MODE:-}" = "linger" ] || [ -f "$HOME/../linger" ]; then
  "$0" ask-daemon "$2" &
  printf 'ask daemon `%s`: serving %s as pid %s\n' "$2" "$data/trusty-agents-$2.sock" "$!" >> "$log"
  : > "$data/trusty-agents-$2.sock"
  printf '%s\n' "$!" > "$HOME/../daemon.pid"
else
  printf 'ask daemon `%s`: serving %s as pid %s\n' "$2" "$data/trusty-agents-$2.sock" "$$" >> "$log"
  printf 'ask daemon `%s`: pid %s exited (answered: 1)\n' "$2" "$$" >> "$log"
fi
echo "echoed: $3"
STUB
chmod +x "$STUB"

# run NAME ARGS...: launcher with polluted caller env; sets $RC, output in
# $WORK/NAME.out.
run() {
  local name="$1"
  shift
  RC=0
  # The optional forwarded names are unset so the allowlist is exact.
  env -u LANG -u LC_ALL -u RUST_LOG -u TMPDIR \
    GITHUB_TOKEN=ghp_fake OPENROUTER_API_KEY=sk-or-fake SLACK_BOT_TOKEN=xoxb-fake \
    TCODE_MOCK_LLM=echo SANDBOX_IDLE_WAIT_SECS=2 \
    "$LAUNCHER" --bin "$STUB" "$@" > "$WORK/$name.out" 2>&1 || RC=$?
}

# --- allowlist, argv-cwd, role, idle -------------------------------------
dir="$WORK/s1"
mkdir -p "$dir"
run s1 --dir "$dir" --role cto cto-assistant "what is on fire"
[ "$RC" -eq 0 ] || fail "idle: exit $RC: $(cat "$WORK/s1.out")"
grep -q 'echoed: what is on fire' "$WORK/s1.out" || fail "idle: answer not printed"
grep -q 'idle: no daemon serves cto-assistant' "$WORK/s1.out" || fail "idle: no idle line"
want="HOME PATH TCODE_MOCK_LLM TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MEMORY_SOCKET TRUSTY_SANDBOX "
# bash adds PWD, SHLVL and _ to its own environment before `env` runs.
names="$(sed 's/=.*//' "$dir/stub.env" | grep -vxE 'PWD|SHLVL|_|OLDPWD' | sort | tr '\n' ' ')"
[ "$names" = "$want" ] || fail "allowlist: stub saw [$names], want [$want]"
grep -qE 'ghp_fake|sk-or-fake|xoxb-fake' "$dir/stub.env" "$WORK/s1.out" && fail "allowlist: a fake secret leaked"
[ "$(tr '\n' ' ' < "$dir/stub.argv")" = "ask cto-assistant what is on fire " ] \
  || fail "argv-cwd: argv [$(tr '\n' ' ' < "$dir/stub.argv")]"
[ "$(cat "$dir/stub.cwd")" = "$dir/home" ] || fail "argv-cwd: cwd [$(cat "$dir/stub.cwd")]"
cmp -s "$dir/home/trusty-agents/cto-assistant/instructions.md" \
  "$SCRIPT_DIR/../crates/trusty-assistants/src/assistants/templates/cto.md" \
  || fail "role: instructions.md is not the cto template"

dir="$WORK/s2"
mkdir -p "$dir"
run s2 --dir "$dir" personal "hi"
[ "$RC" -eq 1 ] || fail "role: no --role and no instructions exit $RC, want 1"
[ ! -e "$dir/stub.argv" ] || fail "role: ta ran without instructions"

# --- not-idle -----------------------------------------------------------
dir="$WORK/s3"
mkdir -p "$dir"
touch "$dir/linger"
run s3 --dir "$dir" --role personal personal "hi"
DAEMON="$(cat "$dir/daemon.pid" 2>/dev/null || true)"
[ "$RC" -eq 1 ] || fail "not-idle: exit $RC, want 1"
grep -q 'did not exit idle' "$WORK/s3.out" || fail "not-idle: no failure line"
grep -q "sent TERM to pid $DAEMON" "$WORK/s3.out" || fail "not-idle: no TERM to the owned pid"
sleep 0.5
if [ -n "$DAEMON" ] && kill -0 "$DAEMON" 2>/dev/null; then fail "not-idle: pid $DAEMON still alive"; fi

# --- refusals -----------------------------------------------------------
REAL_HOME="$(cd ~ && pwd -P)"
run home --dir "$REAL_HOME" --dry-run personal hi
if [ "$RC" -ne 1 ] || ! grep -q 'real home' "$WORK/home.out"; then fail "refusals: real home not refused"; fi
mkdir -p "$WORK/envl/inner"
: > "$WORK/envl/.env.local"
run envl --dir "$WORK/envl/inner" --role personal personal hi
if [ "$RC" -ne 1 ] || ! grep -q '.env.local' "$WORK/envl.out"; then fail "refusals: ancestor .env.local not refused"; fi
[ ! -e "$WORK/envl/inner/stub.argv" ] || fail "refusals: ta ran under an ancestor .env.local"
run slash --dir "$WORK/s1" --dry-run "a/b" hi
[ "$RC" -eq 1 ] || fail "refusals: instance id with / exit $RC, want 1"

# --- dry-run ------------------------------------------------------------
dir="$WORK/s4"
mkdir -p "$dir"
run s4 --dir "$dir" --role personal --dry-run personal hi
[ "$RC" -eq 0 ] || fail "dry-run: exit $RC"
grep -q 'dry run; nothing started' "$WORK/s4.out" || fail "dry-run: no dry-run line"
[ ! -e "$dir/home" ] && [ ! -e "$dir/stub.argv" ] || fail "dry-run: created files or ran ta"

if [ -n "$FAILED" ]; then
  echo "sandbox_assistants_ask_selftest: FAILED:$FAILED" >&2
  exit 1
fi
echo "sandbox_assistants_ask_selftest: all cases passed"
